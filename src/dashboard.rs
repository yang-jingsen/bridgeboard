use crate::core;
pub use crate::core::{BridgeEnv as DashboardEnv, PortRow};
use crate::peer;
use crate::registry::{validate_no_port_conflicts, Registry, RegistryExport};
use crate::state::State;
use crate::terminal::TerminalManager;
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
    let runtime = DashboardRuntime::new(env, include_peers, terminal_api_allowed(host));
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
    terminals: TerminalManager,
    terminal_api_enabled: bool,
}

impl DashboardRuntime {
    fn new(env: DashboardEnv, include_peers: bool, terminal_api_enabled: bool) -> Self {
        let cache_path = env.paths.state_file.with_file_name("dashboard-cache.json");
        Self {
            env,
            include_peers,
            export_cache: ExportCache::load(cache_path),
            terminals: TerminalManager::default(),
            terminal_api_enabled,
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
    let mut buffer = [0_u8; 65536];
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
            let owner_host =
                query_value(&url, "owner_host").filter(|value| !value.trim().is_empty());
            let source_machine =
                query_value(&url, "source_machine").filter(|value| !value.trim().is_empty());
            let local_port = query_value(&url, "local_port")
                .as_deref()
                .map(str::parse::<u16>)
                .transpose()
                .context("invalid local_port")?;
            let port = query_value(&url, "port")
                .as_deref()
                .map(str::parse::<u16>)
                .transpose()
                .context("invalid port")?;
            match run_action(
                &runtime.env,
                &id,
                &action,
                title.as_deref(),
                owner_host.as_deref(),
                source_machine.as_deref(),
                port,
                local_port,
            ) {
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
        "/api/terminal/sessions" => {
            if !require_terminal_access(runtime, stream, method, &request, token)? {
                return Ok(());
            }
            respond_json(
                stream,
                &serde_json::to_string_pretty(&json!({
                    "ok": true,
                    "sessions": runtime.terminals.list(),
                }))?,
            )?;
        }
        "/api/terminal/start" => {
            if !require_terminal_access(runtime, stream, method, &request, token)? {
                return Ok(());
            }
            let cols = terminal_dimension(&url, "cols", 96);
            let rows = terminal_dimension(&url, "rows", 26);
            let service_id =
                query_value(&url, "service_id").filter(|value| !value.trim().is_empty());
            let result = if let Some(service_id) = service_id {
                runtime
                    .terminals
                    .start_service(&runtime.env, &service_id, cols, rows)
            } else {
                runtime.terminals.start_shell(cols, rows)
            };
            match result {
                Ok(session) => respond_json(
                    stream,
                    &serde_json::to_string_pretty(&json!({
                        "ok": true,
                        "session": session,
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
        "/api/terminal/read" => {
            if !require_terminal_access(runtime, stream, method, &request, token)? {
                return Ok(());
            }
            let session_id = query_value(&url, "session").context("missing session")?;
            let after = query_value(&url, "after").and_then(|value| value.parse::<u64>().ok());
            match runtime.terminals.read(&session_id, after) {
                Ok(read) => respond_json(
                    stream,
                    &serde_json::to_string_pretty(&json!({
                        "ok": true,
                        "read": read,
                    }))?,
                )?,
                Err(err) => respond_json_status(
                    stream,
                    "404 Not Found",
                    &serde_json::to_string_pretty(&json!({
                        "ok": false,
                        "message": err.to_string(),
                    }))?,
                )?,
            }
        }
        "/api/terminal/input" => {
            if !require_terminal_access(runtime, stream, method, &request, token)? {
                return Ok(());
            }
            let session_id = query_value(&url, "session").context("missing session")?;
            let data = query_value(&url, "data").unwrap_or_default();
            match runtime.terminals.input(&session_id, &data) {
                Ok(session) => respond_json(
                    stream,
                    &serde_json::to_string_pretty(&json!({
                        "ok": true,
                        "session": session,
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
        "/api/terminal/resize" => {
            if !require_terminal_access(runtime, stream, method, &request, token)? {
                return Ok(());
            }
            let session_id = query_value(&url, "session").context("missing session")?;
            let cols = terminal_dimension(&url, "cols", 96);
            let rows = terminal_dimension(&url, "rows", 26);
            match runtime.terminals.resize(&session_id, cols, rows) {
                Ok(session) => respond_json(
                    stream,
                    &serde_json::to_string_pretty(&json!({
                        "ok": true,
                        "session": session,
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
        "/api/terminal/stop" => {
            if !require_terminal_access(runtime, stream, method, &request, token)? {
                return Ok(());
            }
            let session_id = query_value(&url, "session").context("missing session")?;
            match runtime.terminals.stop(&session_id) {
                Ok(session) => respond_json(
                    stream,
                    &serde_json::to_string_pretty(&json!({
                        "ok": true,
                        "session": session,
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

fn terminal_dimension(url: &Url, key: &str, default: u16) -> u16 {
    query_value(url, key)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(default)
}

fn require_terminal_access(
    runtime: &DashboardRuntime,
    stream: &mut TcpStream,
    method: &str,
    request: &str,
    token: &str,
) -> Result<bool> {
    if !runtime.terminal_api_enabled {
        respond(
            stream,
            "403 Forbidden",
            "application/json; charset=utf-8",
            r#"{"ok":false,"message":"terminal API is disabled for non-loopback dashboard binds"}"#,
        )?;
        return Ok(false);
    }
    require_post_token(stream, method, request, token)
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

fn terminal_api_allowed(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1")
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

fn run_action(
    env: &DashboardEnv,
    id: &str,
    action: &str,
    title: Option<&str>,
    owner_host: Option<&str>,
    source_machine: Option<&str>,
    port: Option<u16>,
    local_port: Option<u16>,
) -> Result<String> {
    let peer_source_target = source_machine
        .map(|source| source != env.machine_id)
        .unwrap_or(false);
    if action == "open" {
        let url = if peer_source_target {
            core::open_remote_target(env, id, owner_host, source_machine, port, local_port)?
        } else {
            core::open(env, id)?
        };
        return Ok(format!("opened {url}"));
    }
    if action == "rename" {
        let title = title.context("missing title")?;
        let lines = if peer_source_target {
            core::rename_title_target(env, id, title, owner_host, source_machine, port)?
        } else {
            core::rename_title(env, id, title)?
        };
        return Ok(lines.join("\n"));
    }

    let lines = match action {
        "up" => {
            if peer_source_target {
                core::remote_up_target(env, id, owner_host, source_machine, port, local_port)?
            } else {
                core::up(env, id)?
            }
        }
        "remote-up" => {
            if peer_source_target {
                core::remote_up_target(env, id, owner_host, source_machine, port, local_port)?
            } else {
                core::remote_up(env, id)?
            }
        }
        "remote-down" => {
            if peer_source_target {
                core::remote_down_target(env, id, owner_host, source_machine, port)?
            } else {
                core::remote_down(env, id)?
            }
        }
        "remote-restart" => {
            if peer_source_target {
                core::remote_restart_target(env, id, owner_host, source_machine, port, local_port)?
            } else {
                core::remote_restart(env, id)?
            }
        }
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
      --bg: #f3f7fb;
      --panel: #ffffff;
      --panel-2: #f7fbff;
      --line: #d5e2f0;
      --text: #111827;
      --muted: #61758e;
      --soft: #edf5ff;
      --accent: #2563eb;
      --accent-2: #5dbdf7;
      --ok: #1d4ed8;
      --warn: #9a5a00;
      --bad: #dc2626;
      --remote: #0369a1;
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
    button.secondary { background: #eaf3ff; color: #163f82; border-color: #b9d4fb; }
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
    input:focus { border-color: var(--accent); box-shadow: 0 0 0 2px rgba(37,99,235,.14); }
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
    .mark { width: 30px; height: 30px; border-radius: 7px; background: linear-gradient(135deg, #07111f, #2563eb 58%, #d9f2ff); }
    h1 { font-size: 18px; margin: 0; font-weight: 700; letter-spacing: 0; }
    .machine { color: var(--muted); font-size: 12px; margin-top: 2px; }
    .nav { display: grid; gap: 4px; }
    .nav button { text-align: left; border: 0; background: transparent; padding: 9px 10px; color: var(--muted); }
    .nav button.active { background: #eaf3ff; color: #173f80; font-weight: 650; }
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
      background: #eaf5ff;
      color: #17458f;
      border: 1px solid #b8d9ff;
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
    .device-grid {
      display: grid;
      grid-template-columns: repeat(auto-fill, minmax(310px, 1fr));
      gap: 10px;
      align-items: stretch;
    }
    .device-card {
      background: var(--panel);
      border: 1px solid var(--line);
      border-radius: 8px;
      padding: 13px;
      min-height: 182px;
      display: grid;
      grid-template-rows: auto 1fr auto;
      gap: 12px;
    }
    .device-head { display: grid; grid-template-columns: 42px minmax(0, 1fr); gap: 10px; align-items: center; }
    .device-icon {
      width: 42px;
      height: 42px;
      border-radius: 8px;
      background: #07111f;
      color: #d9f2ff;
      border: 1px solid #2b5fa8;
      display: inline-flex;
      align-items: center;
      justify-content: center;
      font-weight: 800;
      font-size: 13px;
      letter-spacing: 0;
    }
    .device-name { min-width: 0; }
    .device-name strong { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .device-name span { display: block; color: var(--muted); font-size: 12px; margin-top: 3px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .device-stats {
      display: grid;
      grid-template-columns: repeat(3, minmax(0, 1fr));
      gap: 8px;
    }
    .device-stat {
      border: 1px solid var(--line);
      background: var(--panel-2);
      border-radius: 8px;
      padding: 8px;
      min-width: 0;
    }
    .device-stat span { display: block; color: var(--muted); font-size: 11px; margin-bottom: 3px; }
    .device-stat strong { display: block; font-size: 15px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .device-todo { color: var(--muted); font-size: 12px; line-height: 1.4; }
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
    .state-badge.ok { background: #e5f1ff; color: var(--ok); }
    .state-badge.warn { background: #fff0cf; color: var(--warn); }
    .state-badge.bad { background: #fee2e2; color: var(--bad); }
    .state-badge.muted { background: var(--soft); color: var(--muted); }
    .chips { display: flex; gap: 6px; flex-wrap: wrap; align-content: center; align-items: center; }
    .chip { border-radius: 999px; padding: 4px 8px; font-size: 12px; font-weight: 650; background: var(--soft); color: #46515c; }
    .chip.ok { background: #e5f1ff; color: var(--ok); }
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
    .terminal-layout {
      height: 100%;
      min-height: 0;
      display: grid;
      grid-template-columns: minmax(220px, 280px) minmax(0, 1fr);
      gap: 12px;
    }
    .terminal-side {
      min-height: 0;
      background: var(--panel);
      border: 1px solid var(--line);
      border-radius: 8px;
      padding: 10px;
      display: grid;
      grid-template-rows: auto minmax(0, 1fr);
      gap: 10px;
    }
    .terminal-side-actions { display: grid; grid-template-columns: 1fr 1fr; gap: 6px; }
    .terminal-list { min-height: 0; overflow: auto; display: grid; align-content: start; gap: 6px; }
    .terminal-item {
      width: 100%;
      text-align: left;
      display: grid;
      gap: 3px;
      min-height: 46px;
    }
    .terminal-item.active { border-color: var(--accent); background: #eaf3ff; }
    .terminal-item strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .terminal-item span { color: var(--muted); font-size: 12px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .terminal-panel {
      min-height: 0;
      background: #0c1116;
      border: 1px solid #24313a;
      border-radius: 8px;
      display: grid;
      grid-template-rows: auto minmax(0, 1fr) auto;
      overflow: hidden;
    }
    .terminal-bar {
      background: #111922;
      border-bottom: 1px solid #25323c;
      padding: 9px 10px;
      display: flex;
      align-items: center;
      gap: 8px;
      min-width: 0;
    }
    .terminal-bar strong { color: #edf5ff; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .terminal-bar .spacer { flex: 1 1 auto; }
    .terminal-output {
      margin: 0;
      padding: 13px;
      min-height: 0;
      overflow: auto;
      color: #dbeafe;
      background: #070b0f;
      font: 13px/1.45 Consolas, "Cascadia Mono", "SFMono-Regular", monospace;
      white-space: pre-wrap;
      word-break: break-word;
    }
    .terminal-input-row {
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto auto;
      gap: 6px;
      padding: 9px;
      background: #111922;
      border-top: 1px solid #25323c;
    }
    .terminal-input-row input {
      background: #071017;
      color: #dbeafe;
      border-color: #2d404b;
      font-family: Consolas, "Cascadia Mono", "SFMono-Regular", monospace;
    }
    .terminal-empty {
      height: 100%;
      min-height: 340px;
      display: grid;
      place-items: center;
      color: #8aa1af;
      background: #070b0f;
      border-radius: 8px;
      border: 1px solid #24313a;
    }
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
      .terminal-layout { grid-template-columns: 1fr; }
      .terminal-side { max-height: 220px; }
      .toast { left: 18px; max-width: calc(100vw - 36px); }
    }
    @media (prefers-color-scheme: dark) {
      :root {
        --bg: #070b12; --panel: #0d1420; --panel-2: #121c2b; --line: #263550;
        --text: #f4f8ff; --muted: #9fb5cc; --soft: #17243a;
      }
      .sidebar { background: #090f18; }
      button { background: #111c2b; color: var(--text); border-color: #2b3d5a; }
      button.secondary { background: #10213a; color: #c8e6ff; border-color: #2d5c92; }
      button.warn { background: #261c12; color: #ffd7a3; border-color: #8a5a1f; }
      select { background: #111c2b; color: var(--text); border-color: #2b3d5a; }
      .nav button.active { background: #10213a; color: #d9f2ff; }
      th, td { border-bottom-color: #263550; }
      .state-badge.ok { background: #102b52; color: #9bd7ff; }
      .state-badge.warn { background: #362411; color: #f7c26f; }
      .state-badge.bad { background: #3a1717; color: #fca5a5; }
      .chip { background: #17243a; color: #c5d8ea; }
      .chip.ok { background: #102b52; color: #9bd7ff; }
      .chip.warn { background: #362411; color: #f7c26f; }
      .chip.bad { background: #3a1717; color: #fca5a5; }
      .chip.remote { background: #0f2a44; color: #9bd7ff; }
      .pin-button.active { background: #25314d; color: #d9f2ff; border-color: #4d78b8; }
      .terminal-item.active { background: #10213a; }
      .app-icon { background: #10213a; color: #d9f2ff; border-color: #2d5c92; }
      .device-icon { background: #06101d; color: #d9f2ff; border-color: #2d5c92; }
      .device-stat { background: #121c2b; border-color: #263550; }
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
        <button id="nav-terminals" onclick="setView('terminals')">Terminals</button>
        <button id="nav-devices" onclick="setView('devices')">Devices</button>
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
      <section id="terminals-view" class="view-pane hidden"></section>
      <section id="devices-view" class="view-pane hidden"></section>
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
    let terminalSessions = [];
    let activeTerminalId = '';
    let terminalBuffers = {};
    let terminalAfterSeq = {};
    let terminalPollTimer = null;
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
      view = normalizeView(view);
      currentView = view;
      if (window.location.hash !== '#' + view) history.replaceState(null, '', '#' + view);
      document.getElementById('nav-apps').classList.toggle('active', view === 'apps');
      document.getElementById('nav-services').classList.toggle('active', view === 'services');
      document.getElementById('nav-terminals').classList.toggle('active', view === 'terminals');
      document.getElementById('nav-devices').classList.toggle('active', view === 'devices');
      document.getElementById('nav-ports').classList.toggle('active', view === 'ports');
      document.getElementById('view-title').textContent = viewTitle(view);
      if (view === 'terminals') loadTerminalSessions();
      render();
    }

    function normalizeView(view) {
      return ['apps', 'services', 'terminals', 'devices', 'ports'].includes(view) ? view : 'apps';
    }

    function viewTitle(view) {
      if (view === 'apps') return 'Apps';
      if (view === 'services') return 'Services';
      if (view === 'terminals') return 'Terminals';
      if (view === 'devices') return 'Devices';
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
      const allDevices = collectDevices(rows, local);
      const peers = [...new Set(rows.map(row => row.source_machine).filter(machine => machine && machine !== local))];
      const running = rows.filter(row => serviceState(row.runtime_status).key === 'running').length;
      const stale = rows.filter(row => serviceState(row.runtime_status).key === 'stale').length;
      document.getElementById('machine').textContent = local;
      const sortSelect = document.getElementById('sort-mode');
      if (sortSelect) sortSelect.value = sortMode;
      document.getElementById('updated').textContent = 'Updated ' + new Date().toLocaleTimeString();
      document.getElementById('status').textContent = visibleRows.length + ' shown / ' + rows.length + ' service record(s), ' + peers.length + ' peer source(s), ledger OK';
      document.getElementById('summary').innerHTML = `
        ${metric(currentView === 'devices' ? 'Devices' : (currentView === 'apps' ? 'Apps' : 'Services'), currentView === 'devices' ? allDevices.length : rows.length)}
        ${metric('Running', running)}
        ${metric('Stale', stale)}
        ${metric('Peers', peers.length ? peers.join(', ') : 'none', true)}
      `;
      document.getElementById('apps-view').classList.toggle('hidden', currentView !== 'apps');
      document.getElementById('services-view').classList.toggle('hidden', currentView !== 'services');
      document.getElementById('terminals-view').classList.toggle('hidden', currentView !== 'terminals');
      document.getElementById('devices-view').classList.toggle('hidden', currentView !== 'devices');
      document.getElementById('ports-view').classList.toggle('hidden', currentView !== 'ports');
      renderApps(sortedRows);
      renderServices(sortedRows);
      renderTerminals();
      renderDevices(sortedRows, local);
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
      return Number(pinnedIds.has(rowKey(b))) - Number(pinnedIds.has(rowKey(a)));
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

    function rowKey(row) {
      return [row.source_machine || '', row.owner_host || '', row.id || '', String(row.port || '')].join('|');
    }

    function rowByKey(key) {
      return currentRows.find(candidate => rowKey(candidate) === key);
    }

    function rowActionExtra(row) {
      const extra = {
        owner_host: row.owner_host || '',
        source_machine: row.source_machine || '',
        port: String(row.port || ''),
      };
      if (row.local_port) extra.local_port = String(row.local_port);
      return extra;
    }

    function togglePin(key) {
      if (pinnedIds.has(key)) {
        pinnedIds.delete(key);
        showToast('Unpinned');
      } else {
        pinnedIds.add(key);
        showToast('Pinned');
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
        const key = rowKey(row);
        const remote = isRemote(row);
        const state = serviceState(row.runtime_status);
        const pinned = pinnedIds.has(key);
        const primary = appPrimary(row);
        const secondary = appSecondary(row);
        const busy = busyKey === key + ':' + primary.action;
        const secondaryBusy = busyKey === key + ':' + secondary.action;
        const restartAction = remote ? 'remote-restart' : 'restart';
        const restartBusy = busyKey === key + ':' + restartAction;
        return `
          <article class="app-card">
            <div class="app-head">
              <button class="app-icon" onclick="runAppPrimary('${escapeAttr(key)}')" title="Open ${escapeAttr(row.title || row.id)}">${escapeHtml(appInitials(row))}</button>
              <div class="app-name">
                <strong>${escapeHtml(row.title || row.id)}</strong>
                <span>${escapeHtml(appSubtitle(row))}</span>
              </div>
              <button class="pin-button ${pinned ? 'active' : ''}" title="${pinned ? 'Unpin app' : 'Pin app'}" onclick="togglePin('${escapeAttr(key)}')">${pinned ? '★' : '☆'}</button>
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
              <button class="${primary.className} primary-action wide" ${busy ? 'disabled' : ''} onclick="runAppPrimary('${escapeAttr(key)}')">${busy ? 'Working' : primary.label}</button>
              <button class="${secondary.className}" ${secondaryBusy ? 'disabled' : ''} onclick="runAppSecondary('${escapeAttr(key)}')">${secondaryBusy ? 'Working' : secondary.label}</button>
              <button ${restartBusy ? 'disabled' : ''} onclick="runActionByKey('${escapeAttr(key)}', '${restartAction}')">${restartBusy ? 'Working' : 'Restart'}</button>
              <button ${remote ? 'disabled' : ''} onclick="startServiceTerminal('${escapeAttr(key)}')">Terminal</button>
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

    function runAppPrimary(key) {
      const row = rowByKey(key);
      if (!row) return;
      const primary = appPrimary(row);
      if (primary.action === 'open') {
        openServiceByKey(key);
      } else if (primary.action === 'remote-up-open') {
        runActionThenOpenByKey(key, 'remote-up');
      } else {
        runActionByKey(key, primary.action);
      }
    }

    function runAppSecondary(key) {
      const row = rowByKey(key);
      if (!row) return;
      const secondary = appSecondary(row);
      if (secondary.action === 'open') {
        openServiceByKey(key);
      } else {
        runActionByKey(key, secondary.action);
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

    function renderDevices(rows, local) {
      const target = document.getElementById('devices-view');
      const devices = collectDevices(rows, local);
      if (!devices.length) {
        target.innerHTML = '<div class="empty">No devices discovered.</div>';
        return;
      }
      target.innerHTML = `<div class="device-grid">${devices.map(device => `
        <article class="device-card">
          <div class="device-head">
            <div class="device-icon">${escapeHtml(deviceInitials(device.id))}</div>
            <div class="device-name">
              <strong>${escapeHtml(device.id)}</strong>
              <span>${escapeHtml(deviceSubtitle(device))}</span>
            </div>
          </div>
          <div class="device-stats">
            ${deviceStat('Seen', device.seen)}
            ${deviceStat('Owned', device.owned)}
            ${deviceStat('Running', device.running)}
          </div>
          <div class="chips">
            ${[...device.roles].map(role => chip(role, role === 'local' ? 'ok' : (role === 'peer' ? 'remote' : 'muted'))).join('')}
            ${chip('ports ' + portSummary(device.ports), 'muted')}
          </div>
          <div class="device-todo">TODO: device display names, SSH aliases, dashboard endpoint, trust policy, and local-forward defaults will be managed here.</div>
        </article>
      `).join('')}</div>`;
    }

    function collectDevices(rows, local) {
      const devices = new Map();
      const ensure = id => {
        const key = String(id || '').trim() || 'unknown';
        if (!devices.has(key)) {
          devices.set(key, { id: key, roles: new Set(), seen: 0, owned: 0, running: 0, stale: 0, ports: [] });
        }
        return devices.get(key);
      };
      ensure(local).roles.add('local');
      for (const row of rows) {
        const owner = row.owner_host || 'unknown';
        const source = row.source_machine || owner || 'unknown';
        const state = serviceState(row.runtime_status).key;
        const sourceDevice = ensure(source);
        sourceDevice.roles.add(source === local ? 'local' : 'peer');
        sourceDevice.seen += 1;
        if (state === 'running') sourceDevice.running += 1;
        if (state === 'stale') sourceDevice.stale += 1;

        const ownerDevice = ensure(owner);
        ownerDevice.roles.add(owner === local ? 'local' : 'owner');
        ownerDevice.owned += 1;
        if (row.port) ownerDevice.ports.push(Number(row.port));
      }
      return [...devices.values()].sort((a, b) => {
        const rank = roleRank(a) - roleRank(b);
        return rank || compareText(a.id, b.id);
      });
    }

    function roleRank(device) {
      if (device.roles.has('local')) return 0;
      if (device.roles.has('peer')) return 1;
      return 2;
    }

    function deviceSubtitle(device) {
      const bits = [];
      if (device.roles.has('local')) bits.push('local machine');
      if (device.roles.has('peer')) bits.push('peer registry source');
      if (device.roles.has('owner')) bits.push('service owner');
      return bits.join(' - ') || 'device';
    }

    function deviceInitials(name) {
      const source = String(name || 'device').trim();
      const parts = source.split(/[\s._-]+/).filter(Boolean);
      const letters = parts.length > 1
        ? parts.slice(0, 2).map(part => part[0]).join('')
        : source.slice(0, 2);
      return letters.toUpperCase();
    }

    function deviceStat(label, value) {
      return `<div class="device-stat"><span>${escapeHtml(label)}</span><strong>${escapeHtml(value)}</strong></div>`;
    }

    function portSummary(ports) {
      const unique = [...new Set((ports || []).filter(port => Number.isFinite(port)).sort((a, b) => a - b))];
      if (!unique.length) return 'none';
      if (unique.length <= 3) return unique.map(port => ':' + port).join(', ');
      return ':' + unique[0] + '-:' + unique[unique.length - 1] + ' (' + unique.length + ')';
    }

    function renderServices(rows) {
      const target = document.getElementById('services-view');
      if (!rows.length) {
        target.innerHTML = '<div class="empty">No services recorded.</div>';
        return;
      }
      target.innerHTML = `<div class="service-list">${rows.map(row => {
        const key = rowKey(row);
        const remote = isRemote(row);
        const state = serviceState(row.runtime_status);
        const running = state.key === 'running';
        const primaryAction = running ? (remote ? 'remote-down' : 'down') : (remote ? 'remote-up' : 'up');
        const restartAction = remote ? 'remote-restart' : 'restart';
        const primaryLabel = running ? 'Stop' : 'Start';
        const primaryClass = running ? 'warn' : 'primary';
        const primaryBusy = busyKey === key + ':' + primaryAction;
        const restartBusy = busyKey === key + ':' + restartAction;
        const scope = remote ? `owner ${row.owner_host}` : 'local owner';
        const access = remote ? (String(row.tunnel_modes || '').includes('local') ? 'ssh local' : 'network') : 'local';
        const pinned = pinnedIds.has(key);
        return `
          <article class="service-row">
            <div class="service-main">
              <div class="service-title">
                <button class="pin-button ${pinned ? 'active' : ''}" title="${pinned ? 'Unpin service' : 'Pin service'}" onclick="togglePin('${escapeAttr(key)}')">${pinned ? '★' : '☆'}</button>
                <span class="state-badge ${escapeAttr(state.kind)}">${escapeHtml(state.label)}</span>
                <span class="port-badge">:${escapeHtml(row.port)}</span>
                <strong>${escapeHtml(row.title || row.id)}</strong>
              </div>
              <div class="service-meta">${escapeHtml(row.id)} - owner ${escapeHtml(row.owner_host)} - source ${escapeHtml(row.source_machine)}</div>
              <div class="urlbox"><button class="linklike" onclick="openServiceByKey('${escapeAttr(key)}')">${escapeHtml(row.url)}</button></div>
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
              <button class="${primaryClass} primary-action" ${primaryBusy ? 'disabled' : ''} onclick="runActionByKey('${escapeAttr(key)}', '${primaryAction}')">${primaryBusy ? 'Working' : primaryLabel}</button>
              <button class="primary-action" ${restartBusy ? 'disabled' : ''} onclick="runActionByKey('${escapeAttr(key)}', '${restartAction}')">${restartBusy ? 'Working' : 'Restart'}</button>
              <button class="secondary" onclick="openServiceByKey('${escapeAttr(key)}')">Open</button>
              <button ${remote ? 'disabled' : ''} onclick="startServiceTerminal('${escapeAttr(key)}')">Terminal</button>
              <button onclick="renameService('${escapeAttr(key)}')">Rename</button>
            </div>
          </article>`;
        }).join('')}</div>`;
    }

    function renderTerminals() {
      const target = document.getElementById('terminals-view');
      if (!target) return;
      const active = terminalSessions.find(session => session.id === activeTerminalId) || terminalSessions[0] || null;
      if (active && active.id !== activeTerminalId) activeTerminalId = active.id;
      const activeBuffer = active ? (terminalBuffers[active.id] || '') : '';
      target.innerHTML = `
        <div class="terminal-layout">
          <aside class="terminal-side">
            <div class="terminal-side-actions">
              <button class="primary" onclick="startShellTerminal()">Start Shell</button>
              <button onclick="loadTerminalSessions()">Refresh</button>
            </div>
            <div class="terminal-list">
              ${terminalSessions.length ? terminalSessions.map(session => `
                <button class="terminal-item ${session.id === activeTerminalId ? 'active' : ''}" onclick="selectTerminal('${escapeAttr(session.id)}')">
                  <strong>${escapeHtml(session.title || session.id)}</strong>
                  <span>${escapeHtml(session.status || 'unknown')} ${session.service_id ? '- ' + escapeHtml(session.service_id) : ''}</span>
                </button>
              `).join('') : '<div class="empty">No terminal sessions.</div>'}
            </div>
          </aside>
          ${active ? `
            <section class="terminal-panel">
              <div class="terminal-bar">
                <strong>${escapeHtml(active.title || active.id)}</strong>
                ${chip(active.status || 'unknown', active.status === 'running' ? 'ok' : 'muted')}
                ${active.service_id ? chip(active.service_id, 'muted') : chip('shell', 'muted')}
                <span class="spacer"></span>
                <button onclick="sendTerminalControl('ctrl-c')">Ctrl-C</button>
                <button onclick="resizeActiveTerminal()">Resize</button>
                <button class="warn" onclick="stopActiveTerminal()">Stop</button>
              </div>
              <pre id="terminal-output" class="terminal-output">${escapeHtml(cleanTerminalText(activeBuffer))}</pre>
              <div class="terminal-input-row">
                <input id="terminal-line" autocomplete="off" spellcheck="false" onkeydown="terminalLineKeydown(event)" placeholder="$">
                <button class="primary" onclick="sendTerminalLine()">Send</button>
                <button onclick="sendTerminalControl('ctrl-d')">EOF</button>
              </div>
            </section>
          ` : '<div class="terminal-empty">No active terminal.</div>'}
        </div>`;
      scrollTerminalToBottom();
    }

    async function loadTerminalSessions() {
      try {
        const result = await terminalRequest('sessions');
        terminalSessions = Array.isArray(result.sessions) ? result.sessions : [];
        if (!activeTerminalId && terminalSessions.length) activeTerminalId = terminalSessions[0].id;
        for (const session of terminalSessions) {
          if (!(session.id in terminalBuffers)) terminalBuffers[session.id] = '';
        }
        render();
        ensureTerminalPoll();
      } catch (err) {
        showToast('Terminal refresh failed: ' + err);
      }
    }

    async function startShellTerminal() {
      try {
        const size = terminalSize();
        const result = await terminalRequest('start', size);
        if (!result.session) throw new Error('missing session');
        activeTerminalId = result.session.id;
        terminalBuffers[activeTerminalId] = '';
        delete terminalAfterSeq[activeTerminalId];
        await loadTerminalSessions();
        setView('terminals');
        focusTerminalInput();
      } catch (err) {
        showToast('Start terminal failed: ' + err);
      }
    }

    async function startServiceTerminal(key) {
      const row = rowByKey(key);
      if (!row) return;
      if (isRemote(row)) {
        showToast('Remote embedded terminals are disabled in this MVP');
        return;
      }
      try {
        const size = terminalSize();
        const result = await terminalRequest('start', { ...size, service_id: row.id });
        if (!result.session) throw new Error('missing session');
        activeTerminalId = result.session.id;
        terminalBuffers[activeTerminalId] = '';
        delete terminalAfterSeq[activeTerminalId];
        await loadTerminalSessions();
        setView('terminals');
        focusTerminalInput();
      } catch (err) {
        showToast('Start service terminal failed: ' + err);
      }
    }

    function selectTerminal(id) {
      activeTerminalId = id;
      renderTerminals();
      ensureTerminalPoll();
      pollActiveTerminal();
    }

    async function pollActiveTerminal() {
      if (!activeTerminalId) return;
      try {
        const params = { session: activeTerminalId };
        if (terminalAfterSeq[activeTerminalId] !== undefined) {
          params.after = String(terminalAfterSeq[activeTerminalId]);
        }
        const result = await terminalRequest('read', params);
        const read = result.read;
        if (!read || !read.session) return;
        upsertTerminalSession(read.session);
        for (const chunk of read.chunks || []) {
          terminalAfterSeq[activeTerminalId] = chunk.seq;
          terminalBuffers[activeTerminalId] = (terminalBuffers[activeTerminalId] || '') + String(chunk.text || '');
        }
        trimTerminalBuffer(activeTerminalId);
        if (currentView === 'terminals') {
          renderTerminals();
        }
      } catch (_err) {
      }
    }

    function ensureTerminalPoll() {
      if (terminalPollTimer) return;
      terminalPollTimer = setInterval(() => {
        if (activeTerminalId) pollActiveTerminal();
      }, 450);
    }

    async function sendTerminalLine() {
      const input = document.getElementById('terminal-line');
      if (!input || !activeTerminalId) return;
      const value = input.value;
      input.value = '';
      await sendTerminalData(value + '\r');
      focusTerminalInput();
    }

    function terminalLineKeydown(event) {
      if (event.key === 'Enter') {
        event.preventDefault();
        sendTerminalLine();
      } else if (event.key === 'c' && event.ctrlKey) {
        event.preventDefault();
        sendTerminalControl('ctrl-c');
      } else if (event.key === 'd' && event.ctrlKey) {
        event.preventDefault();
        sendTerminalControl('ctrl-d');
      }
    }

    async function sendTerminalControl(kind) {
      if (kind === 'ctrl-c') await sendTerminalData('\x03');
      if (kind === 'ctrl-d') await sendTerminalData('\x04');
    }

    async function sendTerminalData(data) {
      if (!activeTerminalId) return;
      try {
        await terminalRequest('input', { session: activeTerminalId, data });
        await pollActiveTerminal();
      } catch (err) {
        showToast('Terminal input failed: ' + err);
      }
    }

    async function resizeActiveTerminal() {
      if (!activeTerminalId) return;
      try {
        const result = await terminalRequest('resize', { session: activeTerminalId, ...terminalSize() });
        if (result.session) upsertTerminalSession(result.session);
        showToast('Terminal resized');
      } catch (err) {
        showToast('Terminal resize failed: ' + err);
      }
    }

    async function stopActiveTerminal() {
      if (!activeTerminalId) return;
      try {
        const result = await terminalRequest('stop', { session: activeTerminalId });
        if (result.session) upsertTerminalSession(result.session);
        await pollActiveTerminal();
        renderTerminals();
      } catch (err) {
        showToast('Terminal stop failed: ' + err);
      }
    }

    async function terminalRequest(action, params = {}) {
      const query = new URLSearchParams(params);
      const response = await fetch('/api/terminal/' + action + '?' + query.toString(), authOptions({ method: 'POST', cache: 'no-store' }));
      const result = await response.json();
      if (!response.ok || !result.ok) throw new Error(result.message || 'failed');
      return result;
    }

    function upsertTerminalSession(session) {
      const index = terminalSessions.findIndex(candidate => candidate.id === session.id);
      if (index >= 0) terminalSessions[index] = session;
      else terminalSessions.push(session);
    }

    function terminalSize() {
      const output = document.getElementById('terminal-output');
      const width = output?.clientWidth || 900;
      const height = output?.clientHeight || 460;
      return {
        cols: String(Math.max(40, Math.min(180, Math.floor(width / 8)))),
        rows: String(Math.max(10, Math.min(80, Math.floor(height / 18)))),
      };
    }

    function trimTerminalBuffer(id) {
      const text = terminalBuffers[id] || '';
      if (text.length > 180000) terminalBuffers[id] = text.slice(text.length - 120000);
    }

    function cleanTerminalText(text) {
      return String(text || '')
        .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '')
        .replace(/\x1b\][^\x07]*(\x07|\x1b\\)/g, '')
        .replace(/\r\n/g, '\n')
        .replace(/\r/g, '\n');
    }

    function scrollTerminalToBottom() {
      const output = document.getElementById('terminal-output');
      if (output) output.scrollTop = output.scrollHeight;
    }

    function focusTerminalInput() {
      setTimeout(() => document.getElementById('terminal-line')?.focus(), 50);
    }

    function renderPorts(rows) {
      const target = document.getElementById('ports-view');
      if (!rows.length) {
        target.innerHTML = '<div class="empty">No ports reserved.</div>';
        return;
      }
      const cells = rows.map(row => {
        const key = rowKey(row);
        return `
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
          <td><button class="linklike" onclick="openServiceByKey('${escapeAttr(key)}')">${escapeHtml(row.url)}</button></td>
        </tr>`;
      }).join('');
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

    function openServiceByKey(key) {
      const row = rowByKey(key);
      if (!row) return;
      if (canDirectOpen(row)) {
        runDirectOpen(row);
        return;
      }
      runActionForRow(row, 'open');
    }

    function canDirectOpen(row) {
      if (!row || row.direct_open !== true || !row.url) return false;
      return serviceState(row.runtime_status).key === 'running';
    }

    async function runDirectOpen(row) {
      const status = document.getElementById('status');
      busyKey = rowKey(row) + ':open';
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

    function renameService(key) {
      const row = rowByKey(key);
      if (!row) return;
      const currentTitle = row?.title || row.id;
      const nextTitle = window.prompt('Rename service', currentTitle);
      if (nextTitle === null) return;
      const title = nextTitle.trim();
      if (!title) {
        showToast('Rename failed: title must not be empty');
        return;
      }
      runActionForRow(row, 'rename', { title });
    }

    function escapeHtml(value) {
      return String(value).replace(/[&<>"']/g, ch => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[ch]));
    }

    function escapeAttr(value) {
      return escapeHtml(value).replace(/`/g, '&#96;');
    }

    function runActionByKey(key, action, extra = {}) {
      const row = rowByKey(key);
      if (!row) return;
      runActionForRow(row, action, extra);
    }

    async function runActionForRow(row, action, extra = {}) {
      const status = document.getElementById('status');
      const key = rowKey(row);
      busyKey = key + ':' + action;
      render();
      status.textContent = action + ' ' + row.id + '...';
      try {
        const params = new URLSearchParams({ id: row.id, action, ...rowActionExtra(row), ...extra });
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

    function runActionThenOpenByKey(key, action, extra = {}) {
      const row = rowByKey(key);
      if (!row) return;
      runActionThenOpenForRow(row, action, extra);
    }

    async function runActionThenOpenForRow(row, action, extra = {}) {
      const status = document.getElementById('status');
      const key = rowKey(row);
      busyKey = key + ':' + action;
      render();
      status.textContent = action + ' ' + row.id + '...';
      try {
        const params = new URLSearchParams({ id: row.id, action, ...rowActionExtra(row), ...extra });
        const response = await fetch('/api/action?' + params.toString(), authOptions({ method: 'POST', cache: 'no-store' }));
        const result = await response.json();
        showToast(result.message || 'done');
        if (!response.ok || !result.ok) throw new Error(result.message || 'failed');
        await loadPorts();
        openServiceByKey(key);
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

    if (window.location.hash) setView(window.location.hash.slice(1));
    loadPorts();
    loadAgentPrompts();
    window.addEventListener('focus', refreshOnAttention);
    document.addEventListener('visibilitychange', refreshOnAttention);
  </script>
</body>
</html>
"##;
