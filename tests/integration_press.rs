// Integration tests: press (latency retest) through SOCKS5 proxy and direct.
//
// Uses shadowquic as an in-process SOCKS5 server and the built-in HTTP test
// server for HTTP/2 + HTTP/3 targets.

mod common;

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use proxypen::press::{self, PressOptions, PressProtocol};
use proxypen::{HttpServerConfig, ProxyConfig, ProxyPen, TestStatus, TestTarget, Transport};
use tokio::net::TcpListener;

use common::{install_rustls, start_http1_echo, start_socks5_server};

/// Helper: bind a TCP listener on an ephemeral port, remember it, drop it,
/// return the port. There is a tiny TOCTOU window — fine in tests.
async fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    // Let the kernel release the port before we reuse it.
    tokio::time::sleep(Duration::from_millis(50)).await;
    p
}

/// Guard that aborts the spawned HTTP server task on drop.
struct HttpServerGuard {
    #[allow(dead_code)]
    http1_addr: SocketAddr,
    http2_addr: SocketAddr,
    http3_addr: SocketAddr,
    _abort: tokio::task::JoinHandle<()>,
}

/// Start the full HTTP/1 + HTTP/2 + HTTP/3 test server on ephemeral ports.
async fn start_http_test_server() -> HttpServerGuard {
    install_rustls();

    let h1 = free_port().await;
    let h2 = free_port().await;
    let h3 = free_port().await;

    let config = HttpServerConfig {
        bind: IpAddr::from([127, 0, 0, 1]),
        http1_port: h1,
        http2_port: h2,
        http3_port: h3,
        response_size: 64,
    };

    let handle = tokio::spawn(async move {
        let _ = proxypen::run_http_server(config).await;
    });

    // Give the TLS/QUIC servers time to initialise (cert generation + bind).
    tokio::time::sleep(Duration::from_millis(800)).await;

    let bind = IpAddr::from([127, 0, 0, 1]);
    HttpServerGuard {
        http1_addr: SocketAddr::new(bind, h1),
        http2_addr: SocketAddr::new(bind, h2),
        http3_addr: SocketAddr::new(bind, h3),
        _abort: handle,
    }
}

impl Drop for HttpServerGuard {
    fn drop(&mut self) {
        self._abort.abort();
    }
}

/// Build a TestTarget from a SocketAddr.
fn target_for(addr: SocketAddr, use_tls: bool, insecure: bool) -> TestTarget {
    TestTarget {
        host: addr.ip().to_string(),
        port: addr.port(),
        path: "/".into(),
        use_tls,
        resolved_addr: Some(addr.ip()),
        danger_accept_invalid_certs: insecure,
    }
}

fn socks_transport(addr: SocketAddr) -> Transport {
    Transport::Socks5(ProxyConfig {
        addr: addr.to_string(),
        auth: None,
    })
}

fn direct_transport() -> Transport {
    Transport::Direct(proxypen::DirectConfig::new(None))
}

// ---------------------------------------------------------------
// HTTP/1 press through SOCKS5
// ---------------------------------------------------------------

#[tokio::test]
async fn press_http1_through_socks5() {
    install_rustls();
    let proxy = start_socks5_server().await;
    let upstream = start_http1_echo("press-http1-via-socks").await;

    let opts = PressOptions {
        transport: socks_transport(proxy.addr),
        target: target_for(upstream, false, false),
        protocol: PressProtocol::Http1,
        concurrency: 5,
        num_requests: 20,
        timeout: Duration::from_secs(10),
    };

    let result = press::press(&opts).await;
    assert_eq!(result.success_count, 20, "all HTTP/1 via SOCKS5 should succeed");
    assert_eq!(result.error_count, 0);
    assert!(result.ttfb.sample_count >= 19, "should have TTFB samples");
    assert!(result.total_time.sample_count >= 19);
    assert!(result.ttfb.avg_ms > 0.0, "TTFB should be positive");
    assert!(result.requests_per_second > 0.0, "RPS should be positive");
}

// ---------------------------------------------------------------
// HTTP/1 press direct (baseline)
// ---------------------------------------------------------------

#[tokio::test]
async fn press_http1_direct() {
    install_rustls();
    let upstream = start_http1_echo("press-http1-direct").await;

    let opts = PressOptions {
        transport: direct_transport(),
        target: target_for(upstream, false, false),
        protocol: PressProtocol::Http1,
        concurrency: 5,
        num_requests: 20,
        timeout: Duration::from_secs(10),
    };

    let result = press::press(&opts).await;
    assert_eq!(result.success_count, 20);
    assert_eq!(result.error_count, 0);
    assert!(result.ttfb.avg_ms > 0.0);
}

// ---------------------------------------------------------------
// HTTP/2 press through SOCKS5
// ---------------------------------------------------------------

