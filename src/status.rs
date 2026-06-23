use crate::config::{BridgeConfig, RestartPolicy, ServiceMode, StartupPolicy};
use crate::process::{managed_service_status, pid_alive, service_listener_pid};
use crate::registry::ServiceExport;
use crate::state::{DesiredState, State};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct StatusRow {
    pub id: String,
    pub title: String,
    pub owner_host: String,
    pub port: u16,
    pub role: String,
    pub service_mode: String,
    pub startup_policy: String,
    pub restart_policy: String,
    pub desired_state: String,
    pub service: String,
    pub tunnels: String,
    pub url: String,
    pub local_url: Option<String>,
    pub network_url: Option<String>,
    pub last_health: Option<String>,
    pub last_status: Option<String>,
    pub pid_source: Option<String>,
    pub pid_port: Option<u16>,
    pub task_name: Option<String>,
}

pub fn row_for(cfg: &BridgeConfig, machine_id: &str, state: &State) -> StatusRow {
    let owner_local = cfg.owner_host == machine_id;
    let service = if owner_local {
        match cfg.service.mode {
            ServiceMode::Managed => managed_service_status(cfg),
            ServiceMode::External => {
                if let Some(pid) = service_listener_pid(cfg) {
                    format!("external-running:{pid}")
                } else {
                    match cfg.service.pid {
                        Some(pid) if pid_alive(pid) => format!("external-running:{pid}"),
                        Some(_) => "external-stopped".into(),
                        None => "external-record".into(),
                    }
                }
            }
        }
    } else {
        "remote-owner".into()
    };
    let prefix = format!("{}:", cfg.id);
    let state_entry = state.services.get(&cfg.id);
    let active: Vec<String> = state
        .tunnels
        .iter()
        .filter(|(key, tunnel)| {
            key.starts_with(&prefix) && tunnel.pid.map(pid_alive).unwrap_or(false)
        })
        .map(|(_, tunnel)| format!("{}:{}", tunnel.peer, tunnel.pid.unwrap_or(0)))
        .collect();
    StatusRow {
        id: cfg.id.clone(),
        title: cfg.title.clone(),
        owner_host: cfg.owner_host.clone(),
        port: cfg.port,
        role: if owner_local {
            "owner".into()
        } else {
            "client".into()
        },
        service_mode: service_mode_label(cfg.service.mode).into(),
        startup_policy: startup_policy_label(cfg.service.lifecycle.startup).into(),
        restart_policy: restart_policy_label(cfg.service.lifecycle.restart).into(),
        desired_state: desired_label(state_entry.and_then(|entry| entry.desired)),
        service,
        tunnels: if active.is_empty() {
            "-".into()
        } else {
            active.join(",")
        },
        url: crate::config::open_url(cfg),
        local_url: cfg.local_url.clone(),
        network_url: cfg.network_url.clone(),
        last_health: state_entry.and_then(|entry| entry.last_health.clone()),
        last_status: state_entry.and_then(|entry| entry.last_status.clone()),
        pid_source: cfg
            .service
            .pid_source
            .clone()
            .or_else(|| state_entry.and_then(|entry| entry.pid_source.clone())),
        pid_port: cfg
            .service
            .pid_port
            .or_else(|| state_entry.and_then(|entry| entry.pid_port)),
        task_name: cfg.service.task_name.clone(),
    }
}

pub fn row_for_export(export: &ServiceExport, machine_id: &str, state: &State) -> StatusRow {
    let prefix = format!("{}:", export.id);
    let active: Vec<String> = state
        .tunnels
        .iter()
        .filter(|(key, tunnel)| {
            key.starts_with(&prefix) && tunnel.pid.map(pid_alive).unwrap_or(false)
        })
        .map(|(_, tunnel)| format!("{}:{}", tunnel.peer, tunnel.pid.unwrap_or(0)))
        .collect();
    StatusRow {
        id: export.id.clone(),
        title: export.title.clone(),
        owner_host: export.owner_host.clone(),
        port: export.port,
        role: if export.owner_host == machine_id {
            "owner".into()
        } else {
            "peer".into()
        },
        service_mode: service_mode_label(export.service_mode).into(),
        startup_policy: startup_policy_label(export.lifecycle.startup).into(),
        restart_policy: restart_policy_label(export.lifecycle.restart).into(),
        desired_state: desired_label(
            state
                .services
                .get(&export.id)
                .and_then(|entry| entry.desired),
        ),
        service: export
            .runtime_status
            .clone()
            .unwrap_or_else(|| "peer-export".into()),
        tunnels: if active.is_empty() {
            "-".into()
        } else {
            active.join(",")
        },
        url: export
            .open_url
            .clone()
            .unwrap_or_else(|| format!("http://127.0.0.1:{}/", export.port)),
        local_url: export.local_url.clone(),
        network_url: export.network_url.clone(),
        last_health: state
            .services
            .get(&export.id)
            .and_then(|entry| entry.last_health.clone()),
        last_status: state
            .services
            .get(&export.id)
            .and_then(|entry| entry.last_status.clone()),
        pid_source: export.pid_source.clone(),
        pid_port: export.pid_port,
        task_name: export.task_name.clone(),
    }
}

pub fn render_table(rows: &[StatusRow]) -> String {
    let mut out = String::new();
    out.push_str("ID                 PORT   ROLE    SERVICE          TUNNELS              URL\n");
    out.push_str("------------------ ------ ------- ---------------- -------------------- ------------------------------\n");
    for row in rows {
        out.push_str(&format!(
            "{:<18} {:<6} {:<7} {:<16} {:<20} {}\n",
            truncate(&row.id, 18),
            row.port,
            row.role,
            truncate(&row.service, 16),
            truncate(&row.tunnels, 20),
            row.url
        ));
    }
    out
}

fn service_mode_label(mode: ServiceMode) -> &'static str {
    match mode {
        ServiceMode::Managed => "managed",
        ServiceMode::External => "external",
    }
}

fn startup_policy_label(policy: StartupPolicy) -> &'static str {
    match policy {
        StartupPolicy::Manual => "manual",
        StartupPolicy::OnDemand => "on_demand",
        StartupPolicy::Autostart => "autostart",
    }
}

fn restart_policy_label(policy: RestartPolicy) -> &'static str {
    match policy {
        RestartPolicy::Never => "never",
        RestartPolicy::OnFailure => "on_failure",
    }
}

fn desired_label(desired: Option<DesiredState>) -> String {
    match desired {
        Some(DesiredState::Running) => "running".into(),
        Some(DesiredState::Stopped) => "stopped".into(),
        None => "-".into(),
    }
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    let mut s: String = value.chars().take(width.saturating_sub(1)).collect();
    s.push('…');
    s
}
