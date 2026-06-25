use crate::config::{load_bridge_config, BridgeConfig, LifecycleConfig, ServiceMode, TunnelMode};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Registry {
    #[serde(default)]
    pub entries: BTreeMap<String, RegistryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub id: String,
    pub config_path: PathBuf,
    pub registered_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryExport {
    pub machine_id: String,
    pub services: Vec<ServiceExport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceExport {
    pub id: String,
    pub title: String,
    pub owner_host: String,
    pub port: u16,
    #[serde(default = "default_service_mode")]
    pub service_mode: ServiceMode,
    #[serde(default = "default_tunnel_modes")]
    pub tunnel_modes: Vec<TunnelMode>,
    #[serde(default = "default_bind_host")]
    pub bind_host: String,
    #[serde(default)]
    pub lifecycle: LifecycleConfig,
    #[serde(default)]
    pub runtime_status: Option<String>,
    #[serde(default)]
    pub recorded_pid: Option<u32>,
    #[serde(default)]
    pub pid_source: Option<String>,
    #[serde(default)]
    pub pid_port: Option<u16>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub detach: Option<String>,
    #[serde(default)]
    pub task_name: Option<String>,
    #[serde(default)]
    pub local_url: Option<String>,
    #[serde(default)]
    pub network_url: Option<String>,
    #[serde(default)]
    pub open_url: Option<String>,
    pub config_path: PathBuf,
}

fn default_service_mode() -> ServiceMode {
    ServiceMode::Managed
}

fn default_tunnel_modes() -> Vec<TunnelMode> {
    vec![TunnelMode::LocalForward]
}

fn default_bind_host() -> String {
    "127.0.0.1".to_string()
}

impl Registry {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        Ok(serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(self)? + "\n")?;
        fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn register(&mut self, config_path: PathBuf) -> Result<()> {
        let cfg = load_bridge_config(&config_path)?;
        self.entries.insert(
            cfg.id.clone(),
            RegistryEntry {
                id: cfg.id,
                config_path,
                registered_at: crate::time::now_iso(),
            },
        );
        Ok(())
    }

    pub fn unregister(&mut self, id: &str) -> Option<RegistryEntry> {
        self.entries.remove(id)
    }

    pub fn load_configs(&self) -> Result<Vec<(RegistryEntry, BridgeConfig)>> {
        let mut out = Vec::new();
        for entry in self.entries.values() {
            let cfg = load_bridge_config(&entry.config_path).with_context(|| {
                format!("load registered config {}", entry.config_path.display())
            })?;
            out.push((entry.clone(), cfg));
        }
        Ok(out)
    }

    pub fn get_config(&self, id: &str) -> Result<BridgeConfig> {
        let entry = self
            .entries
            .get(id)
            .with_context(|| format!("service `{id}` is not registered"))?;
        load_bridge_config(&entry.config_path)
    }

    pub fn try_get_config(&self, id: &str) -> Result<Option<BridgeConfig>> {
        let Some(entry) = self.entries.get(id) else {
            return Ok(None);
        };
        Ok(Some(load_bridge_config(&entry.config_path)?))
    }

    pub fn try_get_entry_config(&self, id: &str) -> Result<Option<(RegistryEntry, BridgeConfig)>> {
        let Some(entry) = self.entries.get(id) else {
            return Ok(None);
        };
        Ok(Some((
            entry.clone(),
            load_bridge_config(&entry.config_path)?,
        )))
    }

    pub fn export(&self, machine_id: &str) -> Result<RegistryExport> {
        self.export_with_runtime(machine_id, true)
    }

    pub fn export_with_runtime(
        &self,
        machine_id: &str,
        include_runtime: bool,
    ) -> Result<RegistryExport> {
        let mut services = Vec::new();
        for entry in self.entries.values() {
            let cfg = load_bridge_config(&entry.config_path)?;
            services.push(ServiceExport {
                id: cfg.id.clone(),
                title: cfg.title.clone(),
                owner_host: cfg.owner_host.clone(),
                port: cfg.port,
                service_mode: cfg.service.mode,
                tunnel_modes: cfg.tunnel.modes.clone(),
                bind_host: cfg.tunnel.bind_host.clone(),
                lifecycle: cfg.service.lifecycle.clone(),
                runtime_status: if include_runtime {
                    runtime_status(&cfg, machine_id)
                } else {
                    None
                },
                recorded_pid: cfg.service.pid,
                pid_source: cfg.service.pid_source.clone(),
                pid_port: cfg.service.pid_port,
                notes: cfg.service.notes.clone(),
                detach: cfg.service.detach.clone(),
                task_name: cfg.service.task_name.clone(),
                local_url: cfg.local_url.clone(),
                network_url: cfg.network_url.clone(),
                open_url: Some(crate::config::open_url(&cfg)),
                config_path: entry.config_path.clone(),
            });
        }
        Ok(RegistryExport {
            machine_id: machine_id.to_string(),
            services,
        })
    }
}

fn runtime_status(cfg: &BridgeConfig, machine_id: &str) -> Option<String> {
    if cfg.owner_host != machine_id {
        return None;
    }
    match cfg.service.mode {
        ServiceMode::Managed => Some(crate::process::managed_service_status(cfg)),
        ServiceMode::External => {
            if let Some(pid) = crate::process::service_listener_pid(cfg) {
                Some(format!("external-running:{pid}"))
            } else {
                match cfg.service.pid {
                    Some(pid) if crate::process::pid_alive(pid) => {
                        Some(format!("external-running:{pid}"))
                    }
                    Some(_) => Some("external-stopped".into()),
                    None => Some("external-record".into()),
                }
            }
        }
    }
}

pub fn validate_no_port_conflicts(exports: &[RegistryExport]) -> Result<()> {
    let mut by_owner_port: BTreeMap<(String, u16), (String, String, String)> = BTreeMap::new();
    let mut seen_same: BTreeSet<(String, u16, String)> = BTreeSet::new();
    for export in exports {
        for svc in &export.services {
            let owner = if svc.owner_host.trim().is_empty() {
                export.machine_id.clone()
            } else {
                svc.owner_host.clone()
            };
            let key = (owner.clone(), svc.port, svc.id.clone());
            if !seen_same.insert(key) {
                continue;
            }
            match by_owner_port.get(&(owner.clone(), svc.port)) {
                Some((id, machine, path)) if id != &svc.id => {
                    bail!(
                        "port {} conflict on owner `{}`: `{}` from {} ({}) vs `{}` from {} ({})",
                        svc.port,
                        owner,
                        id,
                        machine,
                        path,
                        svc.id,
                        export.machine_id,
                        svc.config_path.display()
                    );
                }
                Some(_) => {}
                None => {
                    by_owner_port.insert(
                        (owner, svc.port),
                        (
                            svc.id.clone(),
                            export.machine_id.clone(),
                            svc.config_path.display().to_string(),
                        ),
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn export(machine: &str, id: &str, port: u16) -> RegistryExport {
        export_with_owner(machine, machine, id, port)
    }

    fn export_with_owner(machine: &str, owner: &str, id: &str, port: u16) -> RegistryExport {
        RegistryExport {
            machine_id: machine.into(),
            services: vec![ServiceExport {
                id: id.into(),
                title: id.into(),
                owner_host: owner.into(),
                port,
                service_mode: ServiceMode::Managed,
                tunnel_modes: vec![TunnelMode::LocalForward],
                bind_host: "127.0.0.1".into(),
                lifecycle: LifecycleConfig::default(),
                runtime_status: None,
                recorded_pid: None,
                pid_source: None,
                pid_port: None,
                notes: None,
                detach: None,
                task_name: None,
                local_url: None,
                network_url: None,
                open_url: None,
                config_path: format!("/{id}.yaml").into(),
            }],
        }
    }

    #[test]
    fn same_owner_same_port_same_id_is_allowed() {
        validate_no_port_conflicts(&[
            export("workstation", "x", 24001),
            export_with_owner("workstation-cache", "workstation", "x", 24001),
        ])
        .unwrap();
    }

    #[test]
    fn same_port_across_different_owners_is_allowed() {
        validate_no_port_conflicts(&[
            export("workstation", "x", 24001),
            export("gpu-box", "y", 24001),
        ])
        .unwrap();
    }

    #[test]
    fn same_owner_same_port_different_id_is_rejected() {
        assert!(validate_no_port_conflicts(&[
            export("workstation", "x", 24001),
            export_with_owner("workstation-cache", "workstation", "y", 24001)
        ])
        .is_err());
    }

    #[test]
    fn old_peer_exports_default_new_fields() {
        let text = r#"{
          "machine_id": "gpu-box",
          "services": [{
            "id": "x",
            "title": "x",
            "owner_host": "gpu-box",
            "port": 24001,
            "config_path": "/tmp/x.yaml"
          }]
        }"#;
        let export: RegistryExport = serde_json::from_str(text).unwrap();
        assert_eq!(export.services[0].bind_host, "127.0.0.1");
        assert_eq!(export.services[0].service_mode, ServiceMode::Managed);
        assert_eq!(
            export.services[0].tunnel_modes,
            vec![TunnelMode::LocalForward]
        );
        assert!(export.services[0].open_url.is_none());
    }
}
