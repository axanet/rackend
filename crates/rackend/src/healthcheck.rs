//! `rackend healthcheck`: exit 0 iff `GET /api/health` returns 200 (Docker HEALTHCHECK).
//! Uses a raw HTTP/1.1 request on std I/O so it needs no HTTP client dependency.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::Duration;

/// Checks the server listening on `addr` (the unspecified address means localhost).
pub fn check(addr: SocketAddr) -> Result<(), String> {
    let target = match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), addr.port())
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), addr.port())
        }
        _ => addr,
    };
    let timeout = Duration::from_secs(2);
    let mut stream = TcpStream::connect_timeout(&target, timeout)
        .map_err(|e| format!("connect {target}: {e}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    stream
        .write_all(b"GET /api/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .map_err(|e| format!("write: {e}"))?;
    let mut head = [0u8; 64];
    let n = stream.read(&mut head).map_err(|e| format!("read: {e}"))?;
    let line = String::from_utf8_lossy(&head[..n]);
    let status = line.split_whitespace().nth(1).unwrap_or_default();
    if status == "200" {
        Ok(())
    } else {
        Err(format!("unhealthy: status {status:?}"))
    }
}
