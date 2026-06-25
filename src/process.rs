use crate::config::{
    service_cwd, service_log_path, service_pid_path, BridgeConfig, ServiceMode, TunnelMode,
};
use crate::state::{ServiceState, State, TunnelState};
use anyhow::{bail, Context, Result};
use std::fs::{self, OpenOptions};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
#[cfg(windows)]
use std::time::SystemTime;
use std::time::{Duration, Instant};

fn command(program: &str) -> Command {
    crate::command::quiet_command(program)
}

pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        command("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        let filter = format!("PID eq {pid}");
        command("tasklist")
            .args(["/FI", &filter])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
}

pub fn kill_pid(pid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        let output = command("kill").arg(pid.to_string()).output()?;
        if !output.status.success() {
            bail!(
                "kill failed for {}: {}",
                describe_pid(pid),
                output_detail(&output)
            );
        }
    }
    #[cfg(windows)]
    {
        let output = command("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output()?;
        if !output.status.success() {
            bail!(
                "taskkill failed for {}: {}",
                describe_pid(pid),
                output_detail(&output)
            );
        }
    }
    Ok(())
}

fn output_detail(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    match (stdout.is_empty(), stderr.is_empty()) {
        (false, false) => format!("stdout: {stdout}; stderr: {stderr}"),
        (false, true) => stdout,
        (true, false) => stderr,
        (true, true) => format!("exit status {}", output.status),
    }
}

pub fn describe_pid(pid: u32) -> String {
    process_command_line(pid)
        .filter(|line| !line.trim().is_empty())
        .map(|line| format!("pid {pid} ({})", line.trim()))
        .unwrap_or_else(|| format!("pid {pid}"))
}

