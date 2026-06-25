use crate::core;
pub use crate::core::{BridgeEnv as DashboardEnv, PortRow};
use crate::peer;
use crate::registry::{validate_no_port_conflicts, Registry, RegistryExport};
use crate::state::State;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use url::Url;

pub fn serve(env: DashboardEnv, host: &str, port: u16, include_peers: bool) -> Result<()> {
    let addr = format!("{host}:{port}");
    let listener = TcpListener::bind(&addr).with_context(|| format!("bind dashboard on {addr}"))?;
    let token = dashboard_token()?;
    let runtime = DashboardRuntime::new(env, include_peers);
    runtime.refresh_exports_if_needed(Duration::ZERO);
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                if let Err(err) = handle_request(&runtime, &mut stream, &token) {
                    eprintln!("dashboard request failed: {err}");
                }
            }
            Err(err) => eprintln!("dashboard connection failed: {err}"),
        }
    }
    Ok(())
}

pub fn port_rows(env: &DashboardEnv, include_peers: bool) -> Result<Vec<PortRow>> {
    core::port_rows(env, include_peers)
}

struct DashboardRuntime {
    env: DashboardEnv,
    include_peers: bool,
    export_cache: ExportCache,
}

impl DashboardRuntime {
    fn new(env: DashboardEnv, include_peers: bool) -> Self {
        let cache_path = env.paths.state_file.with_file_name("dashboard-cache.json");
        Self {
            env,
            include_peers,
            export_cache: ExportCache::load(cache_path),
        }
    }

    fn port_rows(&self) -> Result<Vec<PortRow>> {
        let state = State::load(&self.env.paths.state_file)?;
        self.refresh_exports_if_needed(Duration::from_secs(30));
        let mut exports = self.export_cache.exports();
        if exports.is_empty() {
            exports = self.fast_local_exports()?;
        }
        if let Err(err) = validate_no_port_conflicts(&exports) {
            eprintln!("warning: dashboard port conflict: {err}");
        }
        Ok(core::port_rows_from_exports(exports, &self.env, &state))
    }

    fn fast_local_exports(&self) -> Result<Vec<RegistryExport>> {
        let registry = Registry::load(&self.env.paths.registry_file)?;
        Ok(vec![
            registry.export_with_runtime(&self.env.machine_id, false)?
        ])
    }

    fn refresh_exports_if_needed(&self, min_interval: Duration) {
        self.export_cache
            .refresh_if_needed(self.env.clone(), self.include_peers, min_interval);
    }
}

#[derive(Clone)]
struct ExportCache {
    path: PathBuf,
    inner: Arc<Mutex<ExportCacheState>>,
}

#[derive(Default)]
struct ExportCacheState {
    exports: Vec<RegistryExport>,
    warnings: Vec<String>,
    updated_at: Option<String>,
    last_refresh: Option<Instant>,
    refreshing: bool,
}

#[derive(Serialize, Deserialize)]
struct DiskExportCache {
    updated_at: String,
    exports: Vec<RegistryExport>,
    #[serde(default)]
    warnings: Vec<String>,
}

impl ExportCache {
    fn load(path: PathBuf) -> Self {
        let mut state = ExportCacheState::default();
        if let Ok(bytes) = fs::read(&path) {
            if let Ok(cache) = serde_json::from_slice::<DiskExportCache>(&bytes) {
                state.exports = cache.exports;
                state.warnings = cache.warnings;
                state.updated_at = Some(cache.updated_at);
            }
        }
        Self {
            path,
            inner: Arc::new(Mutex::new(state)),
        }
    }

    fn exports(&self) -> Vec<RegistryExport> {
        self.inner
            .lock()
            .map(|state| state.exports.clone())
            .unwrap_or_default()
    }

    fn refresh_if_needed(&self, env: DashboardEnv, include_peers: bool, min_interval: Duration) {
        {
            let Ok(mut state) = self.inner.lock() else {
                return;
            };
            if state.refreshing {
                return;
            }
            if state
                .last_refresh
                .map(|last| last.elapsed() < min_interval)
                .unwrap_or(false)
            {
                return;
            }
            state.refreshing = true;
            state.last_refresh = Some(Instant::now());
        }

        let inner = Arc::clone(&self.inner);
        let path = self.path.clone();
        std::thread::spawn(move || {
            let mut exports = Vec::new();
            let mut warnings = Vec::new();
            match Registry::load(&env.paths.registry_file) {
                Ok(registry) => match registry.export(&env.machine_id) {
                    Ok(export) => exports.push(export),
                    Err(err) => {
                        warnings.push(format!("local runtime export: {err}"));
                        match registry.export_with_runtime(&env.machine_id, false) {
                            Ok(export) => exports.push(export),
                            Err(fallback_err) => {
                                warnings.push(format!("local config export: {fallback_err}"))
                            }
                        }
                    }
                },
                Err(err) => warnings.push(format!("local registry: {err}")),
            }
            if include_peers {
                for (name, result) in peer::fetch_peer_exports(&env.app) {
                    match result {
                        Ok(export) => exports.push(export),
                        Err(err) => warnings.push(format!("peer {name}: {err}")),
                    }
                }
            }
            for warning in &warnings {
                eprintln!("warning: dashboard export refresh failed for {warning}");
            }

            let Ok(mut state) = inner.lock() else {
                return;
            };
            state.refreshing = false;
            state.last_refresh = Some(Instant::now());
            state.warnings = warnings;
            if exports.is_empty() && !state.exports.is_empty() {
                return;
            }
            let updated_at = crate::time::now_iso();
            state.exports = exports;
            state.updated_at = Some(updated_at.clone());
            let disk = DiskExportCache {
                updated_at,
                exports: state.exports.clone(),
                warnings: state.warnings.clone(),
            };
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(text) = serde_json::to_string_pretty(&disk) {
                let _ = fs::write(&path, text + "\n");
            }
        });
    }
}