#[tokio::test]
async fn press_http2_through_socks5() {
    install_rustls();
    let server = start_http_test_server().await;
    let proxy = start_socks5_server().await;

    let opts = PressOptions {
        transport: socks_transport(proxy.addr),
        target: target_for(server.http2_addr, true, true),
        protocol: PressProtocol::Http2,
        concurrency: 3,
        num_requests: 9,
        timeout: Duration::from_secs(15),
    };

    let result = press::press(&opts).await;
    assert_eq!(
        result.success_count, 9,
        "all HTTP/2 via SOCKS5 should succeed, errors: {:?}",
        result.errors
    );
    assert!(result.ttfb.sample_count >= 8);
    assert!(result.ttfb.avg_ms > 0.0);
}

// ---------------------------------------------------------------
// HTTP/2 press direct
// ---------------------------------------------------------------

#[tokio::test]
async fn press_http2_direct() {
    install_rustls();
    let server = start_http_test_server().await;

    let opts = PressOptions {
        transport: direct_transport(),
        target: target_for(server.http2_addr, true, true),
        protocol: PressProtocol::Http2,
        concurrency: 3,
        num_requests: 9,
        timeout: Duration::from_secs(15),
    };

    let result = press::press(&opts).await;
    assert_eq!(
        result.success_count, 9,
        "all HTTP/2 direct should succeed, errors: {:?}",
        result.errors
    );
    assert!(result.ttfb.avg_ms > 0.0);
}

// ---------------------------------------------------------------
// HTTP/3 press through SOCKS5
// ---------------------------------------------------------------

#[tokio::test]
async fn press_http3_through_socks5() {
    install_rustls();
    let server = start_http_test_server().await;
    let proxy = start_socks5_server().await;

    let opts = PressOptions {
        transport: socks_transport(proxy.addr),
        target: target_for(server.http3_addr, true, true),
        protocol: PressProtocol::Http3,
        concurrency: 2,
        num_requests: 4,
        timeout: Duration::from_secs(20),
    };

    let result = press::press(&opts).await;
    assert_eq!(
        result.success_count, 4,
        "all HTTP/3 via SOCKS5 should succeed, errors: {:?}",
        result.errors
    );
    assert!(result.ttfb.sample_count >= 3);
    assert!(result.ttfb.avg_ms > 0.0);
    // SOCKS5 mode should NOT have a tcp_connect (the timing module reports
    // socks_handshake instead). We already test that in the proxy-agnostic
    // test below via ProxyPen.
    assert!(!result.errors.iter().any(|e| e.contains("timeout")), "unexpected timeout");
}

// ---------------------------------------------------------------
// HTTP/3 press direct
// ---------------------------------------------------------------

#[tokio::test]
async fn press_http3_direct() {
    install_rustls();
    let server = start_http_test_server().await;

    let opts = PressOptions {
        transport: direct_transport(),
        target: target_for(server.http3_addr, true, true),
        protocol: PressProtocol::Http3,
        concurrency: 2,
        num_requests: 4,
        timeout: Duration::from_secs(20),
    };

    let result = press::press(&opts).await;
    assert_eq!(
        result.success_count, 4,
        "all HTTP/3 direct should succeed, errors: {:?}",
        result.errors
    );
    assert!(result.ttfb.avg_ms > 0.0);
}

// ---------------------------------------------------------------
// Verify ProxyPen under SOCKS5 still reports socks handshake timing
// ---------------------------------------------------------------

#[tokio::test]
async fn proxy_pen_socks5_timing_present() {
    install_rustls();
    let proxy = start_socks5_server().await;
    let upstream = start_http1_echo("timing-check").await;

    let pen = ProxyPen::new(socks_transport(proxy.addr));
    let target = target_for(upstream, false, false);
    let result = pen.test_http1(&target, Duration::from_secs(5)).await;

    assert_eq!(result.http_status, Some(200));
    assert!(
        matches!(result.status, TestStatus::Success),
        "expected success, got {:?}",
        result.status
    );
    assert!(result.timing.socks_handshake.is_some(), "SOCKS handshake should be timed");
}

// ---------------------------------------------------------------
// Stress: mix of successes and failures (unreachable port)
// ---------------------------------------------------------------

#[tokio::test]
async fn press_mixed_success_and_failure() {
    install_rustls();
    let upstream = start_http1_echo("mixed").await;

    // 10 requests to a good server, 5 to a port that is almost certainly closed.
    let bad_port = free_port().await; // nothing listening here

    let opts_good = PressOptions {
        transport: direct_transport(),
        target: target_for(upstream, false, false),
        protocol: PressProtocol::Http1,
        concurrency: 3,
        num_requests: 10,
        timeout: Duration::from_secs(5),
    };
    let good = press::press(&opts_good).await;
    assert_eq!(good.success_count, 10);
    assert_eq!(good.error_count, 0);

    let bad_addr = SocketAddr::new(IpAddr::from([127, 0, 0, 1]), bad_port);
    let opts_bad = PressOptions {
        transport: direct_transport(),
        target: target_for(bad_addr, false, false),
        protocol: PressProtocol::Http1,
        concurrency: 3,
        num_requests: 5,
        timeout: Duration::from_secs(5),
    };
    let bad = press::press(&opts_bad).await;
    assert_eq!(bad.success_count, 0, "should have no successes against closed port");
    assert!(
        bad.error_count >= 1,
        "should have errors against closed port, got {} errors",
        bad.error_count
    );
}
