use crate::config::AppConfig;
use crate::registry::RegistryExport;
use anyhow::Result;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub fn fetch_peer_exports(app: &AppConfig) -> Vec<(String, Result<RegistryExport, String>)> {
    app.peers
        .iter()
        .map(|(name, peer)| {
            let ssh_alias = peer.ssh_alias.as_deref().unwrap_or(name);
            let bridgeboard_bin = peer.bridgeboard_bin.as_deref().unwrap_or("bridgeboard");
            let result = peer_registry_output(ssh_alias, bridgeboard_bin).and_then(|out| {
                if !out.status.success() {
                    return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
                }
                serde_json::from_slice::<RegistryExport>(&out.stdout).map_err(|e| e.to_string())
            });
            (name.clone(), result)
        })
        .collect()
}

pub fn run_bridgeboard_command(
    app: &AppConfig,
    host: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<Output, String> {
    let peer = app.peers.get(host);
    let ssh_alias = peer
        .and_then(|cfg| cfg.ssh_alias.as_deref())
        .unwrap_or(host);
    let bridgeboard_bin = peer
        .and_then(|cfg| cfg.bridgeboard_bin.as_deref())
        .unwrap_or("bridgeboard");
    peer_command_output(ssh_alias, bridgeboard_bin, args, timeout)
}

#[cfg(windows)]
fn peer_registry_output(ssh_alias: &str, bridgeboard_bin: &str) -> Result<Output, String> {
    peer_command_output(
        ssh_alias,
        bridgeboard_bin,
        &["registry", "export", "--json"],
        Duration::from_secs(12),
    )
}

#[cfg(windows)]
fn peer_command_output(
    ssh_alias: &str,
    bridgeboard_bin: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<Output, String> {
    let mut command = crate::command::quiet_command("ssh");
    command
        .args(["-n", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5"])
        .arg(ssh_alias)
        .arg(remote_command_line(bridgeboard_bin, args));
    output_with_timeout(command, timeout)
}

#[cfg(not(windows))]
fn peer_registry_output(ssh_alias: &str, bridgeboard_bin: &str) -> Result<Output, String> {
    peer_command_output(
        ssh_alias,
        bridgeboard_bin,
        &["registry", "export", "--json"],
        Duration::from_secs(12),
    )
}

#[cfg(not(windows))]
fn peer_command_output(
    ssh_alias: &str,
    bridgeboard_bin: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<Output, String> {
    let mut command = crate::command::quiet_command("ssh");
    command
        .args(["-n", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5"])
        .arg(ssh_alias)
        .arg(remote_command_line(bridgeboard_bin, args));
    output_with_timeout(command, timeout)
}

fn remote_command_line(bridgeboard_bin: &str, args: &[&str]) -> String {
    std::iter::once(bridgeboard_bin)
        .chain(args.iter().copied())
        .map(remote_arg_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

fn remote_arg_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

fn output_with_timeout(mut command: Command, timeout: Duration) -> Result<Output, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    loop {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            return child.wait_with_output().map_err(|e| e.to_string());
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "peer registry query timed out after {}s",
                timeout.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::remote_command_line;

    #[test]
    fn remote_command_quotes_space_containing_args() {
        assert_eq!(
            remote_command_line(
                "bridgeboard",
                &["rename", "model-inspector", "--title", "Model Inspector Dashboard"]
            ),
            "\"bridgeboard\" \"rename\" \"model-inspector\" \"--title\" \"Model Inspector Dashboard\""
        );
    }

    #[test]
    fn remote_command_escapes_double_quotes() {
        assert_eq!(
            remote_command_line(
                "bridgeboard",
                &["rename", "id", "--title", "A \"quoted\" title"]
            ),
            "\"bridgeboard\" \"rename\" \"id\" \"--title\" \"A \\\"quoted\\\" title\""
        );
    }
}

pub fn ssh_alias_for<'a>(app: &'a AppConfig, host: &'a str) -> &'a str {
    app.peers
        .get(host)
        .and_then(|peer| peer.ssh_alias.as_deref())
        .unwrap_or(host)
}

pub fn print_peer_warnings(results: &[(String, Result<RegistryExport, String>)]) {
    for (name, result) in results {
        if let Err(err) = result {
            eprintln!("warning: could not query peer `{name}`: {err}");
        }
    }
}