fn handle_request(runtime: &DashboardRuntime, stream: &mut TcpStream, token: &str) -> Result<()> {
    let mut buffer = [0_u8; 4096];
    let n = stream.read(&mut buffer)?;
    let request = String::from_utf8_lossy(&buffer[..n]);
    let first_line = request.lines().next().unwrap_or_default();
    let mut first_parts = first_line.split_whitespace();
    let method = first_parts.next().unwrap_or_default();
    let target = first_parts.next().unwrap_or("/");
    let url = Url::parse(&format!("http://bridgeboard.local{target}"))
        .with_context(|| format!("parse request target {target}"))?;
    match url.path() {
        "/" | "/index.html" => respond_html(stream, &dashboard_html(token))?,
        "/api/ports" => {
            let rows = runtime.port_rows()?;
            respond_json(stream, &serde_json::to_string_pretty(&rows)?)?;
        }
        "/api/agent-prompt" => {
            let machine =
                query_value(&url, "machine").unwrap_or_else(|| runtime.env.machine_id.clone());
            respond_json(
                stream,
                &serde_json::to_string_pretty(&json!({
                    "ok": true,
                    "machine_id": machine,
                    "prompt": agent_prompt_for(&machine),
                }))?,
            )?;
        }
        "/api/agent-prompts" => {
            respond_json(
                stream,
                &serde_json::to_string_pretty(&json!({
                    "ok": true,
                    "prompts": agent_prompt_entries(&runtime.env),
                }))?,
            )?;
        }
        "/api/action" => {
            if !require_post_token(stream, method, &request, token)? {
                return Ok(());
            }
            let id = query_value(&url, "id").context("missing id")?;
            let action = query_value(&url, "action").context("missing action")?;
            let title = query_value(&url, "title");
            match run_action(&runtime.env, &id, &action, title.as_deref()) {
                Ok(message) => respond_json(
                    stream,
                    &serde_json::to_string_pretty(&json!({
                        "ok": true,
                        "message": message,
                    }))?,
                )?,
                Err(err) => respond_json_status(
                    stream,
                    "500 Internal Server Error",
                    &serde_json::to_string_pretty(&json!({
                        "ok": false,
                        "message": err.to_string(),
                    }))?,
                )?,
            }
        }
        "/api/open-dashboard" => {
            if !require_post_token(stream, method, &request, token)? {
                return Ok(());
            }
            let dashboard_url = format!("http://{}/", stream.local_addr()?);
            webbrowser::open(&dashboard_url).with_context(|| format!("open {dashboard_url}"))?;
            respond_json(
                stream,
                &serde_json::to_string_pretty(&json!({
                    "ok": true,
                    "message": format!("opened {dashboard_url}"),
                }))?,
            )?;
        }
        "/api/open-url" => {
            if !require_post_token(stream, method, &request, token)? {
                return Ok(());
            }
            let id = query_value(&url, "id").unwrap_or_default();
            let target_url = query_value(&url, "url").context("missing url")?;
            match open_direct_url(&target_url, &id) {
                Ok(message) => respond_json(
                    stream,
                    &serde_json::to_string_pretty(&json!({
                        "ok": true,
                        "message": message,
                    }))?,
                )?,
                Err(err) => respond_json_status(
                    stream,
                    "500 Internal Server Error",
                    &serde_json::to_string_pretty(&json!({
                        "ok": false,
                        "message": err.to_string(),
                    }))?,
                )?,
            }
        }
        "/health" => respond_text(stream, "ok\n")?,
        _ => respond_not_found(stream)?,
    }
    Ok(())
}

fn require_post_token(
    stream: &mut TcpStream,
    method: &str,
    request: &str,
    token: &str,
) -> Result<bool> {
    if method != "POST" {
        respond(
            stream,
            "405 Method Not Allowed",
            "application/json; charset=utf-8",
            r#"{"ok":false,"message":"method not allowed"}"#,
        )?;
        return Ok(false);
    }
    if header_value(request, "x-bridgeboard-token").as_deref() != Some(token) {
        respond(
            stream,
            "403 Forbidden",
            "application/json; charset=utf-8",
            r#"{"ok":false,"message":"forbidden"}"#,
        )?;
        return Ok(false);
    }
    Ok(true)
}

fn header_value(request: &str, name: &str) -> Option<String> {
    request.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_string())
    })
}

fn query_value(url: &Url, key: &str) -> Option<String> {
    url.query_pairs()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| value.into_owned())
}

fn agent_prompt_for(machine_id: &str) -> String {
    format!(
        "Use Bridgeboard handoff / 服务交接 for any background web service on `{}`: run `bridgeboard ports --peers`, choose a fixed free 24xxx port, start the service, then register it with `bridgeboard handoff --id <id> --title \"<title>\" --port <port> --owner-host {} --pid-from-port --health-url http://127.0.0.1:<port>/ --require-healthy`.",
        machine_id, machine_id
    )
}

fn agent_prompt_entries(env: &DashboardEnv) -> Vec<serde_json::Value> {
    let mut entries = Vec::with_capacity(env.app.peers.len() + 1);
    entries.push(agent_prompt_entry(&env.machine_id, true));
    for machine_id in env.app.peers.keys() {
        if machine_id != &env.machine_id {
            entries.push(agent_prompt_entry(machine_id, false));
        }
    }
    entries
}

fn agent_prompt_entry(machine_id: &str, local: bool) -> serde_json::Value {
    json!({
        "machine_id": machine_id,
        "label": format!("Copy {machine_id}"),
        "local": local,
        "prompt": agent_prompt_for(machine_id),
    })
}

fn run_action(env: &DashboardEnv, id: &str, action: &str, title: Option<&str>) -> Result<String> {
    if action == "open" {
        let url = core::open(env, id)?;
        return Ok(format!("opened {url}"));
    }
    if action == "rename" {
        let title = title.context("missing title")?;
        let lines = core::rename_title(env, id, title)?;
        return Ok(lines.join("\n"));
    }

    let lines = match action {
        "up" => core::up(env, id)?,
        "remote-up" => core::remote_up(env, id)?,
        "remote-down" => core::remote_down(env, id)?,
        "remote-restart" => core::remote_restart(env, id)?,
        "down" | "stop" => core::down(env, id)?,
        "restart" => core::restart(env, id)?,
        other => bail!("unsupported action `{other}`"),
    };
    Ok(if lines.is_empty() {
        format!("{action} {id} done")
    } else {
        lines.join("\n")
    })
}

