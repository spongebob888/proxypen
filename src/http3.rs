use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Buf;
use quinn::crypto::rustls::QuicClientConfig;
use rustls::ClientConfig;
use webpki_roots::TLS_SERVER_ROOTS;

use crate::config::{ProxyConfig, TestTarget};
use crate::direct::{self, DirectConfig};
use crate::error::{ProxyPenError, Result};
use crate::result::{Protocol, TestResult, TestStatus, Timing};
use crate::socks::associate;
use crate::transport::Transport;
use crate::udp_socket::SocksUdpSocket;

/// Test HTTP/3 over the supplied transport (QUIC).
pub async fn test(transport: &Transport, target: &TestTarget, timeout: Duration) -> TestResult {
    match tokio::time::timeout(timeout, do_test(transport, target)).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => TestResult {
            protocol: Protocol::Http3,
            status: TestStatus::Failed(e.to_string()),
            http_status: None,
            timing: empty_timing(),
            response_size: None,
        },
        Err(_) => TestResult {
            protocol: Protocol::Http3,
            status: TestStatus::Failed("timeout".into()),
            http_status: None,
            timing: empty_timing(),
            response_size: None,
        },
    }
}

fn empty_timing() -> Timing {
    Timing {
        socks_handshake: None,
        tcp_connect: None,
        tls_handshake: None,
        first_byte: Duration::ZERO,
        total: Duration::ZERO,
    }
}

async fn do_test(transport: &Transport, target: &TestTarget) -> Result<TestResult> {
    let start = Instant::now();

    // Build the quinn::Endpoint and pick the SocketAddr to connect to.
    // The two transports differ in how the UDP socket is constructed:
    //   - SOCKS5: UDP ASSOCIATE on the proxy + a SOCKS5-wrapped socket.
    //   - Direct: a plain bound UDP socket.
    let (endpoint, target_socket_addr, control) = match transport {
        Transport::Socks5(cfg) => build_socks_endpoint(cfg, target).await?,
        Transport::Direct(cfg) => build_direct_endpoint(cfg, target).await?,
    };
    // Only meaningful in SOCKS5 mode (UDP ASSOCIATE handshake).
    // In direct mode there is no underlying connect step (UDP is connectionless).
    let socks_handshake = if transport.is_direct() {
        None
    } else {
        Some(start.elapsed())
    };

    let client_config = make_quic_client_config()?;

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
        .uri(format!("https://{}{}", target.authority(), path))
        .header("user-agent", "proxypen/0.1")
        .body(())
        .map_err(|e| ProxyPenError::Http(format!("request build: {e}")))?;

    // The h3 driver must be polled concurrently for the connection to process
    // incoming data (QPACK streams, flow control, data routing to request streams).
    let drive_fut = async { std::future::poll_fn(|cx| driver.poll_close(cx)).await };

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

    let request_result = tokio::select! {
        result = request_fut => result,
        _ = drive_fut => Err(ProxyPenError::Http("connection closed unexpectedly".into())),
    };
    let (http_status, total_size, first_byte, total) = request_result?;

    // Keep the SOCKS5 control stream alive until the request is done.
    drop(control);

    Ok(TestResult {
        protocol: Protocol::Http3,
        status: TestStatus::Success,
        http_status: Some(http_status),
        timing: Timing {
            socks_handshake,
            tcp_connect: None,
            tls_handshake: Some(quic_handshake),
            first_byte,
            total,
        },
        response_size: Some(total_size),
    })
}

/// SOCKS5 path: UDP ASSOCIATE → wrap UDP socket → quinn endpoint with abstract socket.
async fn build_socks_endpoint(
    config: &ProxyConfig,
    target: &TestTarget,
) -> Result<(quinn::Endpoint, SocketAddr, Option<tokio::net::TcpStream>)> {
    let assoc = associate::associate(config).await?;
    let socks_addr = target.to_socks_addr();
    let target_socket_addr = resolve_target_for_socks(target, assoc.relay_addr)?;

    let udp_socket = SocksUdpSocket::new(assoc.socket, socks_addr, target_socket_addr);

    let runtime = quinn::default_runtime()
        .ok_or_else(|| ProxyPenError::Quic("no async runtime".into()))?;
    let endpoint = quinn::Endpoint::new_with_abstract_socket(
        quinn::EndpointConfig::default(),
        None,
        Arc::new(udp_socket),
        runtime,
    )
    .map_err(|e| ProxyPenError::Quic(format!("endpoint: {e}")))?;

    Ok((endpoint, target_socket_addr, Some(assoc.control)))
}

/// Direct path: bind a UDP socket (optionally on a specific interface) and
/// hand it to quinn::Endpoint::new.
async fn build_direct_endpoint(
    config: &DirectConfig,
    target: &TestTarget,
) -> Result<(quinn::Endpoint, SocketAddr, Option<tokio::net::TcpStream>)> {
    let target_addr = direct::resolve_target(target).await?;
    let std_socket = direct::build_udp_socket(target_addr.is_ipv6(), config.interface.as_ref())?;

    let runtime = quinn::default_runtime()
        .ok_or_else(|| ProxyPenError::Quic("no async runtime".into()))?;
    let endpoint = quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        None,
        std_socket,
        runtime,
    )
    .map_err(|e| ProxyPenError::Quic(format!("endpoint: {e}")))?;

    Ok((endpoint, target_addr, None))
}

fn make_quic_client_config() -> Result<quinn::ClientConfig> {
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(TLS_SERVER_ROOTS.iter().cloned());

    let mut tls_config = ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    tls_config.alpn_protocols = vec![b"h3".to_vec()];

    let quic_config: QuicClientConfig = tls_config
        .try_into()
        .map_err(|e| ProxyPenError::Quic(format!("QUIC config: {e}")))?;
    Ok(quinn::ClientConfig::new(Arc::new(quic_config)))
}

/// Resolve target to a SocketAddr for quinn's connect() in SOCKS5 mode.
/// For domain targets without a resolved IP, use the relay addr (SOCKS5
/// handles the actual routing).
fn resolve_target_for_socks(target: &TestTarget, relay_addr: SocketAddr) -> Result<SocketAddr> {
    if let Some(ip) = target.resolved_addr {
        Ok(SocketAddr::new(ip, target.port))
    } else if let Ok(ip) = target.host.parse::<IpAddr>() {
        Ok(SocketAddr::new(ip, target.port))
    } else {
        Ok(relay_addr)
    }
}
