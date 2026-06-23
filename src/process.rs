use crate::config::{
    service_cwd, service_log_path, service_pid_path, BridgeConfig, ServiceMode, TunnelMode,
};
use crate::state::{ServiceState, State, TunnelState};
use anyhow::{bail, Context, Result};
use std::fs::{self, OpenOptions};
use std::path::Path;
use std::process::{Command, Stdio};

fn command(program: &str) -> Command {
    crate::command::quiet_command(program)
}

pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        command("kill")
            .arg("-0")
            .arg(pid.to_string())
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
        let status = command("kill").arg(pid.to_string()).status()?;
        if !status.success() {
            bail!("kill failed for pid {pid}");
        }
    }
    #[cfg(windows)]
    {
        let status = command("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status()?;
        if !status.success() {
            bail!("taskkill failed for pid {pid}");
        }
    }
    Ok(())
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
    #[cfg(windows)]
    {
        let script = format!(
            "$p = Get-NetTCPConnection -LocalPort {port} -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty OwningProcess; if ($p) {{ $p }}"
        );
        let output = command("powershell")
            .args(["-NoProfile", "-Command", &script])
            .output()
            .with_context(|| format!("query Windows listener PID on port {port}"))?;
        if !output.status.success() {
            return Ok(None);
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.trim().parse::<u32>().ok()))
    }
    #[cfg(unix)]
    {
        if let Some(pid) = pid_from_lsof(port)? {
            return Ok(Some(pid));
        }
        pid_from_ss(port)
    }
}

pub fn service_listener_pid(cfg: &BridgeConfig) -> Option<u32> {
    service_listener_port(cfg).and_then(|port| pid_listening_on_port(port).ok().flatten())
}

fn service_listener_port(cfg: &BridgeConfig) -> Option<u16> {
    cfg.service
        .pid_port
        .or_else(|| port_from_pid_source(cfg.service.pid_source.as_deref()))
        .or(Some(cfg.port))
}

fn port_from_pid_source(source: Option<&str>) -> Option<u16> {
    source?
        .strip_prefix("port:")
        .and_then(|port| port.trim().parse().ok())
}

#[cfg(unix)]
fn pid_from_lsof(port: u16) -> Result<Option<u32>> {
    let output = match command("lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"])
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("run lsof for port {port}")),
    };
    if !output.status.success() {
        return Ok(None);
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.trim().parse::<u32>().ok()))
}

#[cfg(unix)]
fn pid_from_ss(port: u16) -> Result<Option<u32>> {
    let output = match command("ss").args(["-ltnp"]).output() {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).context("run ss"),
    };
    if !output.status.success() {
        return Ok(None);
    }
    let needle = format!(":{port} ");
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if !line.contains(&needle) {
            continue;
        }
        if let Some(pid_start) = line.find("pid=") {
            let after = &line[pid_start + 4..];
            let pid_text: String = after.chars().take_while(|ch| ch.is_ascii_digit()).collect();
            if let Ok(pid) = pid_text.parse::<u32>() {
                return Ok(Some(pid));
            }
        }
    }
    Ok(None)
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
    if let Some(pid) = read_pid_file(&pid_path) {
        if pid_alive(pid) {
            return Ok(pid);
        }
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
    let pid = child.id();
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
        let killed = kill_external_processes(cfg)?;
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
    if let Some(pid) = read_pid_file(&pid_path) {
        if pid_alive(pid) {
            kill_pid(pid)?;
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

fn kill_external_processes(cfg: &BridgeConfig) -> Result<Vec<u32>> {
    let mut candidates = Vec::new();
    if let Some(pid) = cfg.service.pid {
        candidates.push(pid);
    }
    if let Some(pid) = service_listener_pid(cfg) {
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
    let pid =
        start_ssh_tunnel_process(&args).with_context(|| format!("start ssh tunnel for {id}"))?;
    state.tunnels.insert(
        key,
        TunnelState {
            pid: Some(pid),
            mode: format!("{mode:?}"),
            local_port,
            peer: peer.to_string(),
            updated_at: Some(crate::time::now_iso()),
        },
    );
    Ok(pid)
}

#[cfg(windows)]
fn start_ssh_tunnel_process(args: &[String]) -> Result<u32> {
    let ps_args = args
        .iter()
        .map(|arg| powershell_single_quote(arg))
        .collect::<Vec<_>>()
        .join(", ");
    let script = format!(
        "$p = Start-Process -FilePath 'ssh' -ArgumentList @({ps_args}) -WindowStyle Hidden -PassThru; Start-Sleep -Milliseconds 1000; if ($p.HasExited) {{ exit $p.ExitCode }}; Write-Output $p.Id"
    );
    let output = command("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .output()
        .context("run PowerShell Start-Process for ssh tunnel")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        bail!("ssh tunnel process exited during startup: {detail}");
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.trim().parse::<u32>().ok())
        .context("PowerShell did not report ssh tunnel PID")
}

#[cfg(not(windows))]
fn start_ssh_tunnel_process(args: &[String]) -> Result<u32> {
    let child = command("ssh")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn ssh tunnel")?;
    Ok(child.id())
}

#[cfg(windows)]
fn powershell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
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
            if let Some(pid) = tunnel.pid {
                if pid_alive(pid) {
                    kill_pid(pid)?;
                }
            }
        }
    }
    Ok(())
}