fn open_direct_url(raw_url: &str, id: &str) -> Result<String> {
    let target_url =
        Url::parse(raw_url).with_context(|| format!("parse direct open URL {raw_url}"))?;
    match target_url.scheme() {
        "http" | "https" => {}
        scheme => bail!("refusing to open unsupported URL scheme `{scheme}`"),
    }
    webbrowser::open(target_url.as_str()).with_context(|| format!("open {target_url}"))?;
    if id.is_empty() {
        Ok(format!("opened {}", target_url.as_str()))
    } else {
        Ok(format!("opened {id}: {}", target_url.as_str()))
    }
}

fn respond_html(stream: &mut TcpStream, body: &str) -> Result<()> {
    respond(stream, "200 OK", "text/html; charset=utf-8", body)
}

fn respond_json(stream: &mut TcpStream, body: &str) -> Result<()> {
    respond(stream, "200 OK", "application/json; charset=utf-8", body)
}

fn respond_json_status(stream: &mut TcpStream, status: &str, body: &str) -> Result<()> {
    respond(stream, status, "application/json; charset=utf-8", body)
}

fn respond_text(stream: &mut TcpStream, body: &str) -> Result<()> {
    respond(stream, "200 OK", "text/plain; charset=utf-8", body)
}

fn respond_not_found(stream: &mut TcpStream) -> Result<()> {
    respond(
        stream,
        "404 Not Found",
        "text/plain; charset=utf-8",
        "not found\n",
    )
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &str) -> Result<()> {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    Ok(())
}

pub fn dashboard_html(token: &str) -> String {
    DASHBOARD_HTML.replace("__BRIDGEBOARD_TOKEN__", token)
}

