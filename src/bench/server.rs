use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use crate::bench::pacer::Pacer;
use crate::bench::protocol::{
    KIND_CLOSE, KIND_TCP_DOWN, KIND_TCP_UP, KIND_UDP_DOWN, KIND_UDP_UP, TestReport, TestRequest,
    TestResponse, UDP_HEADER_LEN, decode_udp_header, encode_udp_header, read_handshake,
    write_handshake,
};
use crate::bench::stats::UdpStats;

const STRAGGLE_GRACE: Duration = Duration::from_millis(500);

pub async fn run(bind: IpAddr, port: u16) -> anyhow::Result<()> {
    let addr = SocketAddr::new(bind, port);
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    eprintln!("bench server listening on {local}");
    accept_loop(listener).await
}

/// Run a server, then signal `ready` with the local address. Stops accepting
/// when `shutdown` resolves. Used by `--serve` mode.
pub async fn run_until_signal(
    bind: IpAddr,
    port: u16,
    ready: tokio::sync::oneshot::Sender<SocketAddr>,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(SocketAddr::new(bind, port)).await?;
    let _ = ready.send(listener.local_addr()?);
    loop {
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            accept = listener.accept() => {
                let (stream, peer) = accept?;
                tokio::spawn(async move {
                    if let Err(e) = handle_session(stream, peer).await {
                        tracing::warn!("bench session from {peer} error: {e}");
                    }
                });
            }
        }
    }
}

async fn accept_loop(listener: TcpListener) -> anyhow::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(e) = handle_session(stream, peer).await {
                tracing::warn!("bench session from {peer} error: {e}");
            }
        });
    }
}

async fn handle_session(mut control: TcpStream, peer: SocketAddr) -> anyhow::Result<()> {
    control.set_nodelay(true)?;
    read_handshake(&mut control).await?;
    write_handshake(&mut control).await?;

    loop {
        let req = TestRequest::read(&mut control).await?;
        match req.kind {
            KIND_CLOSE => return Ok(()),
            KIND_TCP_UP => run_tcp_up(&mut control, &req).await?,
            KIND_TCP_DOWN => run_tcp_down(&mut control, &req).await?,
            KIND_UDP_UP => run_udp_up(&mut control, &req, peer.ip()).await?,
            KIND_UDP_DOWN => run_udp_down(&mut control, &req, peer.ip()).await?,
            other => {
                let resp = TestResponse::err(format!("unknown test kind {other}"));
                resp.write(&mut control).await?;
                return Ok(());
            }
        }
    }
}

fn local_bind_ip(control: &TcpStream) -> io::Result<IpAddr> {
    Ok(control.local_addr()?.ip())
}

async fn open_data_listener(bind_ip: IpAddr) -> io::Result<TcpListener> {
    TcpListener::bind(SocketAddr::new(bind_ip, 0)).await
}

async fn accept_one(listener: &TcpListener) -> io::Result<TcpStream> {
    let (stream, _) = listener.accept().await?;
    stream.set_nodelay(true)?;
    Ok(stream)
}

// ---------- TCP ----------

async fn run_tcp_up(control: &mut TcpStream, req: &TestRequest) -> anyhow::Result<()> {
    // Server is the receiver. Open a TCP data listener; client connects to it.
    let bind_ip = local_bind_ip(control)?;
    let listener = open_data_listener(bind_ip).await?;
    let port = listener.local_addr()?.port();
    TestResponse::ok(port).write(control).await?;

    let mut data = accept_one(&listener).await?;

    let chunk = req.payload_size.max(1) as usize;
    let mut buf = vec![0u8; chunk];
    let start = Instant::now();
    let deadline = start + Duration::from_millis(req.duration_ms as u64) + STRAGGLE_GRACE;
    let mut bytes: u64 = 0;

    loop {
        let n = tokio::select! {
            r = data.read(&mut buf) => r?,
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => 0,
        };
        if n == 0 {
            break;
        }
        bytes += n as u64;
    }
    let duration_actual_ns = start.elapsed().as_nanos() as u64;
    let _ = data.shutdown().await;

    let report = TestReport {
        bytes,
        duration_actual_ns,
        ..Default::default()
    };
    report.write(control).await?;
    let _client_report = TestReport::read(control).await?;
    Ok(())
}

