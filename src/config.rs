use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub machine_id: Option<String>,
    #[serde(default)]
    pub defaults: AppDefaults,
    #[serde(default)]
    pub peers: BTreeMap<String, PeerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppDefaults {
    #[serde(default = "default_handoff_tunnel_modes")]
    pub handoff_tunnel_modes: Vec<TunnelMode>,
    #[serde(default)]
    pub assume_local_forward_for_peers: bool,
}

impl Default for AppDefaults {
    fn default() -> Self {
        Self {
            handoff_tunnel_modes: default_handoff_tunnel_modes(),
            assume_local_forward_for_peers: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerConfig {
    #[serde(default)]
    pub ssh_alias: Option<String>,
    #[serde(default)]
    pub bridgeboard_bin: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeConfig {
    pub schema: String,
    pub id: String,
    pub title: String,
    pub owner_host: String,
    pub port: u16,
    pub service: ServiceConfig,
    #[serde(default)]
    pub tunnel: TunnelConfig,
    #[serde(default)]
    pub local_url: Option<String>,
    #[serde(default)]
    pub network_url: Option<String>,
    #[serde(default)]
    pub open_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    #[serde(default = "default_service_mode")]
    pub mode: ServiceMode,
    #[serde(default)]
    pub lifecycle: LifecycleConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detach: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_url: Option<String>,
    #[serde(default, skip_serializing_if = "HealthExpectConfig::is_empty")]
    pub health_expect: HealthExpectConfig,
    #[serde(default = "default_startup_timeout")]
    pub startup_timeout_sec: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HealthExpectConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body_contains: Vec<String>,
}

impl HealthExpectConfig {
    pub fn is_empty(&self) -> bool {
        self.body_contains.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelConfig {
    #[serde(default)]
    pub modes: Vec<TunnelMode>,
    #[serde(default = "default_bind_host")]
    pub bind_host: String,
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            modes: vec![TunnelMode::LocalForward],
            bind_host: default_bind_host(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TunnelMode {
    LocalForward,
    ReverseForward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceMode {
    Managed,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleConfig {
    #[serde(default = "default_startup_policy")]
    pub startup: StartupPolicy,
    #[serde(default = "default_restart_policy")]
    pub restart: RestartPolicy,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            startup: default_startup_policy(),
            restart: default_restart_policy(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupPolicy {
    Manual,
    OnDemand,
    Autostart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy {
    Never,
    OnFailure,
}

fn default_service_mode() -> ServiceMode {
    ServiceMode::Managed
}

fn default_startup_policy() -> StartupPolicy {
    StartupPolicy::Manual
}

fn default_restart_policy() -> RestartPolicy {
    RestartPolicy::Never
}

fn default_bind_host() -> String {
    "127.0.0.1".to_string()
}

fn default_handoff_tunnel_modes() -> Vec<TunnelMode> {
    vec![TunnelMode::LocalForward]
}

fn default_startup_timeout() -> u64 {
    10
}

pub fn load_app_config(path: &Path) -> Result<AppConfig> {
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let cfg: AppConfig =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    Ok(cfg)
}

pub fn load_bridge_config(path: &Path) -> Result<BridgeConfig> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let cfg: BridgeConfig =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    validate_bridge_config(&cfg)?;
    Ok(cfg)
}

pub fn save_bridge_config(path: &Path, cfg: &BridgeConfig) -> Result<()> {
    validate_bridge_config(cfg)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("yaml.tmp");
    fs::write(&tmp, serde_yaml::to_string(cfg)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| {
        format!(
            "replace {} with {}",
            path.display(),
            tmp.file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default()
        )
    })?;
    Ok(())
}

pub fn validate_bridge_config(cfg: &BridgeConfig) -> Result<()> {
    if cfg.schema != "portal-bridge.v1" {
        bail!(
            "unsupported schema `{}`; expected portal-bridge.v1",
            cfg.schema
        );
    }
    if cfg.id.trim().is_empty() {
        bail!("id is required");
    }
    if cfg.owner_host.trim().is_empty() {
        bail!("owner_host is required");
    }
    for expected in &cfg.service.health_expect.body_contains {
        if expected.is_empty() {
            bail!("service.health_expect.body_contains entries must not be empty");
        }
    }
    if !(24000..=24999).contains(&cfg.port) {
        bail!(
            "port {} is outside reserved Bridgeboard range 24000-24999",
            cfg.port
        );
    }
    match cfg.service.mode {
        ServiceMode::Managed => {
            if cfg.service.cwd.is_none() {
                bail!("managed service.cwd is required");
            }
            if cfg.service.command.is_empty() {
                bail!("managed service.command must not be empty");
            }
            let Some(pid_file) = &cfg.service.pid_file else {
                bail!("managed service.pid_file is required");
            };
            let Some(log_file) = &cfg.service.log_file else {
                bail!("managed service.log_file is required");
            };
            if pid_file.is_absolute() {
                bail!("managed service.pid_file must be relative to service.cwd");
            }
            if log_file.is_absolute() {
                bail!("managed service.log_file must be relative to service.cwd");
            }
        }
        ServiceMode::External => {
            if cfg.service.lifecycle.startup != StartupPolicy::Manual {
                bail!("external services must use service.lifecycle.startup: manual");
            }
            if cfg.service.lifecycle.restart != RestartPolicy::Never {
                bail!("external services must use service.lifecycle.restart: never");
            }
        }
    }
    Ok(())
}

pub fn service_cwd(cfg: &BridgeConfig) -> Option<&Path> {
    cfg.service.cwd.as_deref()
}

pub fn service_pid_path(cfg: &BridgeConfig) -> Option<PathBuf> {
    cfg.service
        .pid_file
        .as_ref()
        .map(|path| resolve_service_path(cfg, path))
}

pub fn service_log_path(cfg: &BridgeConfig) -> Option<PathBuf> {
    cfg.service
        .log_file
        .as_ref()
        .map(|path| resolve_service_path(cfg, path))
}

fn resolve_service_path(cfg: &BridgeConfig, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    cfg.service
        .cwd
        .as_ref()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|| path.to_path_buf())
}

pub fn open_url(cfg: &BridgeConfig) -> String {
    cfg.open_url
        .clone()
        .or_else(|| cfg.local_url.clone())
        .or_else(|| cfg.network_url.clone())
        .unwrap_or_else(|| format!("http://127.0.0.1:{}/", cfg.port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_reserved_port_range() {
        let cfg = BridgeConfig {
            schema: "portal-bridge.v1".into(),
            id: "x".into(),
            title: "x".into(),
            owner_host: "workstation".into(),
            port: 24001,
            service: ServiceConfig {
                mode: ServiceMode::Managed,
                lifecycle: LifecycleConfig::default(),
                cwd: Some("/tmp/x".into()),
                command: vec!["python3".into()],
                start_command: None,
                detach: None,
                stop_command: None,
                restart_command: None,
                task_name: None,
                pid_source: None,
                pid_port: None,
                pid_file: Some("server.pid".into()),
                pid: None,
                log_file: Some("server.log".into()),
                health_url: None,
                health_expect: HealthExpectConfig::default(),
                startup_timeout_sec: 10,
                notes: None,
            },
            tunnel: TunnelConfig::default(),
            local_url: None,
            network_url: None,
            open_url: None,
        };
        validate_bridge_config(&cfg).unwrap();
    }

    #[test]
    fn old_service_yaml_defaults_to_manual_lifecycle() {
        let text = r#"
schema: portal-bridge.v1
id: x
title: X
owner_host: workstation
port: 24001
service:
  mode: managed
  cwd: /tmp/x
  command: ["python3", "-m", "http.server", "24001"]
  pid_file: server.pid
  log_file: server.log
"#;
        let cfg: BridgeConfig = serde_yaml::from_str(text).unwrap();
        assert_eq!(cfg.service.lifecycle.startup, StartupPolicy::Manual);
        assert_eq!(cfg.service.lifecycle.restart, RestartPolicy::Never);
        validate_bridge_config(&cfg).unwrap();
    }

    #[test]
    fn external_services_cannot_autostart() {
        let cfg = BridgeConfig {
            schema: "portal-bridge.v1".into(),
            id: "x".into(),
            title: "x".into(),
            owner_host: "workstation".into(),
            port: 24001,
            service: ServiceConfig {
                mode: ServiceMode::External,
                lifecycle: LifecycleConfig {
                    startup: StartupPolicy::Autostart,
                    restart: RestartPolicy::Never,
                },
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
                health_expect: HealthExpectConfig::default(),
                startup_timeout_sec: 10,
                notes: None,
            },
            tunnel: TunnelConfig::default(),
            local_url: None,
            network_url: None,
            open_url: None,
        };
        assert!(validate_bridge_config(&cfg).is_err());
    }
}