#[cfg(windows)]
fn process_command_line(pid: u32) -> Option<String> {
    let script = format!(
        "$p = Get-CimInstance Win32_Process -Filter \"ProcessId = {pid}\" -ErrorAction SilentlyContinue; if ($p) {{ if ($p.CommandLine) {{ $p.CommandLine }} elseif ($p.ExecutablePath) {{ $p.ExecutablePath }} else {{ $p.Name }} }}"
    );
    command("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(unix)]
fn process_command_line(pid: u32) -> Option<String> {
    command("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn run_shell_command(command_text: &str, cwd: Option<&Path>) -> Result<()> {
    let mut command = shell_command(command_text);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let status = command
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("run command `{command_text}`"))?;
    if !status.success() {
        bail!("command failed with status {status}: {command_text}");
    }
    Ok(())
}

#[cfg(windows)]
fn shell_command(command_text: &str) -> Command {
    let mut command = command("cmd");
    command.args(["/C", command_text]);
    command
}

#[cfg(not(windows))]
fn shell_command(command_text: &str) -> Command {
    let mut command = command("sh");
    command.args(["-lc", command_text]);
    command
}

pub fn start_windows_scheduled_task(
    task_name: &str,
    wrapper_path: &Path,
    cwd: Option<&Path>,
    start_command: &str,
    log_file: Option<&Path>,
) -> Result<()> {
    #[cfg(windows)]
    {
        write_windows_cmd_wrapper(wrapper_path, cwd, start_command, log_file)?;
        let start_time = windows_task_start_time();
        let action = format!("cmd.exe /d /c {}", quote_cmd_path(wrapper_path));
        let status = command("schtasks")
            .args([
                "/Create",
                "/TN",
                task_name,
                "/TR",
                &action,
                "/SC",
                "ONCE",
                "/ST",
                &start_time,
                "/F",
            ])
            .status()
            .with_context(|| format!("create scheduled task `{task_name}`"))?;
        if !status.success() {
            bail!("create scheduled task `{task_name}` failed with status {status}");
        }
        let status = command("schtasks")
            .args(["/Run", "/TN", task_name])
            .status()
            .with_context(|| format!("run scheduled task `{task_name}`"))?;
        if !status.success() {
            bail!("run scheduled task `{task_name}` failed with status {status}");
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (task_name, wrapper_path, cwd, start_command, log_file);
        bail!("--detach scheduled-task is only supported on Windows")
    }
}

pub fn end_windows_scheduled_task(task_name: &str) -> Result<()> {
    #[cfg(windows)]
    {
        let status = command("schtasks")
            .args(["/End", "/TN", task_name])
            .status()
            .with_context(|| format!("end scheduled task `{task_name}`"))?;
        if !status.success() {
            bail!("end scheduled task `{task_name}` failed with status {status}");
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = task_name;
        bail!("Windows scheduled tasks are not supported on this platform")
    }
}

pub fn delete_windows_scheduled_task(task_name: &str) -> Result<()> {
    #[cfg(windows)]
    {
        let status = command("schtasks")
            .args(["/Delete", "/TN", task_name, "/F"])
            .status()
            .with_context(|| format!("delete scheduled task `{task_name}`"))?;
        if !status.success() {
            bail!("delete scheduled task `{task_name}` failed with status {status}");
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = task_name;
        bail!("Windows scheduled tasks are not supported on this platform")
    }
}

#[cfg(windows)]
fn write_windows_cmd_wrapper(
    wrapper_path: &Path,
    cwd: Option<&Path>,
    start_command: &str,
    log_file: Option<&Path>,
) -> Result<()> {
    if let Some(parent) = wrapper_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut lines = Vec::new();
    lines.push("@echo off".to_string());
    lines.push("chcp 65001 >NUL".to_string());
    if let Some(log_file) = log_file {
        if let Some(parent) = log_file.parent() {
            lines.push(format!(
                "if not exist {} mkdir {}",
                quote_cmd_path(parent),
                quote_cmd_path(parent)
            ));
        }
    }
    if let Some(cwd) = cwd {
        lines.push(format!("cd /d {} || exit /b 1", quote_cmd_path(cwd)));
    }
    let command = if let Some(log_file) = log_file {
        format!("{} >> {} 2>&1", start_command, quote_cmd_path(log_file))
    } else {
        start_command.to_string()
    };
    lines.push(command);
    fs::write(wrapper_path, lines.join("\r\n") + "\r\n")
        .with_context(|| format!("write {}", wrapper_path.display()))?;
    Ok(())
}

#[cfg(windows)]
fn windows_task_start_time() -> String {
    let script = "(Get-Date).AddMinutes(5).ToString('HH:mm')";
    command("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (!text.is_empty()).then_some(text)
        })
        .unwrap_or_else(|| "23:59".to_string())
}

#[cfg(windows)]
fn quote_cmd_path(path: &Path) -> String {
    format!("\"{}\"", path.display())
}

pub fn pid_listening_on_port(port: u16) -> Result<Option<u32>> {
    Ok(pids_listening_on_port(port)?.into_iter().next())
}

pub fn tcp_port_open(port: u16) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(150)).is_ok()
}

pub fn pids_listening_on_port(port: u16) -> Result<Vec<u32>> {
    #[cfg(windows)]
    {
        let script = format!(
            "Get-NetTCPConnection -LocalPort {port} -State Listen -ErrorAction SilentlyContinue | Select-Object -ExpandProperty OwningProcess -Unique"
        );
        let output = command("powershell")
            .args(["-NoProfile", "-Command", &script])
            .output()
            .with_context(|| format!("query Windows listener PIDs on port {port}"))?;
        if !output.status.success() {
            return Ok(Vec::new());
        }
        let mut pids = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
            .collect::<Vec<_>>();
        pids.sort_unstable();
        pids.dedup();
        Ok(pids)
    }
    #[cfg(unix)]
    {
        let mut pids = pids_from_lsof(port)?;
        pids.extend(pids_from_ss(port)?);
        pids.sort_unstable();
        pids.dedup();
        Ok(pids)
    }
}

pub fn service_listener_pid(cfg: &BridgeConfig) -> Option<u32> {
    service_listener_port(cfg).and_then(|port| pid_listening_on_port(port).ok().flatten())
}

fn service_listener_pids(cfg: &BridgeConfig) -> Vec<u32> {
    service_listener_port(cfg)
        .and_then(|port| pids_listening_on_port(port).ok())
        .unwrap_or_default()
}

fn service_listener_port(cfg: &BridgeConfig) -> Option<u16> {
    cfg.service
        .pid_port
        .or_else(|| port_from_pid_source(cfg.service.pid_source.as_deref()))
        .or(Some(cfg.port))
}

fn configured_listener_port(cfg: &BridgeConfig) -> u16 {
    service_listener_port(cfg).unwrap_or(cfg.port)
}

pub fn managed_service_status(cfg: &BridgeConfig) -> String {
    let pid_file_pid = service_pid_path(cfg).and_then(|path| read_pid_file(&path));
    let pid_file_alive = pid_file_pid.map(pid_alive).unwrap_or(false);
    let listener_pids = service_listener_pids(cfg);
    if listener_pids.len() > 1 {
        return match (pid_file_pid, pid_file_alive) {
            (Some(pid), true) => {
                format!("multi-listener:{};pid_file:{pid}", pid_list(&listener_pids))
            }
            (Some(pid), false) => format!("stale:{pid};listeners:{}", pid_list(&listener_pids)),
            (None, _) => format!("multi-listener:{}", pid_list(&listener_pids)),
        };
    }
    let listener_pid = listener_pids.first().copied();
    match (pid_file_pid, pid_file_alive, listener_pid) {
        (Some(pid), true, Some(listener)) if pid == listener => format!("running:{pid}"),
        (Some(pid), true, Some(listener)) => {
            format!("pid-mismatch:{pid};listener:{listener}")
        }
        (Some(pid), true, None) => format!("no-listener:{pid}"),
        (Some(pid), false, Some(listener)) => format!("stale:{pid};listener:{listener}"),
        (Some(pid), false, None) => format!("stale:{pid}"),
        (None, _, Some(listener)) => format!("port-owned:{listener}"),
        (None, _, None) => "stopped".into(),
    }
}

pub fn managed_service_alive(cfg: &BridgeConfig) -> bool {
    managed_service_status(cfg).starts_with("running:")
}

fn pid_list(pids: &[u32]) -> String {
    pids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn describe_pid_list(pids: &[u32]) -> String {
    pids.iter()
        .map(|pid| describe_pid(*pid))
        .collect::<Vec<_>>()
        .join("; ")
}

fn port_from_pid_source(source: Option<&str>) -> Option<u16> {
    source?
        .strip_prefix("port:")
        .and_then(|port| port.trim().parse().ok())
}

#[cfg(unix)]
fn pids_from_lsof(port: u16) -> Result<Vec<u32>> {
    let output = match command("lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"])
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("run lsof for port {port}")),
    };
    if !output.status.success() {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .collect())
}

