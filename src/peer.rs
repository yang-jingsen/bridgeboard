use crate::config::AppConfig;
use crate::registry::RegistryExport;
use anyhow::Result;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub fn fetch_peer_exports(app: &AppConfig) -> Vec<(String, Result<RegistryExport, String>)> {
    let mut handles = Vec::new();
    for (name, peer) in &app.peers {
        let name = name.clone();
        let ssh_alias = peer.ssh_alias.clone().unwrap_or_else(|| name.clone());
        let bridgeboard_bin = peer
            .bridgeboard_bin
            .clone()
            .unwrap_or_else(|| "bridgeboard".into());
        let handle = thread::spawn(move || {
            let result = peer_registry_output(&ssh_alias, &bridgeboard_bin).and_then(|out| {
                if !out.status.success() {
                    return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
                }
                serde_json::from_slice::<RegistryExport>(&out.stdout).map_err(|e| e.to_string())
            });
            (name, result)
        });
        handles.push(handle);
    }
    handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .unwrap_or_else(|_| ("unknown".into(), Err("peer query thread panicked".into())))
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
    format!(
        "{} exec-encoded {}",
        remote_arg_quote(bridgeboard_bin),
        encode_remote_args(args)
    )
}

fn remote_arg_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

pub fn encode_remote_args(args: &[&str]) -> String {
    let bytes = serde_json::to_vec(args).expect("remote args are JSON-serializable");
    hex_encode(&bytes)
}

pub fn decode_remote_args(payload: &str) -> Result<Vec<String>, String> {
    let bytes = hex_decode(payload)?;
    serde_json::from_slice::<Vec<String>>(&bytes).map_err(|err| err.to_string())
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

fn hex_decode(payload: &str) -> Result<Vec<u8>, String> {
    if payload.len() % 2 != 0 {
        return Err("encoded argument payload has odd length".into());
    }
    let mut bytes = Vec::with_capacity(payload.len() / 2);
    let chars = payload.as_bytes();
    let mut i = 0;
    while i < chars.len() {
        let high = hex_value(chars[i])?;
        let low = hex_value(chars[i + 1])?;
        bytes.push((high << 4) | low);
        i += 2;
    }
    Ok(bytes)
}

fn hex_value(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err("encoded argument payload contains non-hex characters".into()),
    }
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
    use super::{decode_remote_args, encode_remote_args, remote_command_line};

    #[test]
    fn remote_command_quotes_space_containing_args() {
        assert_eq!(
            remote_command_line(
                "bridgeboard",
                &["rename", "model-inspector", "--title", "Model Inspector Dashboard"]
            ),
            "\"bridgeboard\" exec-encoded 5b2272656e616d65222c226d6f64656c2d696e73706563746f72222c222d2d7469746c65222c224d6f64656c20496e73706563746f722044617368626f617264225d"
        );
    }

    #[test]
    fn remote_command_does_not_expose_shell_metacharacters_from_args() {
        let line = remote_command_line(
            "bridgeboard",
            &[
                "rename",
                "id",
                "--title",
                "A \"quoted\" $(touch /tmp/pwn) `x` title",
            ],
        );
        assert!(line.starts_with("\"bridgeboard\" exec-encoded "));
        assert!(!line.contains("touch"));
        assert!(!line.contains("$("));
        assert!(!line.contains('`'));
    }

    #[test]
    fn remote_args_round_trip_through_hex_json() {
        let args = ["rename", "id", "--title", "A \"quoted\" $(safe) title"];
        assert_eq!(
            decode_remote_args(&encode_remote_args(&args)).unwrap(),
            args
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
