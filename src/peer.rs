use crate::config::AppConfig;
use crate::registry::RegistryExport;
use anyhow::Result;
#[cfg(windows)]
use std::fs::{self, File};
#[cfg(not(windows))]
use std::io::Read;
#[cfg(windows)]
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
#[cfg(windows)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
static CAPTURE_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    peer_command_output_legacy(
        ssh_alias,
        bridgeboard_bin,
        &["registry", "export", "--json"],
        Duration::from_secs(30),
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
        .args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5"])
        .arg(ssh_alias)
        .arg(remote_command_line(bridgeboard_bin, args));
    output_with_timeout(command, timeout)
}

#[cfg(not(windows))]
fn peer_registry_output(ssh_alias: &str, bridgeboard_bin: &str) -> Result<Output, String> {
    peer_command_output_legacy(
        ssh_alias,
        bridgeboard_bin,
        &["registry", "export", "--json"],
        Duration::from_secs(30),
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
        .args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5"])
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

fn legacy_remote_command_line(bridgeboard_bin: &str, args: &[&str]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(remote_arg_quote(bridgeboard_bin));
    parts.extend(args.iter().map(|arg| remote_arg_quote(arg)));
    parts.join(" ")
}

fn peer_command_output_legacy(
    ssh_alias: &str,
    bridgeboard_bin: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<Output, String> {
    let mut command = crate::command::quiet_command("ssh");
    command
        .args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5"])
        .arg(ssh_alias)
        .arg(legacy_remote_command_line(bridgeboard_bin, args));
    output_with_timeout(command, timeout)
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

#[cfg(windows)]
fn output_with_timeout(mut command: Command, timeout: Duration) -> Result<Output, String> {
    // Windows OpenSSH can hang when stdout/stderr are captured as Rust pipes.
    let stdout_path = capture_path("stdout");
    let stderr_path = capture_path("stderr");
    let stdout_file = File::create(&stdout_path).map_err(|e| e.to_string())?;
    let stderr_file = File::create(&stderr_path).map_err(|e| e.to_string())?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let stderr = read_capture(&stderr_path).unwrap_or_default();
            cleanup_capture(&stdout_path);
            cleanup_capture(&stderr_path);
            let detail = String::from_utf8_lossy(&stderr);
            let detail = detail.trim();
            let suffix = if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            };
            return Err(format!(
                "peer registry query timed out after {}s{}",
                timeout.as_secs(),
                suffix
            ));
        }
        thread::sleep(Duration::from_millis(50));
    };
    let stdout = read_capture(&stdout_path)?;
    let stderr = read_capture(&stderr_path)?;
    cleanup_capture(&stdout_path);
    cleanup_capture(&stderr_path);
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(windows)]
fn capture_path(label: &str) -> PathBuf {
    let count = CAPTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "bridgeboard-peer-{}-{count}-{label}.tmp",
        std::process::id()
    ));
    path
}

#[cfg(windows)]
fn read_capture(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| format!("failed to read {}: {e}", path.display()))
}

#[cfg(windows)]
fn cleanup_capture(path: &Path) {
    let _ = fs::remove_file(path);
}

#[cfg(not(windows))]
fn output_with_timeout(mut command: Command, timeout: Duration) -> Result<Output, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture peer command stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "failed to capture peer command stderr".to_string())?;
    let stdout_reader = thread::spawn(move || read_stream(stdout));
    let stderr_reader = thread::spawn(move || read_stream(stderr));

    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let stderr = join_reader(stderr_reader, "stderr").unwrap_or_default();
            let detail = String::from_utf8_lossy(&stderr);
            let detail = detail.trim();
            let suffix = if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            };
            let _ = join_reader(stdout_reader, "stdout");
            return Err(format!(
                "peer registry query timed out after {}s{}",
                timeout.as_secs(),
                suffix
            ));
        }
        thread::sleep(Duration::from_millis(50));
    };
    Ok(Output {
        status,
        stdout: join_reader(stdout_reader, "stdout")?,
        stderr: join_reader(stderr_reader, "stderr")?,
    })
}

#[cfg(not(windows))]
fn read_stream(mut stream: impl Read) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    stream.read_to_end(&mut output).map_err(|e| e.to_string())?;
    Ok(output)
}

#[cfg(not(windows))]
fn join_reader(
    handle: thread::JoinHandle<Result<Vec<u8>, String>>,
    name: &str,
) -> Result<Vec<u8>, String> {
    handle
        .join()
        .map_err(|_| format!("peer command {name} reader panicked"))?
}

#[cfg(test)]
mod tests {
    use super::{
        decode_remote_args, encode_remote_args, legacy_remote_command_line, remote_command_line,
    };

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

    #[test]
    fn legacy_remote_command_quotes_args() {
        assert_eq!(
            legacy_remote_command_line("bridgeboard", &["registry", "export", "--json"]),
            "\"bridgeboard\" \"registry\" \"export\" \"--json\""
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
