// SOCKS5-mode integration tests using shadowquic as an in-process SOCKS5 server.

mod common;

use std::time::Duration;

use proxypen::bench::{
    BenchDirection, BenchMode, BenchOptions, run_client_quiet,
};
use proxypen::{ProxyConfig, ProxyPen, TestStatus, TestTarget, Transport};

use common::{install_rustls, start_http1_echo, start_socks5_server};

fn target_for(addr: std::net::SocketAddr) -> TestTarget {
    TestTarget {
        host: addr.ip().to_string(),
        port: addr.port(),
        path: "/".into(),
        use_tls: false,
        resolved_addr: Some(addr.ip()),
    }
}

fn socks_transport(addr: std::net::SocketAddr) -> Transport {
    Transport::Socks5(ProxyConfig {
        addr: addr.to_string(),
        auth: None,
    })
}

#[tokio::test]
async fn http1_through_socks5() {
    install_rustls();
    let proxy = start_socks5_server().await;
    let upstream = start_http1_echo("hello via socks").await;

    let pen = ProxyPen::new(socks_transport(proxy.addr));
    let target = target_for(upstream);
    let result = pen.test_http1(&target, Duration::from_secs(5)).await;

    assert!(
        matches!(result.status, TestStatus::Success),
        "expected success, got {:?}",
        result.status
    );
    assert_eq!(result.http_status, Some(200));

    // Proxy mode should record socks_handshake, not tcp_connect.
    assert!(result.timing.socks_handshake.is_some());
    assert!(result.timing.tcp_connect.is_none());
}

#[tokio::test]
async fn bench_tcp_through_socks5() {
    let proxy = start_socks5_server().await;

    let opts = BenchOptions {
        transport: socks_transport(proxy.addr),
        target: None,
        serve: true, // bench server runs in-process; tunnel goes proxy → 127.0.0.1
        serve_port: None,
        mode: BenchMode::Tcp,
        direction: BenchDirection::Both,
        duration: Duration::from_secs(1),
        udp_bandwidth: vec![],
        udp_size: 1200,
        tcp_chunk: 65536,
    };
    let outcomes = run_client_quiet(opts).await.expect("bench tcp via socks5 failed");

    assert_eq!(outcomes.len(), 2);
    let up = &outcomes[0];
    assert!(up.client.bytes > 0, "TCP-up over SOCKS5 sent nothing");
    assert!(up.server.bytes > 0, "TCP-up over SOCKS5 received nothing");
    let down = &outcomes[1];
    assert!(down.client.bytes > 0, "TCP-down over SOCKS5 received nothing");
}

#[tokio::test]
async fn bench_udp_through_socks5() {
    // Verifies the full SOCKS5 UDP-ASSOCIATE relay path:
    // client → SOCKS5 (UDP relay) → bench server → SOCKS5 → client.
    let proxy = start_socks5_server().await;

    let opts = BenchOptions {
        transport: socks_transport(proxy.addr),
        target: None,
        serve: true,
        serve_port: None,
        mode: BenchMode::Udp,
        direction: BenchDirection::Up,
        duration: Duration::from_secs(1),
        udp_bandwidth: vec![1_000_000],
        udp_size: 1200,
        tcp_chunk: 65536,
    };
    let outcomes = run_client_quiet(opts).await.expect("bench udp via socks5 failed");

    assert_eq!(outcomes.len(), 1);
    let o = &outcomes[0];
    assert!(o.client.packets > 0, "UDP-up over SOCKS5 sent nothing");
    assert!(
        o.server.packets > 0,
        "UDP-up over SOCKS5 — proxy didn't relay any packets"
    );

    let lost = o.client.packets.saturating_sub(o.server.packets);
    let loss_pct = lost as f64 * 100.0 / o.client.packets as f64;
    // Loopback through a relay can have a packet or two lost on startup;
    // 10% is generous but still catches a fundamentally broken relay.
    assert!(
        loss_pct < 10.0,
        "SOCKS5 UDP loss too high: {loss_pct:.2}% ({lost}/{} pkt)",
        o.client.packets
    );
}