fn dashboard_token() -> Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|err| anyhow::anyhow!("generate dashboard token: {err}"))?;
    Ok(hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

const DASHBOARD_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Bridgeboard</title>
  <style>
    :root {
      color-scheme: light dark;
      font-family: Inter, Segoe UI, system-ui, sans-serif;
      --bg: #f4f6f7;
      --panel: #ffffff;
      --panel-2: #f9fbfc;
      --line: #d8e0e6;
      --text: #18212b;
      --muted: #66727f;
      --soft: #eef3f5;
      --accent: #0f766e;
      --accent-2: #2563eb;
      --ok: #15803d;
      --warn: #b45c08;
      --bad: #dc2626;
      --remote: #0e7490;
    }
    * { box-sizing: border-box; }
    html, body { height: 100%; }
    body { margin: 0; background: var(--bg); color: var(--text); overflow: hidden; }
    button {
      border: 1px solid #a9b4be;
      background: var(--panel);
      color: var(--text);
      border-radius: 6px;
      padding: 7px 10px;
      cursor: pointer;
      font-size: 13px;
      line-height: 1.15;
      min-height: 32px;
    }
    button.primary { background: var(--accent); color: #fff; border-color: var(--accent); }
    button.secondary { background: #eaf3ff; color: #17458f; border-color: #b9d4fb; }
    button.warn { background: #fff7ed; color: #8a3d00; border-color: #f0b26d; }
    button:disabled { opacity: .55; cursor: default; }
    input {
      border: 1px solid #a9b4be;
      background: var(--panel);
      color: var(--text);
      border-radius: 6px;
      padding: 7px 10px;
      min-height: 32px;
      font-size: 13px;
      outline: none;
    }
    input:focus { border-color: var(--accent); box-shadow: 0 0 0 2px rgba(15,118,110,.14); }
    select {
      border: 1px solid #a9b4be;
      background: var(--panel);
      color: var(--text);
      border-radius: 6px;
      padding: 7px 9px;
      min-height: 32px;
      font-size: 13px;
      outline: none;
    }
    a { color: var(--accent-2); text-decoration: none; }
    a:hover { text-decoration: underline; }
    button.linklike {
      border: 0;
      background: transparent;
      color: var(--accent-2);
      padding: 0;
      min-height: 0;
      max-width: 100%;
      text-align: left;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    button.linklike:hover { text-decoration: underline; }
    .shell { height: 100vh; min-height: 0; display: grid; grid-template-columns: 214px minmax(0, 1fr); }
    .sidebar {
      border-right: 1px solid var(--line);
      background: #fbfcfd;
      padding: 18px 14px;
      display: flex;
      flex-direction: column;
      gap: 18px;
      min-height: 0;
    }
    .brand { display: flex; align-items: center; gap: 10px; padding: 0 8px; }
    .mark { width: 30px; height: 30px; border-radius: 7px; background: linear-gradient(135deg, #0f766e, #2563eb); }
    h1 { font-size: 18px; margin: 0; font-weight: 700; letter-spacing: 0; }
    .machine { color: var(--muted); font-size: 12px; margin-top: 2px; }
    .nav { display: grid; gap: 4px; }
    .nav button { text-align: left; border: 0; background: transparent; padding: 9px 10px; color: var(--muted); }
    .nav button.active { background: #e8f2f1; color: #0f4f49; font-weight: 650; }
    .sidebar-footer { margin-top: auto; display: grid; gap: 8px; }
    .content { min-width: 0; min-height: 0; padding: 18px 22px 24px; display: flex; flex-direction: column; }
    .topbar { flex: 0 0 auto; display: flex; justify-content: space-between; align-items: flex-start; gap: 16px; margin-bottom: 16px; }
    .title-block h2 { font-size: 22px; margin: 0 0 4px; letter-spacing: 0; }
    .subtle { color: var(--muted); font-size: 13px; }
    .toolbar { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; justify-content: flex-end; }
    .prompt-buttons { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
    .filter { width: min(260px, 34vw); }
    .summary { flex: 0 0 auto; display: grid; grid-template-columns: repeat(4, minmax(132px, 1fr)); gap: 10px; margin-bottom: 14px; }
    .metric { background: var(--panel); border: 1px solid var(--line); border-radius: 8px; padding: 11px 12px; min-height: 68px; }
    .metric .label { color: var(--muted); font-size: 12px; margin-bottom: 5px; }
    .metric .value { font-size: 20px; font-weight: 700; }
    .metric .value.small { font-size: 15px; padding-top: 3px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .statusline { flex: 0 0 auto; min-height: 22px; color: var(--muted); font-size: 13px; margin: 0 0 12px; }
    .view-pane { flex: 1 1 auto; min-height: 0; overflow: auto; padding-right: 4px; }
    .app-grid {
      display: grid;
      grid-template-columns: repeat(auto-fill, minmax(258px, 1fr));
      gap: 10px;
      align-items: stretch;
    }
    .app-card {
      background: var(--panel);
      border: 1px solid var(--line);
      border-radius: 8px;
      padding: 13px;
      min-height: 196px;
      display: grid;
      grid-template-rows: auto 1fr auto;
      gap: 12px;
    }
    .app-card:hover { border-color: #9ab2c4; }
    .app-head { display: grid; grid-template-columns: 42px minmax(0, 1fr) auto; gap: 10px; align-items: center; }
    .app-icon {
      width: 42px;
      height: 42px;
      border-radius: 8px;
      background: #e6f1ef;
      color: #0f4f49;
      border: 1px solid #c1d8d3;
      display: inline-flex;
      align-items: center;
      justify-content: center;
      font-weight: 800;
      font-size: 13px;
      letter-spacing: 0;
    }
    .app-name { min-width: 0; }
    .app-name strong { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .app-name span { display: block; color: var(--muted); font-size: 12px; margin-top: 3px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .app-body { min-width: 0; display: grid; align-content: start; gap: 9px; }
    .app-url { min-width: 0; font-size: 12px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--muted); }
    .app-actions { display: grid; grid-template-columns: 1fr 1fr; gap: 6px; }
    .app-actions .wide { grid-column: 1 / -1; }
    .app-actions button { width: 100%; }
    .service-list { display: grid; gap: 8px; }
    .service-row {
      background: var(--panel);
      border: 1px solid var(--line);
      border-radius: 8px;
      padding: 12px;
      display: grid;
      grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) minmax(208px, 240px);
      gap: 14px;
      align-items: stretch;
    }
    .service-main { min-width: 0; display: grid; align-content: center; }
    .service-title { display: flex; align-items: center; gap: 8px; min-width: 0; }
    .service-title strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .pin-button {
      flex: 0 0 auto;
      width: 28px;
      min-width: 28px;
      padding: 0;
      font-size: 15px;
      line-height: 1;
      display: inline-flex;
      align-items: center;
      justify-content: center;
    }
    .pin-button.active { background: #fff3c4; color: #8a5b00; border-color: #e0b43c; }
    .service-meta { color: var(--muted); font-size: 12px; margin-top: 5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .port-badge {
      flex: 0 0 auto;
      border: 1px solid var(--line);
      border-radius: 6px;
      color: var(--muted);
      background: var(--panel-2);
      padding: 3px 6px;
      font-size: 12px;
      font-variant-numeric: tabular-nums;
    }
    .state-badge {
      flex: 0 0 auto;
      border-radius: 999px;
      padding: 5px 9px;
      font-size: 12px;
      font-weight: 750;
      line-height: 1;
    }
    .state-badge.ok { background: #dff5e8; color: var(--ok); }
    .state-badge.warn { background: #fff0cf; color: var(--warn); }
    .state-badge.bad { background: #fee2e2; color: var(--bad); }
    .state-badge.muted { background: var(--soft); color: var(--muted); }
    .chips { display: flex; gap: 6px; flex-wrap: wrap; align-content: center; align-items: center; }
    .chip { border-radius: 999px; padding: 4px 8px; font-size: 12px; font-weight: 650; background: var(--soft); color: #46515c; }
    .chip.ok { background: #e8f7ee; color: var(--ok); }
    .chip.warn { background: #fff2dc; color: var(--warn); }
    .chip.bad { background: #fee2e2; color: var(--bad); }
    .chip.remote { background: #e1f5f8; color: var(--remote); }
    .chip.muted { color: var(--muted); }
    .urlbox {
      min-width: 0;
      font-size: 13px;
      margin-top: 8px;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
      display: flex;
      align-items: center;
    }
    .actions {
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: 6px;
      align-content: center;
      min-width: 208px;
    }
    .actions button { width: 100%; }
    .actions .wide { grid-column: 1 / -1; }
    .actions .primary-action { min-height: 36px; font-weight: 750; }
    .empty { padding: 28px; background: var(--panel); border: 1px solid var(--line); border-radius: 8px; color: var(--muted); }
    .ports-table { background: var(--panel); border: 1px solid var(--line); border-radius: 8px; overflow: auto; }
    table { width: 100%; border-collapse: collapse; min-width: 980px; }
    th, td { text-align: left; padding: 9px 10px; border-bottom: 1px solid #e5e8eb; font-size: 13px; white-space: nowrap; }
    th { background: var(--panel-2); font-size: 11px; text-transform: uppercase; color: var(--muted); }
    tr:last-child td { border-bottom: 0; }
    .toast {
      position: fixed; left: 236px; bottom: 18px; max-width: min(560px, calc(100vw - 260px));
      background: #17212b; color: #f8fafc; padding: 12px 14px; border-radius: 8px;
      box-shadow: 0 12px 28px rgba(0,0,0,.22); white-space: pre-wrap; z-index: 20;
      pointer-events: none;
    }
    .hidden { display: none; }
    @media (max-width: 980px) {
      .shell { grid-template-columns: 1fr; }
      .sidebar { border-right: 0; border-bottom: 1px solid var(--line); flex-direction: row; align-items: center; }
      .nav { display: flex; }
      .sidebar-footer { margin-top: 0; margin-left: auto; }
      .summary { grid-template-columns: repeat(2, minmax(130px, 1fr)); }
      .app-grid { grid-template-columns: 1fr; }
      .service-row { grid-template-columns: 1fr; }
      .actions { width: 100%; }
      .toast { left: 18px; max-width: calc(100vw - 36px); }
    }
    @media (prefers-color-scheme: dark) {
      :root {
        --bg: #11161a; --panel: #171d22; --panel-2: #1d252b; --line: #303a42;
        --text: #e6eaee; --muted: #a6b0ba; --soft: #232d34;
      }
      .sidebar { background: #141a1f; }
      button { background: #1c242a; color: var(--text); border-color: #3a4650; }
      button.secondary { background: #17243a; color: #bcd5ff; border-color: #2f4b75; }
      button.warn { background: #2a1f17; color: #ffd6a8; border-color: #9a5a20; }
      select { background: #1c242a; color: var(--text); border-color: #3a4650; }
      .nav button.active { background: #15302d; color: #a8f0e7; }
      th, td { border-bottom-color: #303a42; }
      .state-badge.ok { background: #143323; color: #86efac; }
      .state-badge.warn { background: #362411; color: #f7c26f; }
      .state-badge.bad { background: #3a1717; color: #fca5a5; }
      .chip { background: #232d34; color: #c5ced6; }
      .chip.ok { background: #143323; color: #86efac; }
      .chip.warn { background: #362411; color: #f7c26f; }
      .chip.bad { background: #3a1717; color: #fca5a5; }
      .chip.remote { background: #12313a; color: #8de7f8; }
      .pin-button.active { background: #392d12; color: #f7cc62; border-color: #8a681f; }
      .app-icon { background: #15302d; color: #a8f0e7; border-color: #2e5d57; }
    }
  </style>
</head>
<body>
  <script>window.__bridgeboardToken = "__BRIDGEBOARD_TOKEN__";</script>
  <div class="shell">
    <aside class="sidebar">
      <div class="brand">
        <div class="mark"></div>
        <div>
          <h1>Bridgeboard</h1>
          <div class="machine" id="machine">loading</div>
        </div>
      </div>
      <nav class="nav">
        <button id="nav-apps" class="active" onclick="setView('apps')">Apps</button>
        <button id="nav-services" onclick="setView('services')">Services</button>
        <button id="nav-ports" onclick="setView('ports')">Ports</button>
      </nav>
      <div class="sidebar-footer">
        <button onclick="loadPorts()">Refresh</button>
      </div>
    </aside>
    <main class="content">
      <div class="topbar">
        <div class="title-block">
          <h2 id="view-title">Apps</h2>
          <div class="subtle" id="updated">Loading service registry</div>
        </div>
        <div class="toolbar">
          <input id="filter" class="filter" placeholder="Search" oninput="setFilter(this.value)">
          <select id="sort-mode" onchange="setSortMode(this.value)" title="Sort services">
            <option value="default">Default</option>
            <option value="status">Status</option>
            <option value="port">Port</option>
            <option value="name">Name</option>
            <option value="owner">Owner</option>
          </select>
          <button class="secondary" onclick="openDashboard()">Open Browser</button>
          <span id="prompt-buttons" class="prompt-buttons">
            <button class="secondary" onclick="copyAgentPrompt()">Copy Agent Prompt</button>
          </span>
          <button onclick="loadPorts()">Refresh</button>
        </div>
      </div>
      <section class="summary" id="summary"></section>
      <p class="statusline" id="status">Loading services...</p>
      <section id="apps-view" class="view-pane"></section>
      <section id="services-view" class="view-pane"></section>
      <section id="ports-view" class="view-pane hidden"></section>
    </main>
  </div>
  <script>
    let currentRows = [];
    let currentView = 'apps';
    let busyKey = '';
    let loadingPorts = false;
    let lastFocusRefresh = 0;
    let filterText = '';
    const pinnedKey = 'bridgeboard:pinned-service-ids';
    const sortKey = 'bridgeboard:service-sort-mode';
    let pinnedIds = loadPinnedIds();
    let sortMode = localStorage.getItem(sortKey) || 'default';

    async function loadPorts() {
      if (loadingPorts) return;
      loadingPorts = true;
      const status = document.getElementById('status');
      status.textContent = 'Loading services...';
      try {
        const response = await fetch('/api/ports', { cache: 'no-store' });
        if (!response.ok) throw new Error('HTTP ' + response.status);
        currentRows = await response.json();
        render();
      } catch (err) {
        status.textContent = 'Failed to load services: ' + err;
      } finally {
        loadingPorts = false;
      }
    }

    function setView(view) {
      currentView = view;
      document.getElementById('nav-apps').classList.toggle('active', view === 'apps');
      document.getElementById('nav-services').classList.toggle('active', view === 'services');
      document.getElementById('nav-ports').classList.toggle('active', view === 'ports');
      document.getElementById('view-title').textContent = viewTitle(view);
      render();
    }

    function viewTitle(view) {
      if (view === 'apps') return 'Apps';
      if (view === 'services') return 'Services';
      return 'Ports';
    }

    function setFilter(value) {
      filterText = String(value || '').trim().toLowerCase();
      render();
    }

    function setSortMode(value) {
      sortMode = value || 'default';
      localStorage.setItem(sortKey, sortMode);
      render();
    }

    function render() {
      const rows = currentRows;
      const visibleRows = filterText ? rows.filter(matchesFilter) : rows;
      const sortedRows = sortRows(visibleRows);
      const local = rows.find(row => row.local_machine_id)?.local_machine_id || 'local';
      const peers = [...new Set(rows.map(row => row.source_machine).filter(machine => machine && machine !== local))];
      const running = rows.filter(row => serviceState(row.runtime_status).key === 'running').length;
      const stale = rows.filter(row => serviceState(row.runtime_status).key === 'stale').length;
      document.getElementById('machine').textContent = local;
      const sortSelect = document.getElementById('sort-mode');
      if (sortSelect) sortSelect.value = sortMode;
      document.getElementById('updated').textContent = 'Updated ' + new Date().toLocaleTimeString();
      document.getElementById('status').textContent = visibleRows.length + ' shown / ' + rows.length + ' service record(s), ' + peers.length + ' peer source(s), ledger OK';
      document.getElementById('summary').innerHTML = `
        ${metric(currentView === 'apps' ? 'Apps' : 'Services', rows.length)}
        ${metric('Running', running)}
        ${metric('Stale', stale)}
        ${metric('Peers', peers.length ? peers.join(', ') : 'none', true)}
      `;
      document.getElementById('apps-view').classList.toggle('hidden', currentView !== 'apps');
      document.getElementById('services-view').classList.toggle('hidden', currentView !== 'services');
      document.getElementById('ports-view').classList.toggle('hidden', currentView !== 'ports');
      renderApps(sortedRows);
      renderServices(sortedRows);
      renderPorts(sortedRows);
    }

    function matchesFilter(row) {
      const text = [row.id, row.title, row.owner_host, row.source_machine, row.port, row.runtime_status, row.service_mode, row.tunnel_modes, row.url]
        .filter(Boolean)
        .join(' ')
        .toLowerCase();
      return text.includes(filterText);
    }

    function metric(label, value, small = false) {
      return `<div class="metric"><div class="label">${escapeHtml(label)}</div><div class="value ${small ? 'small' : ''}">${escapeHtml(value)}</div></div>`;
    }

    function sortRows(rows) {
      return rows
        .map((row, index) => ({ row, index }))
        .sort((a, b) => comparePinned(a.row, b.row) || compareByMode(a.row, b.row) || a.index - b.index)
        .map(item => item.row);
    }

    function comparePinned(a, b) {
      return Number(pinnedIds.has(b.id)) - Number(pinnedIds.has(a.id));
    }

    function compareByMode(a, b) {
      if (sortMode === 'port') return Number(a.port || 0) - Number(b.port || 0);
      if (sortMode === 'name') return compareText(a.title || a.id, b.title || b.id);
      if (sortMode === 'owner') return compareText(a.owner_host, b.owner_host) || compareText(a.title || a.id, b.title || b.id);
      if (sortMode === 'status') return statusRank(a.runtime_status) - statusRank(b.runtime_status) || compareText(a.title || a.id, b.title || b.id);
      return 0;
    }

    function compareText(a, b) {
      return String(a || '').localeCompare(String(b || ''), undefined, { numeric: true, sensitivity: 'base' });
    }

    function statusRank(status) {
      const state = serviceState(status).key;
      if (state === 'running') return 0;
      if (state === 'unhealthy' || state === 'error') return 1;
      if (state === 'stale') return 2;
      if (state === 'stopped') return 3;
      if (state === 'recorded') return 4;
      return 5;
    }

    function loadPinnedIds() {
      try {
        const raw = JSON.parse(localStorage.getItem(pinnedKey) || '[]');
        return new Set(Array.isArray(raw) ? raw.map(String) : []);
      } catch (_err) {
        return new Set();
      }
    }

    function savePinnedIds() {
      localStorage.setItem(pinnedKey, JSON.stringify([...pinnedIds]));
    }

    function togglePin(id) {
      if (pinnedIds.has(id)) {
        pinnedIds.delete(id);
        showToast('Unpinned ' + id);
      } else {
        pinnedIds.add(id);
        showToast('Pinned ' + id);
      }
      savePinnedIds();
      render();
    }

    function renderApps(rows) {
      const target = document.getElementById('apps-view');
      if (!rows.length) {
        target.innerHTML = '<div class="empty">No apps recorded.</div>';
        return;
      }
      target.innerHTML = `<div class="app-grid">${rows.map(row => {
        const remote = isRemote(row);
        const state = serviceState(row.runtime_status);
        const pinned = pinnedIds.has(row.id);
        const primary = appPrimary(row);
        const secondary = appSecondary(row);
        const busy = busyKey === row.id + ':' + primary.action;
        const secondaryBusy = busyKey === row.id + ':' + secondary.action;
        const restartAction = remote ? 'remote-restart' : 'restart';
        const restartBusy = busyKey === row.id + ':' + restartAction;
        return `
          <article class="app-card">
            <div class="app-head">
              <button class="app-icon" onclick="runAppPrimary('${escapeAttr(row.id)}')" title="Open ${escapeAttr(row.title || row.id)}">${escapeHtml(appInitials(row))}</button>
              <div class="app-name">
                <strong>${escapeHtml(row.title || row.id)}</strong>
                <span>${escapeHtml(appSubtitle(row))}</span>
              </div>
              <button class="pin-button ${pinned ? 'active' : ''}" title="${pinned ? 'Unpin app' : 'Pin app'}" onclick="togglePin('${escapeAttr(row.id)}')">${pinned ? '★' : '☆'}</button>
            </div>
            <div class="app-body">
              <div class="chips">
                ${chip(state.label, state.kind)}
                ${chip(remote ? 'remote' : 'local', remote ? 'remote' : 'ok')}
                ${chip(':' + row.port, 'muted')}
                ${row.startup_policy ? chip(row.startup_policy, 'muted') : ''}
              </div>
              <div class="app-url">${escapeHtml(row.url || '')}</div>
            </div>
            <div class="app-actions">
              <button class="${primary.className} primary-action wide" ${busy ? 'disabled' : ''} onclick="runAppPrimary('${escapeAttr(row.id)}')">${busy ? 'Working' : primary.label}</button>
              <button class="${secondary.className}" ${secondaryBusy ? 'disabled' : ''} onclick="runAppSecondary('${escapeAttr(row.id)}')">${secondaryBusy ? 'Working' : secondary.label}</button>
              <button ${restartBusy ? 'disabled' : ''} onclick="runAction('${escapeAttr(row.id)}', '${restartAction}')">${restartBusy ? 'Working' : 'Restart'}</button>
            </div>
          </article>`;
      }).join('')}</div>`;
    }

    function appPrimary(row) {
      const state = serviceState(row.runtime_status);
      if (state.key === 'running') return { action: 'open', label: 'Open', className: 'primary' };
      if (row.startup_policy === 'on_demand') {
        return { action: isRemote(row) ? 'remote-up-open' : 'open', label: 'Start & Open', className: 'primary' };
      }
      return { action: isRemote(row) ? 'remote-up' : 'up', label: 'Start', className: 'primary' };
    }

    function appSecondary(row) {
      const state = serviceState(row.runtime_status);
      if (state.key === 'running') return { action: isRemote(row) ? 'remote-down' : 'down', label: 'Stop', className: 'warn' };
      return { action: 'open', label: 'Open', className: 'secondary' };
    }

    function runAppPrimary(id) {
      const row = currentRows.find(candidate => candidate.id === id);
      if (!row) return;
      const primary = appPrimary(row);
      if (primary.action === 'open') {
        openService(id);
      } else if (primary.action === 'remote-up-open') {
        runActionThenOpen(id, 'remote-up');
      } else {
        runAction(id, primary.action);
      }
    }

    function runAppSecondary(id) {
      const row = currentRows.find(candidate => candidate.id === id);
      if (!row) return;
      const secondary = appSecondary(row);
      if (secondary.action === 'open') {
        openService(id);
      } else {
        runAction(id, secondary.action);
      }
    }

    function appSubtitle(row) {
      const remote = isRemote(row);
      const location = remote ? `${row.owner_host} via ${row.source_machine}` : row.owner_host;
      return `${row.id} - ${location}`;
    }

    function appInitials(row) {
      const source = String(row.title || row.id || 'app').trim();
      const parts = source.split(/[\s._-]+/).filter(Boolean);
      const letters = parts.length > 1
        ? parts.slice(0, 2).map(part => part[0]).join('')
        : source.slice(0, 2);
      return letters.toUpperCase();
    }

    function renderServices(rows) {
      const target = document.getElementById('services-view');
      if (!rows.length) {
        target.innerHTML = '<div class="empty">No services recorded.</div>';
        return;
      }
      target.innerHTML = `<div class="service-list">${rows.map(row => {
        const remote = isRemote(row);
        const state = serviceState(row.runtime_status);
        const running = state.key === 'running';
        const primaryAction = running ? (remote ? 'remote-down' : 'down') : (remote ? 'remote-up' : 'up');
        const restartAction = remote ? 'remote-restart' : 'restart';
        const primaryLabel = running ? 'Stop' : 'Start';
        const primaryClass = running ? 'warn' : 'primary';
        const primaryBusy = busyKey === row.id + ':' + primaryAction;
        const restartBusy = busyKey === row.id + ':' + restartAction;
        const scope = remote ? `owner ${row.owner_host}` : 'local owner';
        const access = remote ? (String(row.tunnel_modes || '').includes('local') ? 'ssh local' : 'network') : 'local';
        const pinned = pinnedIds.has(row.id);
        return `
          <article class="service-row">
            <div class="service-main">
              <div class="service-title">
                <button class="pin-button ${pinned ? 'active' : ''}" title="${pinned ? 'Unpin service' : 'Pin service'}" onclick="togglePin('${escapeAttr(row.id)}')">${pinned ? '★' : '☆'}</button>
                <span class="state-badge ${escapeAttr(state.kind)}">${escapeHtml(state.label)}</span>
                <span class="port-badge">:${escapeHtml(row.port)}</span>
                <strong>${escapeHtml(row.title || row.id)}</strong>
              </div>
              <div class="service-meta">${escapeHtml(row.id)} - owner ${escapeHtml(row.owner_host)} - source ${escapeHtml(row.source_machine)}</div>
              <div class="urlbox"><button class="linklike" onclick="openService('${escapeAttr(row.id)}')">${escapeHtml(row.url)}</button></div>
            </div>
            <div class="chips">
              ${chip(scope, remote ? 'remote' : 'ok')}
              ${chip(access, 'muted')}
              ${chip(row.service_mode, 'muted')}
              ${chip(row.runtime_status || 'unknown', state.kind)}
              ${chip(row.tunnel_modes || 'reserved', 'muted')}
              ${row.desired_state && row.desired_state !== '-' ? chip('desired ' + row.desired_state, 'muted') : ''}
            </div>
            <div class="actions">
              <button class="${primaryClass} primary-action" ${primaryBusy ? 'disabled' : ''} onclick="runAction('${escapeAttr(row.id)}', '${primaryAction}')">${primaryBusy ? 'Working' : primaryLabel}</button>
              <button class="primary-action" ${restartBusy ? 'disabled' : ''} onclick="runAction('${escapeAttr(row.id)}', '${restartAction}')">${restartBusy ? 'Working' : 'Restart'}</button>
              <button class="secondary" onclick="openService('${escapeAttr(row.id)}')">Open</button>
              <button onclick="renameService('${escapeAttr(row.id)}')">Rename</button>
            </div>
          </article>`;
        }).join('')}</div>`;
    }

    function renderPorts(rows) {
      const target = document.getElementById('ports-view');
      if (!rows.length) {
        target.innerHTML = '<div class="empty">No ports reserved.</div>';
        return;
      }
      const cells = rows.map(row => `
        <tr>
          <td>${escapeHtml(row.port)}</td>
          <td>${escapeHtml(row.id)}</td>
          <td>${escapeHtml(row.owner_host)}</td>
          <td>${escapeHtml(row.source_machine)}</td>
          <td>${escapeHtml(row.service_mode)}</td>
          <td>${escapeHtml(row.startup_policy)}</td>
          <td>${escapeHtml(row.restart_policy)}</td>
          <td>${escapeHtml(row.desired_state)}</td>
          <td>${escapeHtml(row.tunnel_modes)}</td>
          <td>${escapeHtml(row.runtime_status)}</td>
          <td><button class="linklike" onclick="openService('${escapeAttr(row.id)}')">${escapeHtml(row.url)}</button></td>
        </tr>`).join('');
      target.innerHTML = `<div class="ports-table"><table>
          <thead><tr><th>Port</th><th>ID</th><th>Owner</th><th>Source</th><th>Mode</th><th>Startup</th><th>Restart</th><th>Desired</th><th>Tunnel</th><th>Status</th><th>URL</th></tr></thead>
          <tbody>${cells}</tbody>
        </table></div>`;
    }

    function chip(text, kind) {
      return `<span class="chip ${escapeAttr(kind || '')}">${escapeHtml(text)}</span>`;
    }

    function statusKind(status) {
      return serviceState(status).kind;
    }

    function serviceState(status) {
      const value = String(status || '').toLowerCase();
      if (value.includes('running')) return { key: 'running', label: 'Running', kind: 'ok' };
      if (value.includes('stale')) return { key: 'stale', label: 'Stale', kind: 'warn' };
      if (value.includes('unhealthy') || value.includes('failed')) return { key: 'unhealthy', label: 'Unhealthy', kind: 'warn' };
      if (value.includes('conflict') || value.includes('error')) return { key: 'error', label: 'Error', kind: 'bad' };
      if (value.includes('stopped')) return { key: 'stopped', label: 'Stopped', kind: 'muted' };
      if (value.includes('record')) return { key: 'recorded', label: 'Recorded', kind: 'muted' };
      return { key: 'unknown', label: 'Unknown', kind: 'muted' };
    }

    function isRemote(row) {
      return Boolean(row?.local_machine_id && (
        (row.source_machine && row.source_machine !== row.local_machine_id) ||
        (row.owner_host && row.owner_host !== row.local_machine_id)
      ));
    }

    function openDashboard() {
      runDashboardOpen();
    }

    async function loadAgentPrompts() {
      try {
        const response = await fetch('/api/agent-prompts', { cache: 'no-store' });
        const result = await response.json();
        if (!response.ok || !result.ok) throw new Error(result.message || 'failed');
        const prompts = Array.isArray(result.prompts) ? result.prompts : [];
        const target = document.getElementById('prompt-buttons');
        if (!target || !prompts.length) return;
        target.innerHTML = prompts.map(prompt =>
          `<button class="secondary" onclick="copyAgentPrompt('${escapeAttr(prompt.machine_id)}')">${escapeHtml(prompt.label || ('Copy ' + prompt.machine_id))}</button>`
        ).join('');
      } catch (err) {
        console.warn('agent prompt list failed', err);
      }
    }

    async function copyAgentPrompt(machineId = '') {
      try {
        const params = machineId ? '?' + new URLSearchParams({ machine: machineId }).toString() : '';
        const response = await fetch('/api/agent-prompt' + params, { cache: 'no-store' });
        const result = await response.json();
        if (!response.ok || !result.ok) throw new Error(result.message || 'failed');
        await copyText(result.prompt);
        showToast('Agent prompt copied for ' + (result.machine_id || machineId || 'local'));
      } catch (err) {
        showToast('Copy prompt failed: ' + err);
      }
    }

    async function copyText(text) {
      if (navigator.clipboard && window.isSecureContext) {
        await navigator.clipboard.writeText(text);
        return;
      }
      const textarea = document.createElement('textarea');
      textarea.value = text;
      textarea.setAttribute('readonly', '');
      textarea.style.position = 'fixed';
      textarea.style.left = '-9999px';
      document.body.appendChild(textarea);
      textarea.select();
      const copied = document.execCommand('copy');
      textarea.remove();
      if (!copied) throw new Error('clipboard unavailable');
    }

    async function runDashboardOpen() {
      try {
        const response = await fetch('/api/open-dashboard', authOptions({ method: 'POST', cache: 'no-store' }));
        const result = await response.json();
        showToast(result.message || 'opened dashboard');
        if (!response.ok || !result.ok) throw new Error(result.message || 'failed');
      } catch (err) {
        showToast('Open browser failed: ' + err);
      }
    }

    function openService(id) {
      const row = currentRows.find(candidate => candidate.id === id);
      if (canDirectOpen(row)) {
        runDirectOpen(row);
        return;
      }
      runAction(id, 'open');
    }

    function canDirectOpen(row) {
      if (!row || row.direct_open !== true || !row.url) return false;
      return serviceState(row.runtime_status).key === 'running';
    }

    async function runDirectOpen(row) {
      const status = document.getElementById('status');
      busyKey = row.id + ':open';
      render();
      status.textContent = 'open ' + row.id + '...';
      try {
        const params = new URLSearchParams({ id: row.id, url: row.url });
        const response = await fetch('/api/open-url?' + params.toString(), authOptions({ method: 'POST', cache: 'no-store' }));
        const result = await response.json();
        showToast(result.message || 'opened ' + row.url);
        if (!response.ok || !result.ok) throw new Error(result.message || 'failed');
      } catch (err) {
        status.textContent = 'Open failed: ' + err;
        showToast('Open failed: ' + err);
      } finally {
        busyKey = '';
        render();
      }
    }

    function renameService(id) {
      const row = currentRows.find(candidate => candidate.id === id);
      const currentTitle = row?.title || id;
      const nextTitle = window.prompt('Rename service', currentTitle);
      if (nextTitle === null) return;
      const title = nextTitle.trim();
      if (!title) {
        showToast('Rename failed: title must not be empty');
        return;
      }
      runAction(id, 'rename', { title });
    }

    function escapeHtml(value) {
      return String(value).replace(/[&<>"']/g, ch => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[ch]));
    }

    function escapeAttr(value) {
      return escapeHtml(value).replace(/`/g, '&#96;');
    }

    async function runAction(id, action, extra = {}) {
      const status = document.getElementById('status');
      busyKey = id + ':' + action;
      render();
      status.textContent = action + ' ' + id + '...';
      try {
        const params = new URLSearchParams({ id, action, ...extra });
        const response = await fetch('/api/action?' + params.toString(), authOptions({ method: 'POST', cache: 'no-store' }));
        const result = await response.json();
        showToast(result.message || 'done');
        if (!response.ok || !result.ok) throw new Error(result.message || 'failed');
        await loadPorts();
      } catch (err) {
        status.textContent = 'Action failed: ' + err;
        showToast('Action failed: ' + err);
      } finally {
        busyKey = '';
        render();
      }
    }

    async function runActionThenOpen(id, action, extra = {}) {
      const status = document.getElementById('status');
      busyKey = id + ':' + action;
      render();
      status.textContent = action + ' ' + id + '...';
      try {
        const params = new URLSearchParams({ id, action, ...extra });
        const response = await fetch('/api/action?' + params.toString(), authOptions({ method: 'POST', cache: 'no-store' }));
        const result = await response.json();
        showToast(result.message || 'done');
        if (!response.ok || !result.ok) throw new Error(result.message || 'failed');
        await loadPorts();
        openService(id);
      } catch (err) {
        status.textContent = 'Action failed: ' + err;
        showToast('Action failed: ' + err);
      } finally {
        busyKey = '';
        render();
      }
    }

    function authOptions(options = {}) {
      const headers = new Headers(options.headers || {});
      headers.set('X-Bridgeboard-Token', window.__bridgeboardToken || '');
      return { ...options, headers };
    }

    function showToast(message) {
      const old = document.querySelector('.toast');
      if (old) old.remove();
      const toast = document.createElement('div');
      toast.className = 'toast';
      toast.textContent = message;
      document.body.appendChild(toast);
      setTimeout(() => toast.remove(), 4200);
    }

    function refreshOnAttention() {
      const now = Date.now();
      if (document.hidden || now - lastFocusRefresh < 1500) return;
      lastFocusRefresh = now;
      loadPorts();
    }

    loadPorts();
    loadAgentPrompts();
    window.addEventListener('focus', refreshOnAttention);
    document.addEventListener('visibilitychange', refreshOnAttention);
  </script>
</body>
</html>
"##;
