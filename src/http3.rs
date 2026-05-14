use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Buf;
use quinn::crypto::rustls::QuicClientConfig;
use rustls::ClientConfig;
use webpki_roots::TLS_SERVER_ROOTS;

use crate::config::{ProxyConfig, TestTarget};
use crate::error::{ProxyPenError, Result};
use crate::result::{Protocol, TestResult, TestStatus, Timing};
use crate::socks::associate;
use crate::udp_socket::SocksUdpSocket;

/// Test HTTP/3 through a SOCKS5 proxy (via UDP ASSOCIATE).
pub async fn test(config: &ProxyConfig, target: &TestTarget, timeout: Duration) -> TestResult {
    match tokio::time::timeout(timeout, do_test(config, target)).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => TestResult {
            protocol: Protocol::Http3,
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
            protocol: Protocol::Http3,
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

    // Establish SOCKS5 UDP association
    let assoc = associate::associate(config).await?;
    let socks_handshake = start.elapsed();

    // Create the SOCKS5 UDP socket wrapper for quinn
    let socks_addr = target.to_socks_addr();

    // Determine the target SocketAddr for QUIC connection.
    // For domain targets, we need a resolved addr for quinn's connect().
    // The actual routing goes through SOCKS5, so we use the relay addr as a placeholder
    // if target is a domain.
    let target_socket_addr = resolve_target_addr(target, assoc.relay_addr)?;

    let udp_socket = SocksUdpSocket::new(assoc.socket, socks_addr.clone(), target_socket_addr);

    // Configure QUIC/TLS with h3 ALPN
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(TLS_SERVER_ROOTS.iter().cloned());

    let mut tls_config = ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    tls_config.alpn_protocols = vec![b"h3".to_vec()];

    let quic_config: QuicClientConfig = tls_config
        .try_into()
        .map_err(|e| ProxyPenError::Quic(format!("QUIC config: {e}")))?;
    let client_config = quinn::ClientConfig::new(Arc::new(quic_config));

    // Create quinn endpoint with our custom socket
    let runtime = quinn::default_runtime()
        .ok_or_else(|| ProxyPenError::Quic("no async runtime".into()))?;
    let endpoint = quinn::Endpoint::new_with_abstract_socket(
        quinn::EndpointConfig::default(),
        None,
        Arc::new(udp_socket),
        runtime,
    )
    .map_err(|e| ProxyPenError::Quic(format!("endpoint: {e}")))?;

    // QUIC connect
    let quic_start = Instant::now();
    let conn = endpoint
        .connect_with(client_config, target_socket_addr, &target.host)
        .map_err(|e| ProxyPenError::Quic(format!("connect: {e}")))?
        .await?;
    let quic_handshake = quic_start.elapsed();

    // HTTP/3 over QUIC
    let h3_conn = h3_quinn::Connection::new(conn);
    let (mut driver, mut sender) = h3::client::new(h3_conn)
        .await
        .map_err(|e| ProxyPenError::Http(format!("h3 handshake: {e}")))?;

    // Build HTTP/3 request
    let path = if target.path.is_empty() {
        "/"
    } else {
        &target.path
    };
    let req = http::Request::builder()
        .method("GET")
        .uri(format!(
            "https://{}{}",
            target.authority(),
            path
        ))
        .header("user-agent", "proxypen/0.1")
        .body(())
        .map_err(|e| ProxyPenError::Http(format!("request build: {e}")))?;

    // The h3 driver must be polled concurrently for the connection to process
    // incoming data (QPACK streams, flow control, data routing to request streams).
    // Use tokio::select! so we return as soon as the request completes.
    let drive_fut = async {
        std::future::poll_fn(|cx| driver.poll_close(cx)).await
    };

    let request_start = start;
    let request_fut = async move {
        let mut stream = sender
            .send_request(req)
            .await
            .map_err(|e| ProxyPenError::Http(format!("send request: {e}")))?;

        // Signal end of request body (required by h3 — server won't respond without FIN)
        stream
            .finish()
            .await
            .map_err(|e| ProxyPenError::Http(format!("finish request: {e}")))?;

        let response = stream
            .recv_response()
            .await
            .map_err(|e| ProxyPenError::Http(format!("recv response: {e}")))?;

        let first_byte = request_start.elapsed();
        let http_status = response.status().as_u16();

        // Read body
        let mut total_size = 0usize;
        while let Some(chunk) = stream
            .recv_data()
            .await
            .map_err(|e| ProxyPenError::Http(format!("recv data: {e}")))?
        {
            total_size += chunk.remaining();
        }

        let total = request_start.elapsed();

        Ok::<(u16, usize, Duration, Duration), ProxyPenError>((
            http_status, total_size, first_byte, total,
        ))
    };

    // Run both concurrently — select returns when either completes
    let request_result = tokio::select! {
        result = request_fut => result,
        _ = drive_fut => Err(ProxyPenError::Http("connection closed unexpectedly".into())),
    };
    let (http_status, total_size, first_byte, total) = request_result?;

    // Keep control stream alive until we're done
    drop(assoc.control);

    Ok(TestResult {
        protocol: Protocol::Http3,
        status: TestStatus::Success,
        http_status: Some(http_status),
        timing: Timing {
            socks_handshake,
            tls_handshake: Some(quic_handshake),
            first_byte,
            total,
        },
        response_size: Some(total_size),
    })
}

/// Resolve target to a SocketAddr for quinn's connect().
/// For IP targets, just use the IP:port directly.
/// For domain targets, use the relay addr (SOCKS5 handles actual routing).
fn resolve_target_addr(target: &TestTarget, relay_addr: SocketAddr) -> Result<SocketAddr> {
    if let Some(ip) = target.resolved_addr {
        Ok(SocketAddr::new(ip, target.port))
    } else if let Ok(ip) = target.host.parse::<IpAddr>() {
        Ok(SocketAddr::new(ip, target.port))
    } else {
        Ok(relay_addr)
    }
}