#[cfg(unix)]
fn pids_from_ss(port: u16) -> Result<Vec<u32>> {
    let output = match command("ss").args(["-ltnp"]).output() {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).context("run ss"),
    };
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let needle = format!(":{port} ");
    let mut pids = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if !line.contains(&needle) {
            continue;
        }
        if let Some(pid_start) = line.find("pid=") {
            let after = &line[pid_start + 4..];
            let pid_text: String = after.chars().take_while(|ch| ch.is_ascii_digit()).collect();
            if let Ok(pid) = pid_text.parse::<u32>() {
                pids.push(pid);
            }
        }
    }
    Ok(pids)
}

pub fn read_pid_file(path: &Path) -> Option<u32> {
    let text = fs::read_to_string(path).ok()?;
    text.trim().parse().ok()
}

pub fn start_service(cfg: &BridgeConfig, state: &mut State) -> Result<u32> {
    if cfg.service.mode != ServiceMode::Managed {
        bail!(
            "service `{}` is external; Bridgeboard only records it",
            cfg.id
        );
    }
    let pid_path = service_pid_path(cfg).context("managed service pid_file is required")?;
    let pid_file_pid = read_pid_file(&pid_path);
    let live_pid_file_pid = pid_file_pid.filter(|pid| pid_alive(*pid));
    let listener_pids = service_listener_pids(cfg);
    let listener_port = configured_listener_port(cfg);
    match live_pid_file_pid {
        Some(pid) if listener_pids.len() == 1 && listener_pids[0] == pid => return Ok(pid),
        Some(pid) if listener_pids.is_empty() => bail!(
            "service `{}` pid_file points to live {}, but configured port {} is not listening; run `bridgeboard restart {}` or clear the stale process/pid_file",
            cfg.id,
            describe_pid(pid),
            listener_port,
            cfg.id
        ),
        Some(pid) => bail!(
            "service `{}` pid_file points to live {}, but configured port {} is owned by {}; refusing to start over mismatched listener(s)",
            cfg.id,
            describe_pid(pid),
            listener_port,
            describe_pid_list(&listener_pids)
        ),
        None if !listener_pids.is_empty() => bail!(
            "service `{}` cannot start because configured port {} is already owned by {}; stop those process(es) or choose another fixed port",
            cfg.id,
            listener_port,
            describe_pid_list(&listener_pids)
        ),
        None => {}
    }

    let log_path = service_log_path(cfg).context("managed service log_file is required")?;
    let cwd = service_cwd(cfg).context("managed service cwd is required")?;
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = pid_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open log {}", log_path.display()))?;
    let log2 = log.try_clone()?;

    let mut command = command(&cfg.service.command[0]);
    command
        .args(&cfg.service.command[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2));
    let child = command
        .spawn()
        .with_context(|| format!("start service `{}`", cfg.id))?;
    let child_pid = child.id();
    let pid = wait_for_managed_listener(cfg, child_pid, &log_path)?;
    fs::write(&pid_path, format!("{pid}\n"))?;
    let desired = state.services.get(&cfg.id).and_then(|entry| entry.desired);
    state.services.insert(
        cfg.id.clone(),
        ServiceState {
            pid: Some(pid),
            last_health: None,
            last_status: Some("started".into()),
            updated_at: Some(crate::time::now_iso()),
            desired,
            pid_source: None,
            pid_port: None,
        },
    );
    Ok(pid)
}

