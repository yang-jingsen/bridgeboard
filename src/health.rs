use crate::config::HealthExpectConfig;
use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;
use url::Url;

pub fn check_http(url: &str, timeout: Duration) -> Result<String> {
    check_http_with_expect(url, timeout, &HealthExpectConfig::default())
}

pub fn check_http_with_expect(
    url: &str,
    timeout: Duration,
    expect: &HealthExpectConfig,
) -> Result<String> {
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
    let body = buf
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or("");
    for expected in &expect.body_contains {
        if !body.contains(expected) {
            bail!(
                "health body expectation failed for {url}: missing `{}`",
                expected
            );
        }
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    fn serve_once(body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 512];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        format!("http://{addr}/health")
    }

    #[test]
    fn health_expect_body_contains_passes() {
        let url = serve_once(r#"{"version": 3}"#);
        let expect = HealthExpectConfig {
            body_contains: vec![r#""version": 3"#.into()],
        };
        let status = check_http_with_expect(&url, Duration::from_secs(1), &expect).unwrap();
        assert!(status.contains("200"));
    }

    #[test]
    fn health_expect_body_contains_fails() {
        let url = serve_once(r#"{"version": 2}"#);
        let expect = HealthExpectConfig {
            body_contains: vec![r#""version": 3"#.into()],
        };
        let err = check_http_with_expect(&url, Duration::from_secs(1), &expect)
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing"));
    }
}