async fn run_tcp_down(control: &mut TcpStream, req: &TestRequest) -> anyhow::Result<()> {
    // Server is the sender.
    let bind_ip = local_bind_ip(control)?;
    let listener = open_data_listener(bind_ip).await?;
    let port = listener.local_addr()?.port();
    TestResponse::ok(port).write(control).await?;

    let mut data = accept_one(&listener).await?;

    let chunk = req.payload_size.max(1) as usize;
    let buf = vec![0xa5u8; chunk];
    let start = Instant::now();
    let deadline = start + Duration::from_millis(req.duration_ms as u64);
    let mut bytes: u64 = 0;

    while Instant::now() < deadline {
        match data.write(&buf).await {
            Ok(0) => break,
            Ok(n) => bytes += n as u64,
            Err(_) => break,
        }
    }
    let duration_actual_ns = start.elapsed().as_nanos() as u64;
    let _ = data.shutdown().await;

    let report = TestReport {
        bytes,
        duration_actual_ns,
        ..Default::default()
    };
    report.write(control).await?;
    let _client_report = TestReport::read(control).await?;
    Ok(())
}

// ---------- UDP ----------

async fn bind_udp_data(bind_ip: IpAddr) -> io::Result<UdpSocket> {
    UdpSocket::bind(SocketAddr::new(bind_ip, 0)).await
}

async fn run_udp_up(
    control: &mut TcpStream,
    req: &TestRequest,
    _peer_ip: IpAddr,
) -> anyhow::Result<()> {
    let bind_ip = local_bind_ip(control)?;
    let socket = bind_udp_data(bind_ip).await?;
    let port = socket.local_addr()?.port();
    TestResponse::ok(port).write(control).await?;

    let datagram_size = req.payload_size.max(UDP_HEADER_LEN as u32) as usize;
    let mut buf = vec![0u8; datagram_size + 64];
    let mut stats = UdpStats::default();

    // Wait for hello (anchors session_start; gives us the source addr).
    let (n, peer) = socket.recv_from(&mut buf).await?;
    let session_start = Instant::now();
    process_packet(&mut stats, &buf[..n], session_start);

    let deadline = session_start + Duration::from_millis(req.duration_ms as u64) + STRAGGLE_GRACE;

    loop {
        tokio::select! {
            r = socket.recv_from(&mut buf) => {
                let (n, src) = r?;
                if src != peer { continue; }
                process_packet(&mut stats, &buf[..n], session_start);
            }
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => break,
        }
    }
    let duration_actual_ns = session_start.elapsed().as_nanos() as u64;

    let report = stats.into_report(duration_actual_ns);
    report.write(control).await?;
    let _client_report = TestReport::read(control).await?;
    Ok(())
}

async fn run_udp_down(
    control: &mut TcpStream,
    req: &TestRequest,
    _peer_ip: IpAddr,
) -> anyhow::Result<()> {
    let bind_ip = local_bind_ip(control)?;
    let socket = bind_udp_data(bind_ip).await?;
    let port = socket.local_addr()?.port();
    TestResponse::ok(port).write(control).await?;

    let datagram_size = req.payload_size.max(UDP_HEADER_LEN as u32) as usize;

    // Wait for hello so we know where to send.
    let mut tmp = vec![0u8; datagram_size + 64];
    let (_n, peer) = socket.recv_from(&mut tmp).await?;

    let session_start = Instant::now();
    let deadline = session_start + Duration::from_millis(req.duration_ms as u64);
    let mut buf = vec![0u8; datagram_size];
    let pacer = Pacer::new(req.bandwidth_bps, datagram_size);
    let mut seq: u64 = 0;
    let mut bytes: u64 = 0;

    'outer: loop {
        if Instant::now() >= deadline {
            break;
        }
        let due = pacer.target_packets_now();
        while seq < due {
            if Instant::now() >= deadline {
                break 'outer;
            }
            let send_ts = session_start.elapsed().as_nanos() as u64;
            encode_udp_header(&mut buf[..UDP_HEADER_LEN], seq, send_ts);
            match socket.send_to(&buf, peer).await {
                Ok(n) => bytes += n as u64,
                Err(_) => break 'outer,
            }
            seq += 1;
        }
        pacer.sleep_until_packet(seq).await;
    }
    let duration_actual_ns = session_start.elapsed().as_nanos() as u64;

    let report = TestReport {
        bytes,
        packets: seq,
        max_seq: seq.saturating_sub(1),
        duration_actual_ns,
        ..Default::default()
    };
    report.write(control).await?;
    let _client_report = TestReport::read(control).await?;
    Ok(())
}

fn process_packet(stats: &mut UdpStats, buf: &[u8], session_start: Instant) {
    let recv_ts_ns = session_start.elapsed().as_nanos() as u64;
    if let Some((seq, send_ts_ns)) = decode_udp_header(buf) {
        stats.record(seq, send_ts_ns, recv_ts_ns, buf.len());
    }
}
