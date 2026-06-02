use anyhow::{Context, Result};
use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_file: PathBuf,
    pub registry_file: PathBuf,
    pub state_file: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let config_dir = dirs::config_dir()
            .context("could not determine config directory")?
            .join("bridgeboard");
        let data_dir = dirs::data_dir()
            .context("could not determine data directory")?
            .join("bridgeboard");
        let state_dir = dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .context("could not determine state directory")?
            .join("bridgeboard");
        Ok(Self {
            config_file: config_dir.join("config.yaml"),
            registry_file: data_dir.join("registry.json"),
            state_file: state_dir.join("state.json"),
        })
    }
}

pub fn machine_id(configured: Option<&str>) -> String {
    if let Some(value) = configured {
        if !value.trim().is_empty() {
            return value.trim().to_string();
        }
    }
    if let Ok(value) = env::var("BRIDGEBOARD_MACHINE") {
        if !value.trim().is_empty() {
            return value.trim().to_string();
        }
    }
    hostname()
}

fn hostname() -> String {
    if let Ok(output) = crate::command::quiet_command("hostname").output() {
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !text.is_empty() {
            return text;
        }
    }
    "unknown".to_string()
}
