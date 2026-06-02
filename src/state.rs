use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct State {
    #[serde(default)]
    pub services: BTreeMap<String, ServiceState>,
    #[serde(default)]
    pub tunnels: BTreeMap<String, TunnelState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServiceState {
    pub pid: Option<u32>,
    pub last_health: Option<String>,
    pub last_status: Option<String>,
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desired: Option<DesiredState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_port: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredState {
    Running,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TunnelState {
    pub pid: Option<u32>,
    pub mode: String,
    pub local_port: u16,
    pub peer: String,
    pub updated_at: Option<String>,
}

impl State {
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
}
