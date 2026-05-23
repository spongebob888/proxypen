// Helpers shared by integration tests. Each test binary uses a subset of
// these — silence dead-code warnings for the parts a given binary doesn't
// happen to call.
#![allow(dead_code)]


use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Once;

use shadowquic::Manager;
use shadowquic::config::{AuthUser, DirectOutCfg, SocksServerCfg};
use shadowquic::direct::outbound::DirectOut;
use shadowquic::socks::inbound::SocksServer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::oneshot;

/// Pick a free TCP port by binding ephemeral and immediately dropping the
/// listener. There is a tiny TOCTOU window — fine in tests.
pub async fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

static RUSTLS_INIT: Once = Once::new();

/// Install the rustls ring crypto provider once per process. Tests touching
/// HTTP/2 / HTTP/3 (or anything that uses our TLS path) must call this.
pub fn install_rustls() {
    RUSTLS_INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// A tiny HTTP/1.1 server that always replies `200 OK` with the supplied body.
/// Returns the bound address; the server task runs until the test ends.
pub async fn start_http1_echo(body: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf).await; // drain request line + headers
                let resp = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\
                     \r\n\
                     {}",
                    body.len(),
                    body
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });
    addr
}

/// Holds a running shadowquic SOCKS5 server. The server stops when this
/// guard is dropped (via the abort signal on the spawned task).
pub struct Socks5Guard {
    pub addr: SocketAddr,
    _abort: tokio::task::JoinHandle<()>,
}

/// Start a SOCKS5 server (no-auth) on a free localhost port using
/// shadowquic's SocksServer + DirectOut. Returns the bind address.
pub async fn start_socks5_server() -> Socks5Guard {
    start_socks5_server_with_auth(vec![]).await
}

pub async fn start_socks5_server_with_auth(users: Vec<AuthUser>) -> Socks5Guard {
    let port = free_port().await;
    let bind: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let cfg = SocksServerCfg {
        bind_addr: bind,
        users,
    };
    let inbound = SocksServer::new(cfg)
        .await
        .expect("shadowquic SocksServer failed to bind");
    let outbound = DirectOut {
        cfg: DirectOutCfg::default(),
    };
    let manager = Manager {
        inbound: Box::new(inbound),
        outbound: Box::new(outbound),
    };
    let handle = tokio::spawn(async move {
        let _ = manager.run().await;
    });

    // Wait briefly for the listener to be ready (the bind already happened
    // inside SocksServer::new, so this is just a yield).
    tokio::task::yield_now().await;

    Socks5Guard {
        addr: bind,
        _abort: handle,
    }
}

impl Drop for Socks5Guard {
    fn drop(&mut self) {
        self._abort.abort();
    }
}

/// A toy DNS server that answers every query with the supplied A record.
/// Returns the bind address and a `oneshot::Sender` whose drop stops the
/// server.
pub async fn start_fake_dns(answer: Ipv4Addr) -> (SocketAddr, oneshot::Sender<()>) {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = sock.local_addr().unwrap();
    let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 1500];
        loop {
            tokio::select! {
                _ = &mut stop_rx => return,
                r = sock.recv_from(&mut buf) => {
                    let (n, peer) = match r { Ok(v) => v, Err(_) => continue };
                    if let Some(resp) = build_dns_response(&buf[..n], answer) {
                        let _ = sock.send_to(&resp, peer).await;
                    }
                }
            }
        }
    });
    (addr, stop_tx)
}

fn build_dns_response(req: &[u8], answer: Ipv4Addr) -> Option<Vec<u8>> {
    if req.len() < 12 {
        return None;
    }
    let mut r = req.to_vec();
    // QR=1 (response), keep RD; RA=1, RCODE=0.
    r[2] |= 0x80;
    r[3] = 0x80;
    // ANCOUNT = 1
    r[6] = 0;
    r[7] = 1;
    // Append answer: NAME pointer to offset 12, TYPE=A, CLASS=IN, TTL=60, RDLEN=4, IP.
    r.extend_from_slice(&[0xC0, 0x0C]);
    r.extend_from_slice(&1u16.to_be_bytes());
    r.extend_from_slice(&1u16.to_be_bytes());
    r.extend_from_slice(&60u32.to_be_bytes());
    r.extend_from_slice(&4u16.to_be_bytes());
    r.extend_from_slice(&answer.octets());
    Some(r)
}

/// Start the proxypen bench server in-process and return the bound address.
/// The server stops when the returned shutdown sender is dropped.
pub async fn start_bench_server() -> (SocketAddr, oneshot::Sender<()>) {
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    tokio::spawn(async move {
        let _ = proxypen::bench::run_server_until_signal(
            "127.0.0.1".parse().unwrap(),
            0,
            ready_tx,
            shutdown_rx,
        )
        .await;
    });
    let addr = ready_rx.await.unwrap();
    (addr, shutdown_tx)
}
