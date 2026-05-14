use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use h2::client;
use http::Request;
use rustls::pki_types::ServerName;
use tokio_rustls::TlsConnector;

use crate::config::{ProxyConfig, TestTarget};
use crate::error::{ProxyPenError, Result};
use crate::result::{Protocol, TestResult, TestStatus, Timing};
use crate::socks::connector;
use crate::tls::make_tls_config;

/// Test HTTP/2 through a SOCKS5 proxy.
pub async fn test(config: &ProxyConfig, target: &TestTarget, timeout: Duration) -> TestResult {
    match tokio::time::timeout(timeout, do_test(config, target)).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => TestResult {
            protocol: Protocol::Http2,
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
            protocol: Protocol::Http2,
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
    if !target.use_tls {
        return Err(ProxyPenError::InvalidConfig(
            "HTTP/2 requires TLS (use https:// target)".into(),
        ));
    }

    let start = Instant::now();

    // SOCKS5 TCP CONNECT
    let stream = connector::connect(config, target).await?;
    let socks_handshake = start.elapsed();

    // TLS handshake with h2 ALPN
    let tls_start = Instant::now();
    let tls_config = make_tls_config(Some(vec![b"h2".to_vec()]))?;
    let connector = TlsConnector::from(Arc::new(tls_config));
    let server_name = ServerName::try_from(target.host.clone())
        .map_err(|e| ProxyPenError::Tls(format!("invalid server name: {e}")))?;
    let tls_stream = connector
        .connect(server_name, stream)
        .await
        .map_err(|e| ProxyPenError::Tls(e.to_string()))?;
    let tls_handshake = tls_start.elapsed();

    // HTTP/2 handshake
    let (mut sender, conn) = client::handshake(tls_stream).await?;

    // Spawn connection driver
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            tracing::error!("h2 connection error: {e}");
        }
    });

    // Build and send request
    let path = if target.path.is_empty() {
        "/"
    } else {
        &target.path
    };
    let uri = format!("https://{}{}", target.authority(), path);
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header("user-agent", "proxypen/0.1")
        .body(())
        .map_err(|e| ProxyPenError::Http(format!("request build: {e}")))?;

    let (response_fut, _send_stream) = sender.send_request(req, true)?;
    let response = response_fut.await?;
    let first_byte = start.elapsed();

    let http_status = response.status().as_u16();

    // Read body
    let mut body = response.into_body();
    let mut total_size = 0usize;
    while let Some(chunk) = body.data().await {
        let chunk: Bytes = chunk?;
        total_size += chunk.len();
        body.flow_control().release_capacity(chunk.len())?;
    }

    let total = start.elapsed();

    Ok(TestResult {
        protocol: Protocol::Http2,
        status: TestStatus::Success,
        http_status: Some(http_status),
        timing: Timing {
            socks_handshake,
            tls_handshake: Some(tls_handshake),
            first_byte,
            total,
        },
        response_size: Some(total_size),
    })
}