pub fn stop_service(cfg: &BridgeConfig, state: &mut State) -> Result<()> {
    if cfg.service.mode != ServiceMode::Managed {
        let mut stopped_by = Vec::new();
        if let Some(command) = cfg.service.stop_command.as_deref() {
            run_shell_command(command, service_cwd(cfg))?;
            stopped_by.push("command".to_string());
        } else if let Some(task_name) = cfg.service.task_name.as_deref() {
            if cfg!(windows) {
                end_windows_scheduled_task(task_name)?;
                stopped_by.push("task".to_string());
            }
        }
        let killed = if external_stop_may_kill_processes(cfg) {
            kill_external_processes(cfg)?
        } else {
            Vec::new()
        };
        if !killed.is_empty() {
            stopped_by.push(format!(
                "killed-pid:{}",
                killed
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        let last_status = if stopped_by.is_empty() {
            "external-not-stopped".to_string()
        } else {
            format!("external-stopped:{}", stopped_by.join(";"))
        };
        let desired = state.services.get(&cfg.id).and_then(|entry| entry.desired);
        state.services.insert(
            cfg.id.clone(),
            ServiceState {
                pid: None,
                last_health: None,
                last_status: Some(last_status),
                updated_at: Some(crate::time::now_iso()),
                desired,
                pid_source: cfg.service.pid_source.clone(),
                pid_port: cfg.service.pid_port,
            },
        );
        return Ok(());
    }
    let pid_path = service_pid_path(cfg).context("managed service pid_file is required")?;
    let mut candidates = Vec::new();
    if let Some(pid) = read_pid_file(&pid_path) {
        candidates.push((pid, "pid_file"));
    }
    for pid in service_listener_pids(cfg) {
        candidates.push((pid, "listener"));
    }
    candidates.sort_by_key(|(pid, _)| *pid);
    candidates.dedup_by_key(|(pid, _)| *pid);
    for (pid, source) in candidates {
        if pid_alive(pid) {
            kill_pid(pid).with_context(|| {
                format!(
                    "stop managed service `{}` {} {}",
                    cfg.id,
                    source,
                    describe_pid(pid)
                )
            })?;
        }
    }
    let _ = fs::remove_file(&pid_path);
    let desired = state.services.get(&cfg.id).and_then(|entry| entry.desired);
    state.services.insert(
        cfg.id.clone(),
        ServiceState {
            pid: None,
            last_health: None,
            last_status: Some("stopped".into()),
            updated_at: Some(crate::time::now_iso()),
            desired,
            pid_source: None,
            pid_port: None,
        },
    );
    Ok(())
}

fn external_stop_may_kill_processes(cfg: &BridgeConfig) -> bool {
    cfg.service.stop_command.is_some() || cfg.service.task_name.is_some()
}

pub fn reconcile_managed_listener_pid(
    cfg: &BridgeConfig,
    state: &mut State,
) -> Result<Option<u32>> {
    if cfg.service.mode != ServiceMode::Managed {
        return Ok(None);
    }
    let listener_pids = service_listener_pids(cfg);
    if listener_pids.len() != 1 {
        return Ok(None);
    }
    let listener_pid = listener_pids[0];
    let pid_path = service_pid_path(cfg).context("managed service pid_file is required")?;
    if read_pid_file(&pid_path) == Some(listener_pid) {
        return Ok(None);
    }
    fs::write(&pid_path, format!("{listener_pid}\n"))?;
    let entry = state.services.entry(cfg.id.clone()).or_default();
    entry.pid = Some(listener_pid);
    entry.pid_source = None;
    entry.pid_port = None;
    entry.updated_at = Some(crate::time::now_iso());
    Ok(Some(listener_pid))
}

fn wait_for_managed_listener(cfg: &BridgeConfig, child_pid: u32, log_path: &Path) -> Result<u32> {
    let timeout = Duration::from_secs(cfg.service.startup_timeout_sec.max(1));
    let deadline = Instant::now() + timeout;
    let listener_port = configured_listener_port(cfg);
    loop {
        let listener_pids = service_listener_pids(cfg);
        if listener_pids.len() == 1 {
            return Ok(listener_pids[0]);
        }
        if listener_pids.len() > 1 {
            bail!(
                "service `{}` found multiple listeners on configured port {} after startup: {}. {}",
                cfg.id,
                listener_port,
                describe_pid_list(&listener_pids),
                log_tail_summary(log_path)
            );
        }
        if !pid_alive(child_pid) {
            bail!(
                "service `{}` process {} exited before configured port {} started listening. {}",
                cfg.id,
                describe_pid(child_pid),
                listener_port,
                log_tail_summary(log_path)
            );
        }
        if Instant::now() >= deadline {
            bail!(
                "service `{}` process {} did not listen on configured port {} within {}s. {}",
                cfg.id,
                describe_pid(child_pid),
                listener_port,
                timeout.as_secs(),
                log_tail_summary(log_path)
            );
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn log_tail_summary(path: &Path) -> String {
    let Ok(bytes) = fs::read(path) else {
        return format!("log unavailable: {}", path.display());
    };
    let text = String::from_utf8_lossy(&bytes).replace('\0', "");
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return format!("log is empty: {}", path.display());
    }
    let start = lines.len().saturating_sub(20);
    format!(
        "last log lines from {}:\n{}",
        path.display(),
        lines[start..].join("\n")
    )
}

fn kill_external_processes(cfg: &BridgeConfig) -> Result<Vec<u32>> {
    let mut candidates = Vec::new();
    if let Some(pid) = cfg.service.pid {
        candidates.push(pid);
    }
    for pid in service_listener_pids(cfg) {
        candidates.push(pid);
    }
    candidates.sort_unstable();
    candidates.dedup();

    let mut killed = Vec::new();
    for pid in candidates {
        if pid_alive(pid) {
            kill_pid(pid)
                .with_context(|| format!("stop external service `{}` pid {pid}", cfg.id))?;
            killed.push(pid);
        }
    }
    Ok(killed)
}

pub fn tunnel_key(id: &str, mode: TunnelMode, peer: &str) -> String {
    let mode = tunnel_mode_key(mode);
    format!("{id}:{mode}:{peer}")
}

fn tunnel_key_for_port(
    id: &str,
    mode: TunnelMode,
    peer: &str,
    local_port: u16,
    remote_port: u16,
) -> String {
    if local_port == remote_port {
        tunnel_key(id, mode, peer)
    } else {
        let mode = tunnel_mode_key(mode);
        format!("{id}:{mode}:{peer}:{local_port}")
    }
}

fn tunnel_mode_key(mode: TunnelMode) -> &'static str {
    let mode = match mode {
        TunnelMode::LocalForward => "local",
        TunnelMode::ReverseForward => "reverse",
    };
    mode
}

pub fn start_tunnel(
    cfg: &BridgeConfig,
    mode: TunnelMode,
    peer: &str,
    ssh_alias: &str,
    state: &mut State,
) -> Result<u32> {
    start_tunnel_spec(
        &cfg.id,
        cfg.port,
        cfg.port,
        &cfg.tunnel.bind_host,
        mode,
        peer,
        ssh_alias,
        state,
    )
}

pub fn start_tunnel_spec(
    id: &str,
    remote_port: u16,
    local_port: u16,
    bind_host: &str,
    mode: TunnelMode,
    peer: &str,
    ssh_alias: &str,
    state: &mut State,
) -> Result<u32> {
    let key = tunnel_key_for_port(id, mode, peer, local_port, remote_port);
    if let Some(existing) = state.tunnels.get(&key).and_then(|t| t.pid) {
        if pid_alive(existing) {
            return Ok(existing);
        }
    }

    let spec = match mode {
        TunnelMode::LocalForward => format!("{bind_host}:{local_port}:127.0.0.1:{remote_port}"),
        TunnelMode::ReverseForward => format!("{bind_host}:{local_port}:127.0.0.1:{remote_port}"),
    };
    let flag = match mode {
        TunnelMode::LocalForward => "-L",
        TunnelMode::ReverseForward => "-R",
    };
    let args = vec![
        "-o".to_string(),
        "ExitOnForwardFailure=yes".to_string(),
        "-o".to_string(),
        "ServerAliveInterval=30".to_string(),
        "-o".to_string(),
        "ServerAliveCountMax=2".to_string(),
        "-N".to_string(),
        flag.to_string(),
        spec,
        ssh_alias.to_string(),
    ];
    let (pid, task_name) = start_ssh_tunnel_process(&args, id, mode, peer, local_port, remote_port)
        .with_context(|| format!("start ssh tunnel for {id}"))?;
    state.tunnels.insert(
        key,
        TunnelState {
            pid: Some(pid),
            mode: format!("{mode:?}"),
            local_port,
            peer: peer.to_string(),
            task_name,
            updated_at: Some(crate::time::now_iso()),
        },
    );
    Ok(pid)
}

#[cfg(windows)]
fn start_ssh_tunnel_process(
    args: &[String],
    id: &str,
    mode: TunnelMode,
    peer: &str,
    local_port: u16,
    remote_port: u16,
) -> Result<(u32, Option<String>)> {
    let task_name = tunnel_task_name(id, mode, peer, local_port, remote_port);
    let safe_name = safe_task_file_stem(&task_name);
    let temp_dir = std::env::temp_dir();
    let wrapper_path = temp_dir.join(format!("{safe_name}.cmd"));
    let log_file = temp_dir.join(format!("{safe_name}.log"));
    let start_command = format!(
        "ssh {}",
        args.iter()
            .map(|arg| quote_cmd_arg(arg))
            .collect::<Vec<_>>()
            .join(" ")
    );

    end_windows_scheduled_task_quiet(&task_name);
    delete_windows_scheduled_task_quiet(&task_name);
    start_windows_scheduled_task(
        &task_name,
        &wrapper_path,
        None,
        &start_command,
        Some(&log_file),
    )?;

    let pid = wait_for_tunnel_pid(args, mode, local_port, Duration::from_secs(5))?.with_context(
        || {
            format!(
                "ssh tunnel did not become active; log: {}",
                read_lossy(&log_file)
            )
        },
    )?;
    Ok((pid, Some(task_name)))
}

#[cfg(not(windows))]
fn start_ssh_tunnel_process(
    args: &[String],
    id: &str,
    mode: TunnelMode,
    _peer: &str,
    local_port: u16,
    _remote_port: u16,
) -> Result<(u32, Option<String>)> {
    let mut bg_args = Vec::with_capacity(args.len() + 1);
    bg_args.push("-f".to_string());
    bg_args.extend(args.iter().cloned());
    let output = command("ssh")
        .args(&bg_args)
        .stdin(Stdio::null())
        .output()
        .context("spawn ssh tunnel")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        bail!("ssh tunnel command failed: {detail}");
    }
    let pid = wait_for_unix_tunnel_pid(args, mode, local_port, Duration::from_secs(5))?
        .with_context(|| format!("ssh tunnel for {id} did not become active"))?;
    Ok((pid, None))
}

#[cfg(not(windows))]
fn wait_for_unix_tunnel_pid(
    args: &[String],
    mode: TunnelMode,
    local_port: u16,
    timeout: Duration,
) -> Result<Option<u32>> {
    let deadline = Instant::now() + timeout;
    loop {
        if mode == TunnelMode::LocalForward {
            if let Some(pid) = pid_listening_on_port(local_port)? {
                return Ok(Some(pid));
            }
            if tcp_port_open(local_port) {
                if let Some(pid) = matching_ssh_tunnel_pid(args)? {
                    return Ok(Some(pid));
                }
            }
        } else if let Some(pid) = matching_ssh_tunnel_pid(args)? {
            return Ok(Some(pid));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(not(windows))]
fn matching_ssh_tunnel_pid(args: &[String]) -> Result<Option<u32>> {
    let output = command("ps")
        .args(["-eo", "pid=,args="])
        .output()
        .context("query ssh tunnel PID")?;
    if !output.status.success() {
        return Ok(None);
    }
    let needles = args
        .iter()
        .filter(|arg| arg.len() > 2 && *arg != "-N")
        .collect::<Vec<_>>();
    let mut matched = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let trimmed = line.trim_start();
        let Some((pid_text, command_line)) = trimmed.split_once(char::is_whitespace) else {
            continue;
        };
        if !command_line.contains("ssh") {
            continue;
        }
        if needles.iter().all(|needle| command_line.contains(*needle)) {
            if let Ok(pid) = pid_text.parse::<u32>() {
                matched = Some(pid);
            }
        }
    }
    Ok(matched)
}

#[cfg(windows)]
fn powershell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(windows)]
fn tunnel_task_name(
    id: &str,
    mode: TunnelMode,
    peer: &str,
    local_port: u16,
    remote_port: u16,
) -> String {
    let raw = format!(
        "BridgeboardTunnel-{id}-{}-{peer}-{local_port}-{remote_port}",
        tunnel_mode_key(mode)
    );
    let safe = safe_task_file_stem(&raw);
    if safe.len() <= 180 {
        safe
    } else {
        format!(
            "{}-{local_port}-{remote_port}",
            safe.chars().take(150).collect::<String>()
        )
    }
}

#[cfg(windows)]
fn safe_task_file_stem(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(windows)]
fn quote_cmd_arg(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._=:/@".contains(c))
    {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('"', "\\\""))
    }
}

#[cfg(windows)]
fn wait_for_tunnel_pid(
    args: &[String],
    mode: TunnelMode,
    local_port: u16,
    timeout: Duration,
) -> Result<Option<u32>> {
    let deadline = SystemTime::now() + timeout;
    loop {
        if mode == TunnelMode::LocalForward {
            if let Some(pid) = pid_listening_on_port(local_port)? {
                return Ok(Some(pid));
            }
        }
        if let Some(pid) = matching_ssh_tunnel_pid(args)? {
            return Ok(Some(pid));
        }
        if SystemTime::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(windows)]
fn matching_ssh_tunnel_pid(args: &[String]) -> Result<Option<u32>> {
    let needles = args
        .iter()
        .filter(|arg| arg.len() > 2 && *arg != "-N")
        .map(|arg| powershell_single_quote(arg))
        .collect::<Vec<_>>()
        .join(", ");
    let script = format!(
        "$needles = @({needles}); Get-CimInstance Win32_Process -Filter \"Name = 'ssh.exe'\" | Where-Object {{ $cmd = $_.CommandLine; $cmd -and (@($needles | Where-Object {{ $cmd -notlike \"*$($_)*\" }}).Count -eq 0) }} | Sort-Object CreationDate -Descending | Select-Object -First 1 -ExpandProperty ProcessId"
    );
    let output = command("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .context("query Windows ssh tunnel PID")?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.trim().parse::<u32>().ok()))
}

#[cfg(windows)]
fn read_lossy(path: &Path) -> String {
    fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_string())
        .unwrap_or_default()
}

#[cfg(windows)]
fn end_windows_scheduled_task_quiet(task_name: &str) {
    let _ = command("schtasks")
        .args(["/End", "/TN", task_name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(windows)]
fn delete_windows_scheduled_task_quiet(task_name: &str) {
    let _ = command("schtasks")
        .args(["/Delete", "/TN", task_name, "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

pub fn stop_tunnels_for(id: &str, state: &mut State) -> Result<()> {
    let keys: Vec<String> = state
        .tunnels
        .keys()
        .filter(|key| key.starts_with(&format!("{id}:")))
        .cloned()
        .collect();
    for key in keys {
        if let Some(tunnel) = state.tunnels.remove(&key) {
            if let Some(task_name) = tunnel.task_name.as_deref() {
                let _ = end_windows_scheduled_task(task_name);
                let _ = delete_windows_scheduled_task(task_name);
            }
            if let Some(pid) = tunnel.pid {
                if pid_alive(pid) {
                    kill_pid(pid)?;
                }
            }
        }
    }
    Ok(())
}

pub fn stop_tunnels_for_peer(id: &str, peer: &str, state: &mut State) -> Result<()> {
    let keys: Vec<String> = state
        .tunnels
        .iter()
        .filter(|(key, tunnel)| key.starts_with(&format!("{id}:")) && tunnel.peer == peer)
        .map(|(key, _)| key.clone())
        .collect();
    for key in keys {
        if let Some(tunnel) = state.tunnels.remove(&key) {
            if let Some(task_name) = tunnel.task_name.as_deref() {
                let _ = end_windows_scheduled_task(task_name);
                let _ = delete_windows_scheduled_task(task_name);
            }
            if let Some(pid) = tunnel.pid {
                if pid_alive(pid) {
                    kill_pid(pid)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HealthExpectConfig, LifecycleConfig, ServiceConfig, TunnelConfig};

    fn external_cfg() -> BridgeConfig {
        BridgeConfig {
            schema: "portal-bridge.v1".into(),
            id: "observed-service".into(),
            title: "Observed Service".into(),
            owner_host: "workstation".into(),
            port: 24510,
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
                pid_source: Some("port:24510".into()),
                pid_port: Some(24510),
                pid_file: None,
                pid: Some(12345),
                log_file: None,
                health_url: None,
                health_expect: HealthExpectConfig::default(),
                startup_timeout_sec: 5,
                notes: None,
            },
            tunnel: TunnelConfig::default(),
            local_url: None,
            network_url: None,
            open_url: None,
        }
    }

    #[test]
    fn record_only_external_stop_does_not_kill_observed_pid() {
        let cfg = external_cfg();
        assert!(!external_stop_may_kill_processes(&cfg));
    }

    #[test]
    fn controlled_external_stop_may_clean_child_processes() {
        let mut cfg = external_cfg();
        cfg.service.stop_command = Some("stop-observed-service".into());
        assert!(external_stop_may_kill_processes(&cfg));

        let mut cfg = external_cfg();
        cfg.service.task_name = Some("Bridgeboard-observed-service".into());
        assert!(external_stop_may_kill_processes(&cfg));
    }
}
