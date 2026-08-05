use crate::config::{
    self, load_app_config, open_url, save_bridge_config, BridgeConfig, HealthExpectConfig,
    LifecycleConfig, RestartPolicy, ServiceMode, StartupPolicy, TunnelMode,
};
use crate::health;
use crate::paths::{machine_id, AppPaths};
use crate::peer;
use crate::process;
use crate::registry::{
    validate_no_port_conflicts, Registry, RegistryEntry, RegistryExport, ServiceExport,
};
use crate::state::{DesiredState, State};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::thread;
use std::time::Duration;
use url::Url;

#[derive(Clone)]
pub struct BridgeEnv {
    pub paths: AppPaths,
    pub app: config::AppConfig,
    pub machine_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PortRow {
    pub port: u16,
    pub id: String,
    pub title: String,
    pub owner_host: String,
    pub source_machine: String,
    pub local_machine_id: String,
    pub service_mode: String,
    pub tunnel_modes: String,
    pub startup_policy: String,
    pub restart_policy: String,
    pub desired_state: String,
    pub runtime_status: String,
    pub url: String,
    pub direct_open: bool,
    pub local_port: Option<u16>,
    pub network_url: Option<String>,
    pub pid_source: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareOpenResult {
    pub target: String,
    pub service_ref: PrepareOpenServiceRef,
    pub source_config_path: String,
    pub title: String,
    pub url: String,
    pub origin: Option<String>,
    pub local_machine_id: String,
    pub service_mode: String,
    pub tunnel_modes: String,
    pub startup_policy: String,
    pub restart_policy: String,
    pub runtime_status: String,
    pub direct_open: bool,
    pub local_port: Option<u16>,
    pub network_url: Option<String>,
    pub actions: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareOpenServiceRef {
    pub id: String,
    pub owner_host: String,
    pub source_machine: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserveEnvelope {
    pub schema: String,
    pub local_machine_id: String,
    pub observed_at: String,
    pub timeout_sec: u64,
    pub include_peers: bool,
    pub rows: Vec<ObserveRow>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserveRow {
    pub service_ref: PrepareOpenServiceRef,
    pub title: String,
    pub local_machine_id: String,
    pub url: String,
    pub local_port: Option<u16>,
    pub network_url: Option<String>,
    pub service_mode: String,
    pub tunnel_modes: String,
    pub startup_policy: String,
    pub restart_policy: String,
    pub runtime_status_hint: String,
    pub observed_from: String,
    pub observation: ObservationResult,
    pub safe_open_actions: Vec<String>,
    pub safe_lifecycle_actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationResult {
    pub observed_at: String,
    pub status: String,
    pub reason: String,
    pub detail: Option<String>,
    pub http_status: Option<String>,
    pub observed_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManagedRuntimeSpec {
    pub schema: String,
    pub service_ref: PrepareOpenServiceRef,
    pub source_config_path: String,
    pub title: String,
    pub local_machine_id: String,
    pub desired_state: String,
    pub runtime_status: String,
    pub startup_policy: String,
    pub restart_policy: String,
    pub cwd: String,
    pub command: Vec<String>,
    pub pid_file: String,
    pub log_file: String,
    pub health_url: Option<String>,
    pub health_expect_body_contains: Vec<String>,
    pub startup_timeout_sec: u64,
    pub local_url: Option<String>,
    pub open_url: String,
    pub network_url: Option<String>,
    pub tunnel: ManagedRuntimeTunnelSpec,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManagedRuntimeTunnelSpec {
    pub modes: Vec<String>,
    pub bind_host: String,
}

impl BridgeEnv {
    pub fn discover() -> Result<Self> {
        let paths = AppPaths::discover()?;
        Self::from_paths(paths)
    }

    pub fn from_paths(paths: AppPaths) -> Result<Self> {
        let app = load_app_config(&paths.config_file)?;
        let machine_id = machine_id(app.machine_id.as_deref());
        Ok(Self {
            paths,
            app,
            machine_id,
        })
    }
}

pub fn status_rows(
    env: &BridgeEnv,
    id: Option<&str>,
    include_peers: bool,
) -> Result<Vec<crate::status::StatusRow>> {
    let registry = Registry::load(&env.paths.registry_file)?;
    let state = State::load(&env.paths.state_file)?;
    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();
    for (_, cfg) in registry.load_configs()? {
        if id.map(|wanted| wanted != cfg.id).unwrap_or(false) {
            continue;
        }
        seen.insert((cfg.owner_host.clone(), cfg.port, cfg.id.clone()));
        rows.push(crate::status::row_for(&cfg, &env.machine_id, &state));
    }
    if include_peers {
        let peer_results = peer::fetch_peer_exports(&env.app);
        peer::print_peer_warnings(&peer_results);
        for (_, result) in peer_results {
            let Ok(export) = result else {
                continue;
            };
            for service in export.services {
                if id.map(|wanted| wanted != service.id).unwrap_or(false) {
                    continue;
                }
                if !seen.insert((service.owner_host.clone(), service.port, service.id.clone())) {
                    continue;
                }
                rows.push(crate::status::row_for_export(
                    &service,
                    &env.machine_id,
                    &state,
                ));
            }
        }
    }
    Ok(rows)
}

pub fn port_rows(env: &BridgeEnv, include_peers: bool) -> Result<Vec<PortRow>> {
    port_rows_with_runtime(env, include_peers, true)
}

pub fn port_rows_with_runtime(
    env: &BridgeEnv,
    include_peers: bool,
    include_runtime: bool,
) -> Result<Vec<PortRow>> {
    let registry = Registry::load(&env.paths.registry_file)?;
    let state = State::load(&env.paths.state_file)?;
    let mut exports = vec![registry.export_with_runtime(&env.machine_id, include_runtime)?];
    if include_peers {
        let peer_results = peer::fetch_peer_exports(&env.app);
        peer::print_peer_warnings(&peer_results);
        exports.extend(
            peer_results
                .into_iter()
                .filter_map(|(_, result)| result.ok()),
        );
    }
    validate_no_port_conflicts(&exports)?;
    Ok(port_rows_from_exports(exports, env, &state))
}

pub fn observe_services(
    env: &BridgeEnv,
    id: Option<&str>,
    include_peers: bool,
    timeout: Duration,
) -> Result<ObserveEnvelope> {
    let timeout = timeout.max(Duration::from_secs(1));
    let rows: Vec<PortRow> = port_rows_with_runtime(env, include_peers, false)?
        .into_iter()
        .filter(|row| id.map(|wanted| wanted == row.id).unwrap_or(true))
        .collect();
    let local_configs = local_config_map(env)?;
    let peer_observations = fetch_peer_observation_map(env, &rows, include_peers, timeout);
    let mut warnings = Vec::new();
    let mut out_rows = Vec::new();
    for row in rows {
        let observed_from = observation_host(env, &row, include_peers);
        let observation = if observed_from == env.machine_id {
            observe_local_port_row(&row, local_configs.get(&row.id), timeout)
        } else {
            match peer_observations.get(&observed_from) {
                Some(Ok(peer_rows)) => peer_rows
                    .get(&observe_lookup_key(&row))
                    .or_else(|| peer_rows.get(&observe_lookup_key_without_local_port(&row)))
                    .cloned()
                    .unwrap_or_else(|| {
                        ObservationResult::unknown(
                            "not-observed",
                            Some(format!(
                                "peer `{observed_from}` returned no observation for {} owned by {} on port {}",
                                row.id, row.owner_host, row.port
                            )),
                            None,
                        )
                    }),
                Some(Err(err)) => ObservationResult::unknown(
                    "peer-observe-failed",
                    Some(err.clone()),
                    None,
                ),
                None => ObservationResult::unknown(
                    "not-observed",
                    Some(format!("no observation host selected for `{}`", row.id)),
                    None,
                ),
            }
        };
        if let Some(Err(err)) = peer_observations.get(&observed_from) {
            warnings.push(format!("peer {observed_from}: {err}"));
        }
        out_rows.push(observe_row_from_port(row, observed_from, observation));
    }
    warnings.sort();
    warnings.dedup();
    Ok(ObserveEnvelope {
        schema: "bridgeboard.observe.v1".into(),
        local_machine_id: env.machine_id.clone(),
        observed_at: crate::time::now_iso(),
        timeout_sec: timeout.as_secs(),
        include_peers,
        rows: out_rows,
        warnings,
    })
}

pub fn managed_runtime_specs(env: &BridgeEnv, id: Option<&str>) -> Result<Vec<ManagedRuntimeSpec>> {
    let registry = Registry::load(&env.paths.registry_file)?;
    let state = State::load(&env.paths.state_file)?;
    let mut matched_id = false;
    let mut rows = Vec::new();
    for (entry, cfg) in registry.load_configs()? {
        if id.map(|wanted| wanted != cfg.id).unwrap_or(false) {
            continue;
        }
        matched_id = true;
        if cfg.owner_host != env.machine_id || cfg.service.mode != ServiceMode::Managed {
            continue;
        }
        let cwd = cfg
            .service
            .cwd
            .as_ref()
            .context("managed service cwd is required")?;
        let pid_file =
            config::service_pid_path(&cfg).context("managed service pid_file is required")?;
        let log_file =
            config::service_log_path(&cfg).context("managed service log_file is required")?;
        let state_entry = state.services.get(&cfg.id);
        rows.push(ManagedRuntimeSpec {
            schema: "bridgeboard.runtime-spec.v1".into(),
            service_ref: PrepareOpenServiceRef {
                id: cfg.id.clone(),
                owner_host: cfg.owner_host.clone(),
                source_machine: env.machine_id.clone(),
                port: cfg.port,
            },
            source_config_path: entry.config_path.display().to_string(),
            title: cfg.title.clone(),
            local_machine_id: env.machine_id.clone(),
            desired_state: desired_label(state_entry.and_then(|entry| entry.desired)),
            runtime_status: process::managed_service_status(&cfg),
            startup_policy: startup_policy_label(cfg.service.lifecycle.startup).into(),
            restart_policy: restart_policy_label(cfg.service.lifecycle.restart).into(),
            cwd: cwd.display().to_string(),
            command: cfg.service.command.clone(),
            pid_file: pid_file.display().to_string(),
            log_file: log_file.display().to_string(),
            health_url: cfg.service.health_url.clone(),
            health_expect_body_contains: cfg.service.health_expect.body_contains.clone(),
            startup_timeout_sec: cfg.service.startup_timeout_sec,
            local_url: cfg.local_url.clone(),
            open_url: open_url(&cfg),
            network_url: cfg.network_url.clone(),
            tunnel: ManagedRuntimeTunnelSpec {
                modes: cfg
                    .tunnel
                    .modes
                    .iter()
                    .map(|mode| tunnel_mode_value(*mode).to_string())
                    .collect(),
                bind_host: cfg.tunnel.bind_host.clone(),
            },
        });
    }
    if id.is_some() && !matched_id {
        bail!(
            "service `{}` is not registered locally",
            id.unwrap_or_default()
        );
    }
    if id.is_some() && rows.is_empty() {
        bail!(
            "service `{}` is not a local managed service on `{}`",
            id.unwrap_or_default(),
            env.machine_id
        );
    }
    rows.sort_by(|a, b| {
        a.service_ref
            .port
            .cmp(&b.service_ref.port)
            .then_with(|| a.service_ref.id.cmp(&b.service_ref.id))
    });
    Ok(rows)
}

pub(crate) fn port_rows_from_exports(
    exports: Vec<RegistryExport>,
    env: &BridgeEnv,
    state: &State,
) -> Vec<PortRow> {
    let local_machine_id = env.machine_id.as_str();
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    for export in exports {
        for service in export.services {
            if !seen.insert((service.owner_host.clone(), service.port, service.id.clone())) {
                continue;
            }
            let desired = if export.machine_id == local_machine_id {
                desired_label(
                    state
                        .services
                        .get(&service.id)
                        .and_then(|entry| entry.desired),
                )
            } else {
                "-".into()
            };
            let is_remote_record =
                export.machine_id != local_machine_id || service.owner_host != local_machine_id;
            let local_forward = if is_remote_record {
                local_forward_allowed(env, &service.tunnel_modes)
            } else {
                service.tunnel_modes.contains(&TunnelMode::LocalForward)
            };
            let tunnel_owner = if env.app.peers.contains_key(&service.owner_host) {
                service.owner_host.as_str()
            } else {
                export.machine_id.as_str()
            };
            let active_local_port = if is_remote_record && local_forward {
                let prefix = format!("{}:", service.id);
                state
                    .tunnels
                    .iter()
                    .filter(|(key, tunnel)| {
                        key.starts_with(&prefix)
                            && tunnel.peer == tunnel_owner
                            && tunnel.pid.map(process::pid_alive).unwrap_or(false)
                    })
                    .find_map(|(_, tunnel)| Some(tunnel.local_port))
            } else {
                None
            };
            let action_local_port = active_local_port.or_else(|| {
                if is_remote_record && local_forward && local_port_needs_peer_fallback(service.port)
                {
                    peer_fallback_local_port(service.port)
                } else {
                    None
                }
            });
            let local_tunnel_active = active_local_port.is_some();
            let direct_open = !is_remote_record || !local_forward || local_tunnel_active;
            let url = if !is_remote_record {
                service
                    .open_url
                    .clone()
                    .unwrap_or_else(|| format!("http://127.0.0.1:{}/", service.port))
            } else if let Some(local_port) = action_local_port {
                format!("http://127.0.0.1:{local_port}/")
            } else if local_forward {
                service
                    .local_url
                    .clone()
                    .unwrap_or_else(|| format!("http://127.0.0.1:{}/", service.port))
            } else {
                service
                    .network_url
                    .clone()
                    .or_else(|| service.open_url.clone())
                    .or_else(|| service.local_url.clone())
                    .unwrap_or_else(|| format!("http://127.0.0.1:{}/", service.port))
            };
            rows.push(PortRow {
                port: service.port,
                id: service.id,
                title: service.title,
                owner_host: service.owner_host,
                source_machine: export.machine_id.clone(),
                local_machine_id: local_machine_id.to_string(),
                service_mode: service_mode_label(service.service_mode).into(),
                tunnel_modes: tunnel_modes_label_for_peer(
                    env,
                    is_remote_record,
                    &service.tunnel_modes,
                ),
                startup_policy: startup_policy_label(service.lifecycle.startup).into(),
                restart_policy: restart_policy_label(service.lifecycle.restart).into(),
                desired_state: desired,
                runtime_status: service.runtime_status.unwrap_or_else(|| {
                    if export.machine_id == local_machine_id {
                        "not-checked".into()
                    } else {
                        "remote-record".into()
                    }
                }),
                url,
                direct_open,
                local_port: action_local_port,
                network_url: service.network_url,
                pid_source: service.pid_source,
                notes: service.notes,
            });
        }
    }
    rows.sort_by(|a, b| a.port.cmp(&b.port).then_with(|| a.id.cmp(&b.id)));
    rows
}

impl ObservationResult {
    fn healthy(status: String, observed_url: Option<String>) -> Self {
        Self {
            observed_at: crate::time::now_iso(),
            status: "healthy".into(),
            reason: "http-ok".into(),
            detail: Some(status.clone()),
            http_status: Some(status),
            observed_url,
        }
    }

    fn unhealthy(reason: &str, detail: String, http_status: Option<String>, url: String) -> Self {
        Self {
            observed_at: crate::time::now_iso(),
            status: "unhealthy".into(),
            reason: reason.into(),
            detail: Some(bounded_detail(&detail)),
            http_status,
            observed_url: Some(url),
        }
    }

    fn unreachable(reason: &str, detail: String, url: String) -> Self {
        Self {
            observed_at: crate::time::now_iso(),
            status: "unreachable".into(),
            reason: reason.into(),
            detail: Some(bounded_detail(&detail)),
            http_status: None,
            observed_url: Some(url),
        }
    }

    fn unknown(reason: &str, detail: Option<String>, observed_url: Option<String>) -> Self {
        Self {
            observed_at: crate::time::now_iso(),
            status: "unknown".into(),
            reason: reason.into(),
            detail: detail.map(|value| bounded_detail(&value)),
            http_status: None,
            observed_url,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ObservationLookupKey {
    id: String,
    owner_host: String,
    source_machine: String,
    port: u16,
    local_port: Option<u16>,
}

fn observe_lookup_key(row: &PortRow) -> ObservationLookupKey {
    ObservationLookupKey {
        id: row.id.clone(),
        owner_host: row.owner_host.clone(),
        source_machine: row.source_machine.clone(),
        port: row.port,
        local_port: row.local_port,
    }
}

fn observe_row_lookup_key(row: &ObserveRow) -> ObservationLookupKey {
    ObservationLookupKey {
        id: row.service_ref.id.clone(),
        owner_host: row.service_ref.owner_host.clone(),
        source_machine: row.service_ref.source_machine.clone(),
        port: row.service_ref.port,
        local_port: row.local_port,
    }
}

fn observe_lookup_key_without_local_port(row: &PortRow) -> ObservationLookupKey {
    let mut key = observe_lookup_key(row);
    key.local_port = None;
    key
}

fn local_config_map(env: &BridgeEnv) -> Result<BTreeMap<String, BridgeConfig>> {
    let registry = Registry::load(&env.paths.registry_file)?;
    let mut configs = BTreeMap::new();
    for (_, cfg) in registry.load_configs()? {
        configs.insert(cfg.id.clone(), cfg);
    }
    Ok(configs)
}

fn observation_host(env: &BridgeEnv, row: &PortRow, include_peers: bool) -> String {
    if include_peers
        && row.owner_host != env.machine_id
        && env.app.peers.contains_key(&row.owner_host)
    {
        return row.owner_host.clone();
    }
    if include_peers
        && row.source_machine != env.machine_id
        && env.app.peers.contains_key(&row.source_machine)
    {
        return row.source_machine.clone();
    }
    env.machine_id.clone()
}

fn fetch_peer_observation_map(
    env: &BridgeEnv,
    rows: &[PortRow],
    include_peers: bool,
    timeout: Duration,
) -> BTreeMap<String, Result<BTreeMap<ObservationLookupKey, ObservationResult>, String>> {
    if !include_peers {
        return BTreeMap::new();
    }
    let mut hosts = BTreeSet::new();
    for row in rows {
        let host = observation_host(env, row, true);
        if host != env.machine_id {
            hosts.insert(host);
        }
    }
    let mut handles = Vec::new();
    for host in hosts {
        let env = env.clone();
        let handle = thread::spawn(move || {
            let result = fetch_peer_observations(&env, &host, timeout).map(|envelope| {
                envelope
                    .rows
                    .into_iter()
                    .map(|row| (observe_row_lookup_key(&row), row.observation))
                    .collect()
            });
            (host, result)
        });
        handles.push(handle);
    }
    let mut map = BTreeMap::new();
    for handle in handles {
        let (host, result) = handle.join().unwrap_or_else(|_| {
            (
                "unknown".into(),
                Err("peer observation thread panicked".into()),
            )
        });
        map.insert(host, result);
    }
    map
}

fn fetch_peer_observations(
    env: &BridgeEnv,
    peer_name: &str,
    timeout: Duration,
) -> Result<ObserveEnvelope, String> {
    let timeout_arg = timeout.as_secs().max(1).to_string();
    let args = ["observe", "--json", "--timeout-sec", timeout_arg.as_str()];
    let output = peer::run_bridgeboard_command(&env.app, peer_name, &args, timeout)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(if detail.is_empty() {
            format!("peer observe exited with {}", output.status)
        } else {
            detail
        });
    }
    let envelope: ObserveEnvelope =
        serde_json::from_slice(&output.stdout).map_err(|err| err.to_string())?;
    if envelope.schema != "bridgeboard.observe.v1" {
        return Err(format!("unsupported observe schema `{}`", envelope.schema));
    }
    Ok(envelope)
}

fn observe_local_port_row(
    row: &PortRow,
    cfg: Option<&BridgeConfig>,
    timeout: Duration,
) -> ObservationResult {
    let (url, expect) = observation_url_and_expect(row, cfg);
    let Some(url) = url else {
        return ObservationResult::unknown("missing-url", None, None);
    };
    observe_http_url(&url, timeout, &expect)
}

fn observation_url_and_expect(
    row: &PortRow,
    cfg: Option<&BridgeConfig>,
) -> (Option<String>, HealthExpectConfig) {
    if let Some(cfg) = cfg {
        let url = cfg
            .service
            .health_url
            .clone()
            .or_else(|| cfg.open_url.clone())
            .or_else(|| cfg.local_url.clone())
            .or_else(|| Some(format!("http://127.0.0.1:{}/", cfg.port)));
        return (url, cfg.service.health_expect.clone());
    }
    (
        Some(row.url.clone()).filter(|url| !url.trim().is_empty()),
        HealthExpectConfig::default(),
    )
}

fn observe_http_url(
    url: &str,
    timeout: Duration,
    expect: &HealthExpectConfig,
) -> ObservationResult {
    match health::check_http_with_expect(url, timeout, expect) {
        Ok(status) => ObservationResult::healthy(status, Some(url.to_string())),
        Err(err) => classify_observation_error(url, err),
    }
}

fn classify_observation_error(url: &str, err: anyhow::Error) -> ObservationResult {
    let detail = err.to_string();
    let lower = detail.to_lowercase();
    if let Some(status) = detail
        .split_once("health check failed: ")
        .map(|(_, status)| status.to_string())
    {
        return ObservationResult::unhealthy("http-status", detail, Some(status), url.to_string());
    }
    if lower.contains("health body expectation failed") {
        return ObservationResult::unhealthy("body-mismatch", detail, None, url.to_string());
    }
    if lower.contains("only http") {
        return ObservationResult::unknown("unsupported-url", Some(detail), Some(url.to_string()));
    }
    if lower.contains("parse health_url") || lower.contains("missing host") {
        return ObservationResult::unknown("invalid-url", Some(detail), Some(url.to_string()));
    }
    if lower.contains("timed out") || lower.contains("timeout") {
        return ObservationResult::unreachable("timeout", detail, url.to_string());
    }
    if lower.contains("connection refused") {
        return ObservationResult::unreachable("connection-refused", detail, url.to_string());
    }
    if lower.contains("resolve") || lower.contains("lookup address") {
        return ObservationResult::unreachable("resolve-failed", detail, url.to_string());
    }
    ObservationResult::unreachable("connect-failed", detail, url.to_string())
}

fn observe_row_from_port(
    row: PortRow,
    observed_from: String,
    observation: ObservationResult,
) -> ObserveRow {
    ObserveRow {
        service_ref: PrepareOpenServiceRef {
            id: row.id.clone(),
            owner_host: row.owner_host.clone(),
            source_machine: row.source_machine.clone(),
            port: row.port,
        },
        title: row.title.clone(),
        local_machine_id: row.local_machine_id.clone(),
        url: row.url.clone(),
        local_port: row.local_port,
        network_url: row.network_url.clone(),
        service_mode: row.service_mode.clone(),
        tunnel_modes: row.tunnel_modes.clone(),
        startup_policy: row.startup_policy.clone(),
        restart_policy: row.restart_policy.clone(),
        runtime_status_hint: row.runtime_status.clone(),
        observed_from,
        safe_open_actions: vec!["prepare-open".to_string()],
        safe_lifecycle_actions: safe_lifecycle_actions_for_row(&row),
        observation,
    }
}

fn safe_lifecycle_actions_for_row(row: &PortRow) -> Vec<String> {
    if row.service_mode == "managed" {
        if row.owner_host == row.local_machine_id && row.source_machine == row.local_machine_id {
            return ["up", "down", "restart"]
                .into_iter()
                .map(str::to_string)
                .collect();
        } else {
            return ["remote-up", "remote-down", "remote-restart"]
                .into_iter()
                .map(str::to_string)
                .collect();
        }
    }
    Vec::new()
}

fn bounded_detail(value: &str) -> String {
    const LIMIT: usize = 240;
    let trimmed = value.trim();
    if trimmed.chars().count() <= LIMIT {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(LIMIT.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

fn local_port_needs_peer_fallback(port: u16) -> bool {
    process::tcp_port_open(port)
}

fn peer_fallback_local_port(port: u16) -> Option<u16> {
    if let Some(preferred) = preferred_peer_fallback_port(port) {
        if !process::tcp_port_open(preferred) {
            return Some(preferred);
        }
    }
    (24700..=24899).find(|candidate| !process::tcp_port_open(*candidate))
}

fn preferred_peer_fallback_port(port: u16) -> Option<u16> {
    let preferred = port.saturating_add(400);
    if (24001..=24999).contains(&preferred) {
        return Some(preferred);
    }
    None
}

pub fn up(env: &BridgeEnv, id: &str) -> Result<Vec<String>> {
    up_inner(env, id, true, true)
}

pub fn up_from_peer(
    env: &BridgeEnv,
    peer_name: &str,
    id: &str,
    local_port: Option<u16>,
) -> Result<Vec<String>> {
    up_from_peer_target(env, peer_name, id, None, None, local_port)
}

pub fn up_from_peer_target(
    env: &BridgeEnv,
    peer_name: &str,
    id: &str,
    owner_host: Option<&str>,
    port: Option<u16>,
    local_port: Option<u16>,
) -> Result<Vec<String>> {
    let service = fetch_peer_service(env, peer_name, id, owner_host, port)?;
    let owner = peer_service_owner(env, peer_name, &service);
    let mut messages = run_remote_up(env, owner, id)?;
    let mut state = State::load(&env.paths.state_file)?;
    start_peer_service_tunnel_or_reuse_reverse(
        env,
        &mut state,
        peer_name,
        &service,
        local_port,
        &mut messages,
    )?;
    state.save(&env.paths.state_file)?;
    Ok(messages)
}

pub fn remote_up_target(
    env: &BridgeEnv,
    id: &str,
    owner_host: Option<&str>,
    source_machine: Option<&str>,
    port: Option<u16>,
    local_port: Option<u16>,
) -> Result<Vec<String>> {
    let Some((peer_name, service)) =
        find_peer_service_target(env, id, owner_host, source_machine, port)?
    else {
        bail!("service `{id}` was not found on the requested peer target");
    };
    let owner = peer_service_owner(env, &peer_name, &service);
    let mut messages = run_remote_up(env, owner, id)?;
    let mut state = State::load(&env.paths.state_file)?;
    start_peer_service_tunnel_or_reuse_reverse(
        env,
        &mut state,
        &peer_name,
        &service,
        local_port,
        &mut messages,
    )?;
    state.save(&env.paths.state_file)?;
    Ok(messages)
}

pub fn remote_up(env: &BridgeEnv, id: &str) -> Result<Vec<String>> {
    let registry = Registry::load(&env.paths.registry_file)?;
    if let Some((_, cfg)) = registry.try_get_entry_config(id)? {
        if cfg.owner_host == env.machine_id {
            return up(env, id);
        }
        let mut messages = run_remote_up(env, &cfg.owner_host, id)?;
        let mut state = State::load(&env.paths.state_file)?;
        start_config_tunnel_or_reuse_reverse(env, &mut state, &cfg, &mut messages)?;
        state.save(&env.paths.state_file)?;
        return Ok(messages);
    }

    let Some((peer_name, service)) = find_peer_service(env, id)? else {
        bail!("service `{id}` is not registered locally and was not found on configured peers");
    };
    let owner = if env.app.peers.contains_key(&service.owner_host) {
        service.owner_host.as_str()
    } else {
        peer_name.as_str()
    };
    let mut messages = run_remote_up(env, owner, id)?;
    let mut state = State::load(&env.paths.state_file)?;
    start_peer_service_tunnel_or_reuse_reverse(
        env,
        &mut state,
        &peer_name,
        &service,
        None,
        &mut messages,
    )?;
    state.save(&env.paths.state_file)?;
    Ok(messages)
}

pub fn remote_down(env: &BridgeEnv, id: &str) -> Result<Vec<String>> {
    let registry = Registry::load(&env.paths.registry_file)?;
    if let Some((_, cfg)) = registry.try_get_entry_config(id)? {
        if cfg.owner_host == env.machine_id {
            return down(env, id);
        }
        let mut messages = run_remote_down(env, &cfg.owner_host, id)?;
        stop_local_tunnels(env, id, &mut messages)?;
        return Ok(messages);
    }

    let Some((peer_name, service)) = find_peer_service(env, id)? else {
        bail!("service `{id}` is not registered locally and was not found on configured peers");
    };
    let owner = if env.app.peers.contains_key(&service.owner_host) {
        service.owner_host.as_str()
    } else {
        peer_name.as_str()
    };
    let mut messages = run_remote_down(env, owner, id)?;
    stop_local_tunnels(env, id, &mut messages)?;
    Ok(messages)
}

pub fn remote_restart(env: &BridgeEnv, id: &str) -> Result<Vec<String>> {
    let registry = Registry::load(&env.paths.registry_file)?;
    if let Some((_, cfg)) = registry.try_get_entry_config(id)? {
        if cfg.owner_host == env.machine_id {
            return restart(env, id);
        }
        let mut messages = run_remote_restart(env, &cfg.owner_host, id)?;
        let mut state = State::load(&env.paths.state_file)?;
        start_config_tunnel_or_reuse_reverse(env, &mut state, &cfg, &mut messages)?;
        state.save(&env.paths.state_file)?;
        return Ok(messages);
    }

    let Some((peer_name, service)) = find_peer_service(env, id)? else {
        bail!("service `{id}` is not registered locally and was not found on configured peers");
    };
    let owner = if env.app.peers.contains_key(&service.owner_host) {
        service.owner_host.as_str()
    } else {
        peer_name.as_str()
    };
    let mut messages = run_remote_restart(env, owner, id)?;
    let mut state = State::load(&env.paths.state_file)?;
    start_peer_service_tunnel_or_reuse_reverse(
        env,
        &mut state,
        &peer_name,
        &service,
        None,
        &mut messages,
    )?;
    state.save(&env.paths.state_file)?;
    Ok(messages)
}

pub fn remote_down_target(
    env: &BridgeEnv,
    id: &str,
    owner_host: Option<&str>,
    source_machine: Option<&str>,
    port: Option<u16>,
) -> Result<Vec<String>> {
    let Some((peer_name, service)) =
        find_peer_service_target(env, id, owner_host, source_machine, port)?
    else {
        bail!("service `{id}` was not found on the requested peer target");
    };
    let owner = peer_service_owner(env, &peer_name, &service).to_string();
    let mut messages = run_remote_down(env, &owner, id)?;
    let mut state = State::load(&env.paths.state_file)?;
    process::stop_tunnels_for_peer(id, &owner, &mut state)?;
    state.save(&env.paths.state_file)?;
    messages.push(format!("stopped local tunnels for {id} via {owner}"));
    Ok(messages)
}

pub fn remote_restart_target(
    env: &BridgeEnv,
    id: &str,
    owner_host: Option<&str>,
    source_machine: Option<&str>,
    port: Option<u16>,
    local_port: Option<u16>,
) -> Result<Vec<String>> {
    let Some((peer_name, service)) =
        find_peer_service_target(env, id, owner_host, source_machine, port)?
    else {
        bail!("service `{id}` was not found on the requested peer target");
    };
    let owner = peer_service_owner(env, &peer_name, &service);
    let mut messages = run_remote_restart(env, owner, id)?;
    let mut state = State::load(&env.paths.state_file)?;
    start_peer_service_tunnel_or_reuse_reverse(
        env,
        &mut state,
        &peer_name,
        &service,
        local_port,
        &mut messages,
    )?;
    state.save(&env.paths.state_file)?;
    Ok(messages)
}

pub fn open_remote_target(
    env: &BridgeEnv,
    id: &str,
    owner_host: Option<&str>,
    source_machine: Option<&str>,
    port: Option<u16>,
    local_port: Option<u16>,
) -> Result<String> {
    let Some((peer_name, service)) =
        find_peer_service_target(env, id, owner_host, source_machine, port)?
    else {
        bail!("service `{id}` was not found on the requested peer target");
    };
    let local_forward = local_forward_allowed(env, &service.tunnel_modes);
    if service.lifecycle.startup == StartupPolicy::OnDemand {
        let owner = peer_service_owner(env, &peer_name, &service);
        let _ = run_remote_up(env, owner, id)?;
    }
    if local_forward {
        let mut state = State::load(&env.paths.state_file)?;
        let mut messages = Vec::new();
        start_peer_service_tunnel_or_reuse_reverse(
            env,
            &mut state,
            &peer_name,
            &service,
            local_port,
            &mut messages,
        )?;
        state.save(&env.paths.state_file)?;
    }
    let url = peer_open_url_with_local_port(&service, local_forward, local_port);
    webbrowser::open(&url).with_context(|| format!("open {url}"))?;
    Ok(url)
}

pub fn rename_title_target(
    env: &BridgeEnv,
    id: &str,
    title: &str,
    owner_host: Option<&str>,
    source_machine: Option<&str>,
    port: Option<u16>,
) -> Result<Vec<String>> {
    let Some((peer_name, service)) =
        find_peer_service_target(env, id, owner_host, source_machine, port)?
    else {
        bail!("service `{id}` was not found on the requested peer target");
    };
    let owner = peer_service_owner(env, &peer_name, &service);
    run_remote_rename(env, owner, id, title)
}

pub fn rename_title(env: &BridgeEnv, id: &str, title: &str) -> Result<Vec<String>> {
    let title = title.trim();
    if title.is_empty() {
        bail!("title must not be empty");
    }

    let registry = Registry::load(&env.paths.registry_file)?;
    if let Some((entry, mut cfg)) = registry.try_get_entry_config(id)? {
        if cfg.owner_host != env.machine_id && env.app.peers.contains_key(&cfg.owner_host) {
            return run_remote_rename(env, &cfg.owner_host, id, title);
        }
        cfg.title = title.to_string();
        save_bridge_config(&entry.config_path, &cfg)
            .with_context(|| format!("rename `{id}` in {}", entry.config_path.display()))?;
        return Ok(vec![format!("renamed {id} to {title}")]);
    }

    let Some((peer_name, service)) = find_peer_service(env, id)? else {
        bail!("service `{id}` is not registered locally and was not found on configured peers");
    };
    let owner = if env.app.peers.contains_key(&service.owner_host) {
        service.owner_host.as_str()
    } else {
        peer_name.as_str()
    };
    run_remote_rename(env, owner, id, title)
}

fn run_remote_rename(env: &BridgeEnv, owner: &str, id: &str, title: &str) -> Result<Vec<String>> {
    if owner == env.machine_id {
        return rename_title(env, id, title);
    }
    let output = peer::run_bridgeboard_command(
        &env.app,
        owner,
        &["rename", id, "--title", title],
        Duration::from_secs(30),
    )
    .map_err(|err| anyhow::anyhow!("rename `{id}` on `{owner}` failed to start: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        bail!("rename `{id}` on `{owner}` failed: {detail}");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut messages = vec![format!("remote rename {id} on {owner}")];
    messages.extend(
        stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| format!("{owner}: {line}")),
    );
    messages.extend(
        stderr
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| format!("{owner} warning: {line}")),
    );
    Ok(messages)
}

fn run_remote_up(env: &BridgeEnv, owner: &str, id: &str) -> Result<Vec<String>> {
    if owner == env.machine_id {
        return up(env, id);
    }
    let output =
        peer::run_bridgeboard_command(&env.app, owner, &["up", id], Duration::from_secs(90))
            .map_err(|err| {
                anyhow::anyhow!("remote-up `{id}` on `{owner}` failed to start: {err}")
            })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        bail!("remote-up `{id}` on `{owner}` failed: {detail}");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut messages = vec![format!("remote-up {id} on {owner}")];
    messages.extend(
        stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| format!("{owner}: {line}")),
    );
    messages.extend(
        stderr
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| format!("{owner} warning: {line}")),
    );
    Ok(messages)
}

fn run_remote_down(env: &BridgeEnv, owner: &str, id: &str) -> Result<Vec<String>> {
    if owner == env.machine_id {
        return down(env, id);
    }
    run_remote_service_command(env, owner, id, "down", Duration::from_secs(90))
}

fn run_remote_restart(env: &BridgeEnv, owner: &str, id: &str) -> Result<Vec<String>> {
    if owner == env.machine_id {
        return restart(env, id);
    }
    run_remote_service_command(env, owner, id, "restart", Duration::from_secs(120))
}

fn run_remote_service_command(
    env: &BridgeEnv,
    owner: &str,
    id: &str,
    action: &str,
    timeout: Duration,
) -> Result<Vec<String>> {
    let output =
        peer::run_bridgeboard_command(&env.app, owner, &[action, id], timeout).map_err(|err| {
            anyhow::anyhow!("remote-{action} `{id}` on `{owner}` failed to start: {err}")
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        bail!("remote-{action} `{id}` on `{owner}` failed: {detail}");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut messages = vec![format!("remote-{action} {id} on {owner}")];
    messages.extend(
        stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| format!("{owner}: {line}")),
    );
    messages.extend(
        stderr
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| format!("{owner} warning: {line}")),
    );
    Ok(messages)
}

fn up_inner(
    env: &BridgeEnv,
    id: &str,
    mark_desired: bool,
    start_reverse_tunnels: bool,
) -> Result<Vec<String>> {
    let registry = Registry::load(&env.paths.registry_file)?;
    let mut state = State::load(&env.paths.state_file)?;
    let mut messages = Vec::new();
    let Some((entry, cfg)) = registry.try_get_entry_config(id)? else {
        let Some((peer_name, service)) = find_peer_service(env, id)? else {
            bail!("service `{id}` is not registered locally and was not found on configured peers");
        };
        let owner = peer_service_owner(env, &peer_name, &service);
        messages.extend(run_remote_up(env, owner, id)?);
        start_peer_service_tunnel_or_reuse_reverse(
            env,
            &mut state,
            &peer_name,
            &service,
            None,
            &mut messages,
        )?;
        state.save(&env.paths.state_file)?;
        return Ok(messages);
    };

    if cfg.owner_host == env.machine_id {
        start_owned_service(
            env,
            Some(&entry),
            &cfg,
            &mut state,
            mark_desired,
            &mut messages,
        )?;
        if start_reverse_tunnels {
            for mode in &cfg.tunnel.modes {
                if *mode == TunnelMode::ReverseForward {
                    for (peer_name, peer_cfg) in &env.app.peers {
                        let ssh_alias = peer_cfg.ssh_alias.as_deref().unwrap_or(peer_name);
                        let pid =
                            process::start_tunnel(&cfg, *mode, peer_name, ssh_alias, &mut state)?;
                        messages.push(format!("reverse tunnel to {peer_name} pid {pid}"));
                    }
                }
            }
        }
    } else {
        messages.extend(run_remote_up(env, &cfg.owner_host, id)?);
        start_config_tunnel_or_reuse_reverse(env, &mut state, &cfg, &mut messages)?;
    }
    state.save(&env.paths.state_file)?;
    Ok(messages)
}

fn stop_local_tunnels(env: &BridgeEnv, id: &str, messages: &mut Vec<String>) -> Result<()> {
    let mut state = State::load(&env.paths.state_file)?;
    process::stop_tunnels_for(id, &mut state)?;
    state.save(&env.paths.state_file)?;
    messages.push(format!("stopped local tunnels for {id}"));
    Ok(())
}

pub fn down(env: &BridgeEnv, id: &str) -> Result<Vec<String>> {
    let registry = Registry::load(&env.paths.registry_file)?;
    let mut state = State::load(&env.paths.state_file)?;
    let mut messages = Vec::new();
    process::stop_tunnels_for(id, &mut state)?;
    if let Some(cfg) = registry.try_get_config(id)? {
        if cfg.owner_host == env.machine_id {
            process::stop_service(&cfg, &mut state)?;
            if cfg.service.mode == ServiceMode::Managed {
                set_desired(&mut state, id, DesiredState::Stopped);
                messages.push(format!("stopped service {id}"));
            } else {
                let last_status = state
                    .services
                    .get(id)
                    .and_then(|service| service.last_status.as_deref())
                    .unwrap_or("external-not-stopped");
                if let Some(detail) = last_status.strip_prefix("external-stopped:") {
                    messages.push(format!("stopped external service {id} ({detail})"));
                } else {
                    match last_status {
                        "external-stopped-by-command" => {
                            messages.push(format!("stopped external service {id} by command"))
                        }
                        "external-task-ended" => messages
                            .push(format!("ended scheduled task for external service {id}")),
                        _ => messages.push(format!(
                            "stopped tunnels for external record {id}; service process was not touched"
                        )),
                    }
                }
            }
        } else {
            messages.push(format!("stopped local tunnels for {id}"));
        }
    } else {
        messages.push(format!("stopped local tunnels for {id}"));
    }
    state.save(&env.paths.state_file)?;
    Ok(messages)
}

pub fn restart(env: &BridgeEnv, id: &str) -> Result<Vec<String>> {
    let registry = Registry::load(&env.paths.registry_file)?;
    if let Some(cfg) = registry.try_get_config(id)? {
        if cfg.owner_host == env.machine_id && cfg.service.mode == ServiceMode::External {
            if let Some(command) = cfg.service.restart_command.as_deref() {
                process::run_shell_command(command, config::service_cwd(&cfg))?;
                let mut state = State::load(&env.paths.state_file)?;
                let entry = state.services.entry(id.to_string()).or_default();
                entry.last_status = Some("external-restarted-by-command".into());
                entry.updated_at = Some(crate::time::now_iso());
                state.save(&env.paths.state_file)?;
                return Ok(vec![format!("restarted external service {id} by command")]);
            }
        }
    }
    let mut messages = down(env, id)?;
    messages.extend(up(env, id)?);
    Ok(messages)
}

pub fn prepare_open(
    env: &BridgeEnv,
    id: &str,
    owner_host: Option<&str>,
    source_machine: Option<&str>,
    port: Option<u16>,
    local_port: Option<u16>,
    target: &str,
) -> Result<PrepareOpenResult> {
    let target = normalized_prepare_open_target(target);
    let source_targets_peer = source_machine
        .map(|source| source != env.machine_id)
        .unwrap_or(false);
    if source_targets_peer {
        return prepare_open_peer_target(
            env,
            id,
            owner_host,
            source_machine,
            port,
            local_port,
            target,
        );
    }

    let registry = Registry::load(&env.paths.registry_file)?;
    if let Some((entry, cfg)) = registry.try_get_entry_config(id)? {
        let owner_matches = owner_host
            .map(|owner| owner == cfg.owner_host)
            .unwrap_or(true);
        let port_matches = port.map(|wanted| wanted == cfg.port).unwrap_or(true);
        if owner_matches && port_matches {
            return prepare_open_config(env, &entry, &cfg, target);
        }
        if source_machine
            .map(|source| source == env.machine_id)
            .unwrap_or(false)
        {
            bail!(
                "service `{id}` on local source is owned by `{}`, not `{}`",
                cfg.owner_host,
                owner_host.unwrap_or_default()
            );
        }
    } else if source_machine
        .map(|source| source == env.machine_id)
        .unwrap_or(false)
    {
        bail!(
            "service `{id}` is not registered on local source `{}`",
            env.machine_id
        );
    }

    prepare_open_peer_target(
        env,
        id,
        owner_host,
        source_machine,
        port,
        local_port,
        target,
    )
}

fn prepare_open_config(
    env: &BridgeEnv,
    entry: &RegistryEntry,
    cfg: &BridgeConfig,
    target: &str,
) -> Result<PrepareOpenResult> {
    let mut messages = Vec::new();
    if cfg.service.lifecycle.startup == StartupPolicy::OnDemand {
        messages.extend(up(env, &cfg.id)?);
    }
    let url = prepare_open_config_url(cfg, target);
    let state = State::load(&env.paths.state_file)?;
    let status_row = crate::status::row_for(cfg, &env.machine_id, &state);
    let (actions, warnings) = split_action_messages(messages);
    Ok(PrepareOpenResult {
        target: target.to_string(),
        service_ref: PrepareOpenServiceRef {
            id: cfg.id.clone(),
            owner_host: cfg.owner_host.clone(),
            source_machine: env.machine_id.clone(),
            port: cfg.port,
        },
        source_config_path: entry.config_path.display().to_string(),
        title: cfg.title.clone(),
        url: url.clone(),
        origin: origin_for_url(&url),
        local_machine_id: env.machine_id.clone(),
        service_mode: service_mode_label(cfg.service.mode).into(),
        tunnel_modes: tunnel_modes_label_for_peer(
            env,
            cfg.owner_host != env.machine_id,
            &cfg.tunnel.modes,
        ),
        startup_policy: startup_policy_label(cfg.service.lifecycle.startup).into(),
        restart_policy: restart_policy_label(cfg.service.lifecycle.restart).into(),
        runtime_status: status_row.service,
        direct_open: true,
        local_port: None,
        network_url: cfg.network_url.clone(),
        actions,
        warnings,
    })
}

fn prepare_open_peer_target(
    env: &BridgeEnv,
    id: &str,
    owner_host: Option<&str>,
    source_machine: Option<&str>,
    port: Option<u16>,
    local_port: Option<u16>,
    target: &str,
) -> Result<PrepareOpenResult> {
    let Some((peer_name, service)) =
        find_peer_service_target(env, id, owner_host, source_machine, port)?
    else {
        bail!("service `{id}` was not found on the requested peer target");
    };
    let mut messages = Vec::new();
    let local_forward = local_forward_allowed(env, &service.tunnel_modes);
    if service.lifecycle.startup == StartupPolicy::OnDemand {
        let owner = peer_service_owner(env, &peer_name, &service);
        messages.extend(run_remote_up(env, owner, id)?);
    }
    if local_forward {
        let mut state = State::load(&env.paths.state_file)?;
        start_peer_service_tunnel_or_reuse_reverse(
            env,
            &mut state,
            &peer_name,
            &service,
            local_port,
            &mut messages,
        )?;
        state.save(&env.paths.state_file)?;
    }
    let url = peer_open_url_with_local_port(&service, local_forward, local_port);
    let (actions, warnings) = split_action_messages(messages);
    Ok(PrepareOpenResult {
        target: target.to_string(),
        service_ref: PrepareOpenServiceRef {
            id: service.id.clone(),
            owner_host: service.owner_host.clone(),
            source_machine: peer_name,
            port: service.port,
        },
        source_config_path: service.config_path.display().to_string(),
        title: service.title.clone(),
        url: url.clone(),
        origin: origin_for_url(&url),
        local_machine_id: env.machine_id.clone(),
        service_mode: service_mode_label(service.service_mode).into(),
        tunnel_modes: tunnel_modes_label_for_peer(env, true, &service.tunnel_modes),
        startup_policy: startup_policy_label(service.lifecycle.startup).into(),
        restart_policy: restart_policy_label(service.lifecycle.restart).into(),
        runtime_status: service
            .runtime_status
            .clone()
            .unwrap_or_else(|| "peer-export".into()),
        direct_open: true,
        local_port: if local_forward {
            Some(local_port.unwrap_or(service.port))
        } else {
            None
        },
        network_url: service.network_url.clone(),
        actions,
        warnings,
    })
}

pub fn open(env: &BridgeEnv, id: &str) -> Result<String> {
    let prepared = prepare_open(env, id, None, None, None, None, "external")?;
    webbrowser::open(&prepared.url).with_context(|| format!("open {}", prepared.url))?;
    Ok(prepared.url)
}

fn normalized_prepare_open_target(target: &str) -> &str {
    match target {
        "external" => "external",
        _ => "internal",
    }
}

fn prepare_open_config_url(cfg: &BridgeConfig, target: &str) -> String {
    if target == "internal" {
        let raw = cfg
            .local_url
            .clone()
            .or_else(|| cfg.open_url.clone())
            .or_else(|| cfg.network_url.clone())
            .unwrap_or_else(|| format!("http://127.0.0.1:{}/", cfg.port));
        return normalize_embedded_loopback_url(&raw);
    }
    open_url(cfg)
}

fn normalize_embedded_loopback_url(raw_url: &str) -> String {
    let Ok(mut url) = Url::parse(raw_url) else {
        return raw_url.to_string();
    };
    if matches!(url.host_str(), Some("0.0.0.0") | Some("::"))
        && url.set_host(Some("127.0.0.1")).is_ok()
    {
        return url.to_string();
    }
    raw_url.to_string()
}

pub fn split_action_messages(messages: Vec<String>) -> (Vec<String>, Vec<String>) {
    let mut actions = Vec::new();
    let mut warnings = Vec::new();
    for message in messages {
        if message.to_lowercase().contains("warning") {
            warnings.push(message);
        } else {
            actions.push(message);
        }
    }
    (actions, warnings)
}

fn origin_for_url(raw_url: &str) -> Option<String> {
    Url::parse(raw_url)
        .ok()
        .map(|url| url.origin().ascii_serialization())
}

fn peer_open_url_with_local_port(
    service: &ServiceExport,
    local_tunnel: bool,
    local_port: Option<u16>,
) -> String {
    if local_tunnel {
        if let Some(port) = local_port {
            return format!("http://127.0.0.1:{port}/");
        }
        return service
            .local_url
            .clone()
            .unwrap_or_else(|| format!("http://127.0.0.1:{}/", service.port));
    }
    service
        .network_url
        .clone()
        .or_else(|| service.open_url.clone())
        .or_else(|| service.local_url.clone())
        .unwrap_or_else(|| format!("http://127.0.0.1:{}/", service.port))
}

pub fn log_tail(env: &BridgeEnv, id: &str, lines: usize) -> Result<String> {
    let registry = Registry::load(&env.paths.registry_file)?;
    let cfg = registry.get_config(id)?;
    if cfg.owner_host != env.machine_id {
        bail!("logs are only local for owner host `{}`", cfg.owner_host);
    }
    let path = config::service_log_path(&cfg)
        .with_context(|| format!("service `{}` has no log_file", cfg.id))?;
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes).replace('\0', "");
    let all_lines: Vec<&str> = text.lines().collect();
    let start = all_lines.len().saturating_sub(lines);
    Ok(all_lines[start..].join("\n"))
}

pub fn startup(env: &BridgeEnv, dry_run: bool) -> Result<Vec<String>> {
    let registry = Registry::load(&env.paths.registry_file)?;
    let mut state = State::load(&env.paths.state_file)?;
    let mut messages = Vec::new();
    for (_, cfg) in registry.load_configs()? {
        if !is_local_managed_with_startup(&cfg, env, StartupPolicy::Autostart) {
            continue;
        }
        if dry_run {
            messages.push(format!("would start {}", cfg.id));
            continue;
        }
        start_owned_service(env, None, &cfg, &mut state, true, &mut messages)?;
    }
    if !dry_run {
        state.save(&env.paths.state_file)?;
    }
    if messages.is_empty() {
        messages.push("no autostart services".into());
    }
    Ok(messages)
}

pub fn supervise_once(env: &BridgeEnv) -> Result<Vec<String>> {
    let registry = Registry::load(&env.paths.registry_file)?;
    let mut state = State::load(&env.paths.state_file)?;
    let mut messages = Vec::new();
    for (_, cfg) in registry.load_configs()? {
        if cfg.owner_host != env.machine_id || cfg.service.mode != ServiceMode::Managed {
            continue;
        }
        if cfg.service.lifecycle.restart != RestartPolicy::OnFailure {
            continue;
        }
        if desired_for(&state, &cfg.id) != Some(DesiredState::Running) {
            continue;
        }
        if managed_service_alive(&cfg) {
            continue;
        }
        messages.push(format!("restarting {} after failed pid check", cfg.id));
        start_owned_service(env, None, &cfg, &mut state, true, &mut messages)?;
    }
    state.save(&env.paths.state_file)?;
    Ok(messages)
}

pub fn render_port_table(rows: &[PortRow]) -> String {
    let mut out = String::new();
    out.push_str(
        "PORT   ID                 OWNER        MODE      LIFE       RESTART    DESIRED  STATUS                 URL\n",
    );
    out.push_str(
        "------ ------------------ ------------ --------- ---------- ---------- -------- ---------------------- ------------------------------\n",
    );
    for row in rows {
        out.push_str(&format!(
            "{:<6} {:<18} {:<12} {:<9} {:<10} {:<10} {:<8} {:<22} {}\n",
            row.port,
            truncate(&row.id, 18),
            truncate(&row.owner_host, 12),
            truncate(&row.service_mode, 9),
            truncate(&row.startup_policy, 10),
            truncate(&row.restart_policy, 10),
            truncate(&row.desired_state, 8),
            truncate(&row.runtime_status, 22),
            row.url
        ));
    }
    out
}

pub fn service_mode_label(mode: ServiceMode) -> &'static str {
    match mode {
        ServiceMode::Managed => "managed",
        ServiceMode::External => "external",
    }
}

pub fn tunnel_modes_label(modes: &[TunnelMode]) -> String {
    if modes.is_empty() {
        return "reserved".into();
    }
    modes
        .iter()
        .map(|mode| match mode {
            TunnelMode::LocalForward => "local",
            TunnelMode::ReverseForward => "reverse",
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn tunnel_mode_value(mode: TunnelMode) -> &'static str {
    match mode {
        TunnelMode::LocalForward => "local_forward",
        TunnelMode::ReverseForward => "reverse_forward",
    }
}

fn tunnel_modes_label_for_peer(env: &BridgeEnv, is_peer: bool, modes: &[TunnelMode]) -> String {
    if is_peer && modes.is_empty() && env.app.defaults.assume_local_forward_for_peers {
        return "local(default)".into();
    }
    tunnel_modes_label(modes)
}

fn local_forward_allowed(env: &BridgeEnv, modes: &[TunnelMode]) -> bool {
    modes.contains(&TunnelMode::LocalForward)
        || (modes.is_empty() && env.app.defaults.assume_local_forward_for_peers)
}

pub fn startup_policy_label(policy: StartupPolicy) -> &'static str {
    match policy {
        StartupPolicy::Manual => "manual",
        StartupPolicy::OnDemand => "on_demand",
        StartupPolicy::Autostart => "autostart",
    }
}

pub fn restart_policy_label(policy: RestartPolicy) -> &'static str {
    match policy {
        RestartPolicy::Never => "never",
        RestartPolicy::OnFailure => "on_failure",
    }
}

pub fn lifecycle_label(lifecycle: &LifecycleConfig) -> String {
    format!(
        "{}/{}",
        startup_policy_label(lifecycle.startup),
        restart_policy_label(lifecycle.restart)
    )
}

fn start_owned_service(
    _env: &BridgeEnv,
    entry: Option<&RegistryEntry>,
    cfg: &BridgeConfig,
    state: &mut State,
    mark_desired: bool,
    messages: &mut Vec<String>,
) -> Result<()> {
    if cfg.service.mode == ServiceMode::Managed {
        let pid = process::start_service(cfg, state)?;
        verify_managed_service_stable(cfg, pid, state)?;
        if mark_desired {
            set_desired(state, &cfg.id, DesiredState::Running);
        }
        messages.push(format!("service {} running pid {}", cfg.id, pid));
    } else if cfg.service.detach.as_deref() == Some("scheduled_task") {
        let Some(command) = cfg.service.start_command.as_deref() else {
            messages.push(format!("service {} is external; recorded only", cfg.id));
            return Ok(());
        };
        let Some(task_name) = cfg.service.task_name.as_deref() else {
            messages.push(format!(
                "service {} has no task_name; recorded only",
                cfg.id
            ));
            return Ok(());
        };
        let wrapper_path = entry
            .map(|entry| entry.config_path.with_extension("cmd"))
            .unwrap_or_else(|| std::path::PathBuf::from(format!("{}.cmd", cfg.id)));
        let log_path = config::service_log_path(cfg);
        process::start_windows_scheduled_task(
            task_name,
            &wrapper_path,
            config::service_cwd(cfg),
            command,
            log_path.as_deref(),
        )?;
        messages.push(format!("scheduled task {task_name} started"));
    } else {
        messages.push(format!("service {} is external; recorded only", cfg.id));
    }
    if let Some(url) = &cfg.service.health_url {
        match health::check_http_with_expect(
            url,
            Duration::from_secs(cfg.service.startup_timeout_sec),
            &cfg.service.health_expect,
        ) {
            Ok(status) => {
                messages.push(format!("health: {status}"));
                if let Some(pid) = process::reconcile_managed_listener_pid(cfg, state)? {
                    messages.push(format!("pid reconciled to listener {pid}"));
                }
                let entry = state.services.entry(cfg.id.clone()).or_default();
                entry.last_health = Some(status);
                entry.last_status = Some("healthy".into());
                entry.updated_at = Some(crate::time::now_iso());
            }
            Err(err) => {
                let text = err.to_string();
                messages.push(format!("warning: health check failed: {text}"));
                let entry = state.services.entry(cfg.id.clone()).or_default();
                entry.last_health = Some(format!("failed: {text}"));
                entry.last_status = Some("unhealthy".into());
                entry.updated_at = Some(crate::time::now_iso());
            }
        }
    }
    Ok(())
}

fn verify_managed_service_stable(cfg: &BridgeConfig, pid: u32, state: &mut State) -> Result<()> {
    std::thread::sleep(Duration::from_millis(250));
    for attempt in 0..3 {
        let status = process::managed_service_status(cfg);
        if !managed_status_matches_pid(&status, pid) {
            let entry = state.services.entry(cfg.id.clone()).or_default();
            entry.last_status = Some(format!("unstable:{status}"));
            entry.updated_at = Some(crate::time::now_iso());
            bail!(
                "service `{}` did not reach stable running state: expected running:{pid}, got {status}; resolve the stale pid/port owner before opening tunnels",
                cfg.id
            );
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    Ok(())
}

fn managed_status_matches_pid(status: &str, pid: u32) -> bool {
    status == format!("running:{pid}")
}

fn is_local_managed_with_startup(
    cfg: &BridgeConfig,
    env: &BridgeEnv,
    startup: StartupPolicy,
) -> bool {
    cfg.owner_host == env.machine_id
        && cfg.service.mode == ServiceMode::Managed
        && cfg.service.lifecycle.startup == startup
}

fn managed_service_alive(cfg: &BridgeConfig) -> bool {
    process::managed_service_alive(cfg)
}

fn desired_for(state: &State, id: &str) -> Option<DesiredState> {
    state.services.get(id).and_then(|entry| entry.desired)
}

fn desired_label(desired: Option<DesiredState>) -> String {
    match desired {
        Some(DesiredState::Running) => "running".into(),
        Some(DesiredState::Stopped) => "stopped".into(),
        None => "-".into(),
    }
}

fn set_desired(state: &mut State, id: &str, desired: DesiredState) {
    let entry = state.services.entry(id.to_string()).or_default();
    entry.desired = Some(desired);
    entry.updated_at = Some(crate::time::now_iso());
}

fn find_peer_service(env: &BridgeEnv, id: &str) -> Result<Option<(String, ServiceExport)>> {
    find_peer_service_target(env, id, None, None, None)
}

fn find_peer_service_target(
    env: &BridgeEnv,
    id: &str,
    owner_host: Option<&str>,
    source_machine: Option<&str>,
    port: Option<u16>,
) -> Result<Option<(String, ServiceExport)>> {
    if let Some(peer_name) = source_machine {
        if peer_name == env.machine_id {
            return Ok(None);
        }
        let service = fetch_peer_service(env, peer_name, id, owner_host, port)?;
        return Ok(Some((peer_name.to_string(), service)));
    }
    let peer_results = peer::fetch_peer_exports(&env.app);
    peer::print_peer_warnings(&peer_results);
    for (peer_name, result) in peer_results {
        let Ok(export) = result else {
            continue;
        };
        for service in export.services {
            if service_matches_target(&service, id, owner_host, port) {
                return Ok(Some((peer_name, service)));
            }
        }
    }
    Ok(None)
}

fn fetch_peer_service(
    env: &BridgeEnv,
    peer_name: &str,
    id: &str,
    owner_host: Option<&str>,
    port: Option<u16>,
) -> Result<ServiceExport> {
    let export = peer::fetch_peer_export(&env.app, peer_name)
        .map_err(|err| anyhow::anyhow!("query peer `{peer_name}` failed: {err}"))?;
    export
        .services
        .into_iter()
        .find(|service| service_matches_target(service, id, owner_host, port))
        .with_context(|| {
            if let Some(owner) = owner_host {
                if let Some(port) = port {
                    format!(
                        "service `{id}` owned by `{owner}` on port {port} was not found on peer `{peer_name}`"
                    )
                } else {
                    format!("service `{id}` owned by `{owner}` was not found on peer `{peer_name}`")
                }
            } else {
                format!("service `{id}` was not found on peer `{peer_name}`")
            }
        })
}

fn service_matches_target(
    service: &ServiceExport,
    id: &str,
    owner_host: Option<&str>,
    port: Option<u16>,
) -> bool {
    service.id == id
        && owner_host
            .map(|owner| service.owner_host == owner)
            .unwrap_or(true)
        && port.map(|wanted| service.port == wanted).unwrap_or(true)
}

fn peer_service_owner<'a>(
    env: &'a BridgeEnv,
    peer_name: &'a str,
    service: &'a ServiceExport,
) -> &'a str {
    if env.app.peers.contains_key(&service.owner_host) {
        service.owner_host.as_str()
    } else {
        peer_name
    }
}

fn start_config_tunnel_or_reuse_reverse(
    env: &BridgeEnv,
    state: &mut State,
    cfg: &BridgeConfig,
    messages: &mut Vec<String>,
) -> Result<()> {
    if let Some(pid) = wait_for_reverse_forward_listener(&cfg.tunnel.modes, cfg.port) {
        messages.push(format!(
            "local port {} already available via reverse tunnel pid {}",
            cfg.port, pid
        ));
        return Ok(());
    }
    ensure_local_forward_allowed(env, &cfg.id, &cfg.tunnel.modes)?;
    let pid = process::start_tunnel(
        cfg,
        TunnelMode::LocalForward,
        &cfg.owner_host,
        peer::ssh_alias_for(&env.app, &cfg.owner_host),
        state,
    )?;
    messages.push(format!(
        "local tunnel {} -> {} pid {}",
        cfg.port, cfg.owner_host, pid
    ));
    Ok(())
}

fn start_peer_service_tunnel_or_reuse_reverse(
    env: &BridgeEnv,
    state: &mut State,
    peer_name: &str,
    service: &ServiceExport,
    local_port: Option<u16>,
    messages: &mut Vec<String>,
) -> Result<()> {
    let local_port = local_port.unwrap_or(service.port);
    if local_port == service.port {
        if let Some(pid) = wait_for_reverse_forward_listener(&service.tunnel_modes, local_port) {
            messages.push(format!(
                "local port {} already available via reverse tunnel pid {}",
                local_port, pid
            ));
            return Ok(());
        }
    }
    let pid = start_peer_service_tunnel(env, state, peer_name, service, Some(local_port))?;
    messages.push(format!(
        "local tunnel {} -> {}:{} pid {}",
        local_port, service.owner_host, service.port, pid
    ));
    Ok(())
}

fn wait_for_reverse_forward_listener(modes: &[TunnelMode], port: u16) -> Option<String> {
    for attempt in 0..20 {
        if let Some(pid) = reverse_forward_listener(modes, port) {
            return Some(pid);
        }
        if attempt < 19 {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    None
}

fn reverse_forward_listener(modes: &[TunnelMode], port: u16) -> Option<String> {
    if !modes.contains(&TunnelMode::ReverseForward) {
        return None;
    }
    if let Some(pid) = process::pid_listening_on_port(port).ok().flatten() {
        return Some(pid.to_string());
    }
    if process::tcp_port_open(port) {
        return Some("unknown".into());
    }
    None
}

fn start_peer_service_tunnel(
    env: &BridgeEnv,
    state: &mut State,
    peer_name: &str,
    service: &ServiceExport,
    local_port: Option<u16>,
) -> Result<u32> {
    ensure_local_forward_allowed(env, &service.id, &service.tunnel_modes)?;
    let owner = if env.app.peers.contains_key(&service.owner_host) {
        service.owner_host.as_str()
    } else {
        peer_name
    };
    let ssh_alias = peer::ssh_alias_for(&env.app, owner);
    let bind_host = if service.bind_host.trim().is_empty() {
        "127.0.0.1"
    } else {
        service.bind_host.as_str()
    };
    process::start_tunnel_spec(
        &service.id,
        service.port,
        local_port.unwrap_or(service.port),
        bind_host,
        TunnelMode::LocalForward,
        owner,
        ssh_alias,
        state,
    )
}

fn ensure_local_forward_allowed(env: &BridgeEnv, id: &str, modes: &[TunnelMode]) -> Result<()> {
    if local_forward_allowed(env, modes) {
        return Ok(());
    }
    bail!(
        "service `{id}` reserves its port but does not enable local_forward; set defaults.assume_local_forward_for_peers: true in Bridgeboard config to allow local override"
    )
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    let mut s: String = value.chars().take(width.saturating_sub(3)).collect();
    s.push_str("...");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ServiceConfig, TunnelConfig};
    use crate::paths::AppPaths;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use tempfile::TempDir;

    fn external_test_config() -> BridgeConfig {
        BridgeConfig {
            schema: "portal-bridge.v1".into(),
            id: "web-portal".into(),
            title: "Web Portal".into(),
            owner_host: "workstation".into(),
            port: 24991,
            service: ServiceConfig {
                mode: ServiceMode::External,
                lifecycle: LifecycleConfig::default(),
                cwd: None,
                command: Vec::new(),
                start_command: None,
                detach: None,
                stop_command: None,
                restart_command: None,
                task_name: None,
                pid_source: None,
                pid_port: None,
                pid_file: None,
                pid: None,
                log_file: None,
                health_url: None,
                health_expect: crate::config::HealthExpectConfig::default(),
                startup_timeout_sec: 10,
                notes: None,
            },
            tunnel: TunnelConfig::default(),
            local_url: Some("http://127.0.0.1:24991/".into()),
            network_url: None,
            open_url: Some("http://127.0.0.1:24991/custom".into()),
        }
    }

    fn managed_test_config() -> BridgeConfig {
        let mut cfg = external_test_config();
        cfg.service.mode = ServiceMode::Managed;
        cfg.service.lifecycle = LifecycleConfig {
            startup: StartupPolicy::OnDemand,
            restart: RestartPolicy::OnFailure,
        };
        cfg.service.cwd = Some("/tmp/bridgeboard-runtime-spec-test".into());
        cfg.service.command = vec![
            "python3".into(),
            "-m".into(),
            "http.server".into(),
            "24991".into(),
        ];
        cfg.service.pid_file = Some(".bridgeboard/server.pid".into());
        cfg.service.log_file = Some(".bridgeboard/server.log".into());
        cfg.service.health_url = Some("http://127.0.0.1:24991/health".into());
        cfg.tunnel.modes = vec![TunnelMode::LocalForward];
        cfg
    }

    fn env_with_service(cfg: &BridgeConfig) -> (TempDir, BridgeEnv) {
        let dir = tempfile::tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");
        let registry_file = dir.path().join("registry.json");
        let state_file = dir.path().join("state.json");
        let service_file = dir.path().join("portal-bridge.yaml");
        fs::write(&config_file, "machine_id: workstation\n").unwrap();
        fs::write(&service_file, serde_yaml::to_string(cfg).unwrap()).unwrap();
        let mut registry = Registry::default();
        registry.register(service_file).unwrap();
        registry.save(&registry_file).unwrap();
        let env = BridgeEnv::from_paths(AppPaths {
            config_file,
            registry_file,
            state_file,
        })
        .unwrap();
        (dir, env)
    }

    fn bind_reserved_listener(start: u16) -> TcpListener {
        for port in start..25000 {
            if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
                return listener;
            }
        }
        panic!("no free reserved test port");
    }

    fn serve_once(body: &'static str) -> u16 {
        let listener = bind_reserved_listener(24990);
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 512];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        port
    }

    #[test]
    fn lifecycle_label_uses_config_terms() {
        let lifecycle = LifecycleConfig {
            startup: StartupPolicy::Autostart,
            restart: RestartPolicy::OnFailure,
        };
        assert_eq!(lifecycle_label(&lifecycle), "autostart/on_failure");
    }

    #[test]
    fn managed_status_pid_match_is_exact() {
        assert!(managed_status_matches_pid("running:42", 42));
        assert!(!managed_status_matches_pid(
            "pid-mismatch:42;listener:7",
            42
        ));
        assert!(!managed_status_matches_pid("running:7", 42));
        assert!(!managed_status_matches_pid(
            "multi-listener:7,42;pid_file:42",
            42
        ));
    }

    #[test]
    fn preferred_peer_fallback_port_uses_stable_offset_inside_policy_range() {
        assert_eq!(preferred_peer_fallback_port(24260), Some(24660));
        assert_eq!(preferred_peer_fallback_port(24308), Some(24708));
        assert_eq!(preferred_peer_fallback_port(24699), None);
    }

    #[test]
    fn port_rows_no_runtime_marks_local_runtime_as_not_checked() {
        let cfg = external_test_config();
        let (_dir, env) = env_with_service(&cfg);
        let rows = port_rows_with_runtime(&env, false, false).unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "web-portal");
        assert_eq!(rows[0].runtime_status, "not-checked");
    }

    #[test]
    fn observe_local_external_without_listener_is_unreachable_and_conservative() {
        let listener = bind_reserved_listener(24980);
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut cfg = external_test_config();
        cfg.port = port;
        cfg.local_url = Some(format!("http://127.0.0.1:{port}/"));
        cfg.open_url = None;
        let (_dir, env) = env_with_service(&cfg);
        let envelope = observe_services(&env, None, false, Duration::from_secs(1)).unwrap();

        assert_eq!(envelope.schema, "bridgeboard.observe.v1");
        assert_eq!(envelope.rows.len(), 1);
        let row = &envelope.rows[0];
        assert_eq!(row.service_ref.id, "web-portal");
        assert_eq!(row.observation.status, "unreachable");
        assert_eq!(row.observation.reason, "connection-refused");
        assert_eq!(row.safe_open_actions, vec!["prepare-open".to_string()]);
        assert!(row.safe_lifecycle_actions.is_empty());
    }

    #[test]
    fn observe_local_managed_health_reports_healthy_with_lifecycle_actions() {
        let port = serve_once(r#"{"ok":true}"#);
        let mut cfg = managed_test_config();
        cfg.port = port;
        cfg.service.health_url = Some(format!("http://127.0.0.1:{port}/health"));
        cfg.local_url = Some(format!("http://127.0.0.1:{port}/"));
        cfg.open_url = None;
        let (_dir, env) = env_with_service(&cfg);
        let envelope = observe_services(&env, None, false, Duration::from_secs(1)).unwrap();

        assert_eq!(envelope.rows.len(), 1);
        let row = &envelope.rows[0];
        assert_eq!(row.observation.status, "healthy");
        assert_eq!(row.observation.reason, "http-ok");
        assert!(row.safe_lifecycle_actions.contains(&"up".to_string()));
        assert!(row.safe_lifecycle_actions.contains(&"down".to_string()));
        assert!(row.safe_lifecycle_actions.contains(&"restart".to_string()));
    }

    #[test]
    fn managed_runtime_spec_exposes_launch_and_desired_state_without_yaml_parsing() {
        let cfg = managed_test_config();
        let (_dir, env) = env_with_service(&cfg);
        let rows = managed_runtime_specs(&env, Some("web-portal")).unwrap();

        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.schema, "bridgeboard.runtime-spec.v1");
        assert_eq!(row.service_ref.id, "web-portal");
        assert_eq!(row.desired_state, "-");
        assert_eq!(
            row.command,
            vec![
                "python3".to_string(),
                "-m".to_string(),
                "http.server".to_string(),
                "24991".to_string()
            ]
        );
        assert_eq!(row.cwd, "/tmp/bridgeboard-runtime-spec-test");
        assert!(row.pid_file.ends_with(".bridgeboard/server.pid"));
        assert!(row.log_file.ends_with(".bridgeboard/server.log"));
        assert_eq!(
            row.health_url.as_deref(),
            Some("http://127.0.0.1:24991/health")
        );
        assert_eq!(row.tunnel.modes, vec!["local_forward".to_string()]);
    }

    #[test]
    fn prepare_open_local_external_target_preserves_open_url_without_browser_side_effect() {
        let cfg = external_test_config();
        let (_dir, env) = env_with_service(&cfg);
        let result = prepare_open(
            &env,
            "web-portal",
            Some("workstation"),
            Some("workstation"),
            None,
            None,
            "external",
        )
        .unwrap();

        assert_eq!(result.target, "external");
        assert_eq!(result.service_ref.id, "web-portal");
        assert_eq!(result.service_ref.owner_host, "workstation");
        assert_eq!(result.service_ref.source_machine, "workstation");
        assert_eq!(result.service_ref.port, 24991);
        assert!(result.source_config_path.ends_with("portal-bridge.yaml"));
        assert_eq!(result.url, "http://127.0.0.1:24991/custom");
        assert_eq!(result.origin.as_deref(), Some("http://127.0.0.1:24991"));
        assert_eq!(result.actions, Vec::<String>::new());
        assert_eq!(result.warnings, Vec::<String>::new());
        assert!(result.direct_open);
    }

    #[test]
    fn prepare_open_local_internal_prefers_embeddable_loopback_url() {
        let mut cfg = external_test_config();
        cfg.local_url = None;
        cfg.open_url = Some("http://0.0.0.0:24991/app".into());
        let (_dir, env) = env_with_service(&cfg);
        let result = prepare_open(
            &env,
            "web-portal",
            Some("workstation"),
            Some("workstation"),
            None,
            None,
            "internal",
        )
        .unwrap();

        assert_eq!(result.target, "internal");
        assert_eq!(result.url, "http://127.0.0.1:24991/app");
        assert_eq!(result.origin.as_deref(), Some("http://127.0.0.1:24991"));
    }

    #[test]
    fn prepare_open_splits_action_and_warning_messages() {
        let (actions, warnings) = split_action_messages(vec![
            "remote-up app on eva-02".into(),
            "eva-02 warning: slow SSH".into(),
            "local tunnel 24201 -> eva-02:24201 pid 7".into(),
        ]);
        assert_eq!(
            actions,
            vec![
                "remote-up app on eva-02".to_string(),
                "local tunnel 24201 -> eva-02:24201 pid 7".to_string()
            ]
        );
        assert_eq!(warnings, vec!["eva-02 warning: slow SSH".to_string()]);
    }
}
