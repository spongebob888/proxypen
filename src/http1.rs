use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::TlsConnector;

use crate::config::{ProxyConfig, TestTarget};
use crate::error::{ProxyPenError, Result};
use crate::result::{Protocol, TestResult, TestStatus, Timing};
use crate::socks::connector;
use crate::tls::make_tls_config;

/// Test HTTP/1.1 through a SOCKS5 proxy.
pub async fn test(config: &ProxyConfig, target: &TestTarget, timeout: Duration) -> TestResult {
    match tokio::time::timeout(timeout, do_test(config, target)).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => TestResult {
            protocol: Protocol::Http1,
            status: TestStatus::Failed(e.to_string()),
            http_status: None,
            timing: Timing {
                socks_handshake: Duration::ZERO,
                tls_handshake: None,
                first_byte: Duration::ZERO,
                total: Duration::ZERO,
            },
            response_size: None,
        },
        Err(_) => TestResult {
            protocol: Protocol::Http1,
            status: TestStatus::Failed("timeout".into()),
            http_status: None,
            timing: Timing {
                socks_handshake: Duration::ZERO,
                tls_handshake: None,
                first_byte: Duration::ZERO,
                total: Duration::ZERO,
            },
            response_size: None,
        },
    }
}

async fn do_test(config: &ProxyConfig, target: &TestTarget) -> Result<TestResult> {
    let start = Instant::now();

    // SOCKS5 TCP CONNECT
    let stream = connector::connect(config, target).await?;
    let socks_handshake = start.elapsed();

    let mut tls_handshake_duration = None;

    // Build request
    let path = if target.path.is_empty() {
        "/"
    } else {
        &target.path
    };
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: proxypen/0.1\r\n\r\n",
        path,
        target.authority()
    );

    let (http_status, response_size, first_byte) = if target.use_tls {
        // TLS handshake
        let tls_start = Instant::now();
        let tls_config = make_tls_config(None)?;
        let connector = TlsConnector::from(Arc::new(tls_config));
        let server_name = ServerName::try_from(target.host.clone())
            .map_err(|e| ProxyPenError::Tls(format!("invalid server name: {e}")))?;
        let mut tls_stream = connector.connect(server_name, stream).await
            .map_err(|e| ProxyPenError::Tls(e.to_string()))?;
        tls_handshake_duration = Some(tls_start.elapsed());

        // Send request
        tls_stream.write_all(request.as_bytes()).await?;

        // Read response
        let mut buf = Vec::with_capacity(8192);
        let mut tmp = [0u8; 4096];
        let n = tls_stream.read(&mut tmp).await?;
        let first_byte_time = start.elapsed();
        buf.extend_from_slice(&tmp[..n]);

        // Read remaining (treat UnexpectedEof as normal close — common with TLS)
        loop {
            match tls_stream.read(&mut tmp).await {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
        }

        let (status, _) = parse_status_line(&buf)?;
        (status, buf.len(), first_byte_time)
    } else {
        // Plain HTTP
        let mut stream = stream;
        stream.write_all(request.as_bytes()).await?;

        let mut buf = Vec::with_capacity(8192);
        let mut tmp = [0u8; 4096];
        let n = stream.read(&mut tmp).await?;
        let first_byte_time = start.elapsed();
        buf.extend_from_slice(&tmp[..n]);

        loop {
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }

        let (status, _) = parse_status_line(&buf)?;
        (status, buf.len(), first_byte_time)
    };

    let total = start.elapsed();

    Ok(TestResult {
        protocol: Protocol::Http1,
        status: TestStatus::Success,
        http_status: Some(http_status),
        timing: Timing {
            socks_handshake,
            tls_handshake: tls_handshake_duration,
            first_byte,
            total,
        },
        response_size: Some(response_size),
    })
}

/// Parse the HTTP status line from a response buffer.
/// Returns (status_code, header_end_offset).
fn parse_status_line(buf: &[u8]) -> Result<(u16, usize)> {
    let response = String::from_utf8_lossy(buf);
    let first_line = response
        .lines()
        .next()
        .ok_or_else(|| ProxyPenError::Http("empty response".into()))?;

    // "HTTP/1.1 200 OK"
    let parts: Vec<&str> = first_line.splitn(3, ' ').collect();
    if parts.len() < 2 {
        return Err(ProxyPenError::Http(format!(
            "malformed status line: {first_line}"
        )));
    }

    let status: u16 = parts[1]
        .parse()
        .map_err(|_| ProxyPenError::Http(format!("invalid status code: {}", parts[1])))?;

    let header_end = response
        .find("\r\n\r\n")
        .map(|i| i + 4)
        .unwrap_or(buf.len());

    Ok((status, header_end))
}
