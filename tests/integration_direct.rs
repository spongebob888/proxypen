// Direct-mode integration tests: HTTP/1 + bench TCP/UDP without a proxy.

mod common;

use std::time::Duration;

use proxypen::bench::{
    BenchDirection, BenchMode, BenchOptions, run_client_quiet,
};
use proxypen::direct::parse_interface;
use proxypen::{
    DirectConfig, InterfaceSpec, ProxyPen, TestStatus, TestTarget, Transport,
};

use common::{install_rustls, start_http1_echo};

fn direct_transport() -> Transport {
    Transport::Direct(DirectConfig::new(None))
}

fn target_for(addr: std::net::SocketAddr) -> TestTarget {
    TestTarget {
        host: addr.ip().to_string(),
        port: addr.port(),
        path: "/".into(),
        use_tls: false,
        resolved_addr: Some(addr.ip()),
    }
}

#[tokio::test]
async fn http1_direct_against_local_server() {
    install_rustls();
    let server_addr = start_http1_echo("hello world").await;

    let pen = ProxyPen::new(direct_transport());
    let target = target_for(server_addr);
    let result = pen.test_http1(&target, Duration::from_secs(5)).await;

    assert!(
        matches!(result.status, TestStatus::Success),
        "expected success, got {:?}",
        result.status
    );
    assert_eq!(result.http_status, Some(200));
    let timing = &result.timing;
    assert!(timing.tcp_connect.is_some(), "direct mode should record tcp_connect");
    assert!(timing.socks_handshake.is_none(), "direct mode should not record socks_handshake");
    assert!(result.response_size.unwrap_or(0) > 0);
}

#[tokio::test]
async fn http1_direct_with_source_ip_interface() {
    // Source-IP binding (loopback). Proves the interface flag is wired
    // through Transport::Direct → direct::connect_tcp end-to-end.
    install_rustls();
    let server_addr = start_http1_echo("ok").await;

    let iface = parse_interface("127.0.0.1");
    assert!(matches!(iface, InterfaceSpec::Address(_)));

    let transport = Transport::Direct(DirectConfig::new(Some(iface)));
    let pen = ProxyPen::new(transport);
    let target = target_for(server_addr);
    let result = pen.test_http1(&target, Duration::from_secs(5)).await;

    assert!(
        matches!(result.status, TestStatus::Success),
        "interface-bound HTTP/1 failed: {:?}",
        result.status
    );
    assert_eq!(result.http_status, Some(200));
}

#[tokio::test]
async fn bench_tcp_direct_serve() {
    let opts = BenchOptions {
        transport: direct_transport(),
        target: None, // --serve picks 127.0.0.1:<auto>
        serve: true,
        serve_port: None,
        mode: BenchMode::Tcp,
        direction: BenchDirection::Both,
        duration: Duration::from_secs(1),
        udp_bandwidth: vec![],
        udp_size: 1200,
        tcp_chunk: 65536,
    };
    let outcomes = run_client_quiet(opts).await.expect("bench tcp run failed");

    assert_eq!(outcomes.len(), 2, "expected TCP up + TCP down");

    // Upload: client.bytes ≈ server.bytes (server may report slightly less if
    // the connection closed mid-flight). Both should be > 0.
    let up = &outcomes[0];
    assert!(up.client.bytes > 0, "TCP upload sent nothing");
    assert!(up.server.bytes > 0, "TCP upload server received nothing");

    let down = &outcomes[1];
    assert!(down.client.bytes > 0, "TCP download received nothing");
    assert!(down.server.bytes > 0, "TCP download server sent nothing");
}

#[tokio::test]
async fn bench_udp_direct_serve_low_bandwidth() {
    // 1 Mbit/s for 1 second over loopback — should see zero loss.
    let opts = BenchOptions {
        transport: direct_transport(),
        target: None,
        serve: true,
        serve_port: None,
        mode: BenchMode::Udp,
        direction: BenchDirection::Up, // upload only — keep test short
        duration: Duration::from_secs(1),
        udp_bandwidth: vec![1_000_000],
        udp_size: 1200,
        tcp_chunk: 65536,
    };
    let outcomes = run_client_quiet(opts).await.expect("bench udp run failed");

    assert_eq!(outcomes.len(), 1, "expected one UDP plan");
    let o = &outcomes[0];

    // We sent something
    assert!(o.client.packets > 0, "UDP sender sent zero packets");
    assert!(o.server.packets > 0, "UDP receiver got zero packets");

    // Loopback should have negligible loss
    let lost = o.client.packets.saturating_sub(o.server.packets);
    let loss_pct = lost as f64 * 100.0 / o.client.packets as f64;
    assert!(
        loss_pct < 5.0,
        "loopback UDP loss too high: {loss_pct:.2}% ({lost}/{} pkt)",
        o.client.packets
    );

    // No duplicates and no out-of-order on loopback
    assert_eq!(o.server.dup, 0, "duplicates seen on loopback");
    assert_eq!(o.server.ooo, 0, "out-of-order seen on loopback");
}

#[tokio::test]
async fn bench_validation_target_required_without_serve() {
    let opts = BenchOptions {
        transport: direct_transport(),
        target: None,
        serve: false,
        serve_port: None,
        mode: BenchMode::Tcp,
        direction: BenchDirection::Up,
        duration: Duration::from_secs(1),
        udp_bandwidth: vec![],
        udp_size: 1200,
        tcp_chunk: 65536,
    };
    let err = run_client_quiet(opts).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("--target") || msg.contains("--serve"),
        "expected target/serve mention, got: {msg}"
    );
}
