use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;
use url::Url;

pub fn check_http(url: &str, timeout: Duration) -> Result<String> {
    let parsed = Url::parse(url).with_context(|| format!("parse health_url {url}"))?;
    if parsed.scheme() != "http" {
        bail!("only http health URLs are supported in v1: {url}");
    }
    let host = parsed.host_str().context("health_url missing host")?;
    let port = parsed.port_or_known_default().unwrap_or(80);
    let addr = format!("{host}:{port}");
    let addr = addr
        .to_socket_addrs()?
        .next()
        .with_context(|| format!("resolve health_url host {host}"))?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let path = if parsed.path().is_empty() {
        "/"
    } else {
        parsed.path()
    };
    let query = parsed.query().map(|q| format!("?{q}")).unwrap_or_default();
    let request = format!(
        "GET {path}{query} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nUser-Agent: bridgeboard/0.1\r\n\r\n"
    );
    stream.write_all(request.as_bytes())?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf)?;
    let status = buf
        .lines()
        .next()
        .unwrap_or("HTTP/1.1 000 unknown")
        .to_string();
    if !status.contains(" 2") && !status.contains(" 3") {
        bail!("health check failed: {status}");
    }
    Ok(status)
}
