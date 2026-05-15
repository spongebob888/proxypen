use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

use crate::bench::pacer::Pacer;
use crate::bench::protocol::{
    KIND_TCP_DOWN, KIND_TCP_UP, KIND_UDP_DOWN, KIND_UDP_UP, STATUS_OK, TestReport, TestRequest,
    TestResponse, UDP_HEADER_LEN, decode_udp_header, encode_udp_header, read_handshake,
    write_handshake,
};
use crate::bench::server;
use crate::bench::stats::UdpStats;
use crate::bench::udp_io::BenchUdpSocket;
use crate::config::TestTarget;
use crate::transport::Transport;

const STRAGGLE_GRACE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub struct BenchOptions {
    pub transport: Transport,
    pub target: Option<SocketAddr>,
    pub serve: bool,
    pub serve_port: Option<u16>,
    pub mode: BenchMode,
    pub direction: BenchDirection,
    pub duration: Duration,
    pub udp_bandwidth: Vec<u64>,
    pub udp_size: usize,
    pub tcp_chunk: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchMode {
    Tcp,
    Udp,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchDirection {
    Up,
    Down,
    Both,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub kind: u8,
    pub bandwidth_bps: u64,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct BenchOutcome {
    pub plan: Plan,
    pub client: TestReport,
    pub server: TestReport,
}

/// Run the benchmark, print results to stdout, and return the per-test outcomes.
pub async fn run(opts: BenchOptions) -> anyhow::Result<Vec<BenchOutcome>> {
    run_inner(opts, true).await
}

/// Run the benchmark without printing anything. For tests / programmatic use.
pub async fn run_quiet(opts: BenchOptions) -> anyhow::Result<Vec<BenchOutcome>> {
    run_inner(opts, false).await
}

async fn run_inner(opts: BenchOptions, print: bool) -> anyhow::Result<Vec<BenchOutcome>> {
    // Optional in-process server (--serve).
    let _serve_guard = if opts.serve {
        Some(spawn_local_server(&opts).await?)
    } else {
        None
    };

    let target = match opts.target {
        Some(t) => t,
        None => {
            let guard = _serve_guard
                .as_ref()
                .ok_or_else(|| anyhow!("--target is required when --serve is not set"))?;
            guard.addr
        }
    };

    let plans = build_plans(&opts);
    if plans.is_empty() {
        bail!("no tests selected (check --mode/--direction/--udp-bandwidth)");
    }

    if print {
        println!(
            "bench client → {target}    {}",
            match &opts.transport {
                Transport::Direct(_) => "direct".to_string(),
                Transport::Socks5(c) => format!("via socks5://{}", c.addr),
            }
        );
        println!();
    }

    let mut control = opts
        .transport
        .connect_tcp(&synthetic_target(target))
        .await?;
    write_handshake(&mut control).await?;
    read_handshake(&mut control).await?;

    let mut udp_used = false;
    let mut outcomes: Vec<BenchOutcome> = Vec::new();

    for plan in plans {
        let req = TestRequest {
            kind: plan.kind,
            duration_ms: opts.duration.as_millis() as u32,
            payload_size: if plan.kind == KIND_UDP_UP || plan.kind == KIND_UDP_DOWN {
                opts.udp_size as u32
            } else {
                opts.tcp_chunk as u32
            },
            bandwidth_bps: plan.bandwidth_bps,
            expected_packets: 0,
        };
        req.write(&mut control).await?;
        let resp = TestResponse::read(&mut control).await?;
        if resp.status != STATUS_OK {
            bail!("server rejected test {}: {}", plan.label, resp.error);
        }

        let server_data_addr = SocketAddr::new(target.ip(), resp.data_port);
        let (client_report, server_report) = match plan.kind {
            KIND_TCP_UP | KIND_TCP_DOWN => {
                let client_report = run_tcp_data(
                    &opts.transport,
                    server_data_addr,
                    plan.kind,
                    opts.duration,
                    opts.tcp_chunk,
                )
                .await?;
                let server_report = TestReport::read(&mut control).await?;
                client_report.write(&mut control).await?;
                (client_report, server_report)
            }
            KIND_UDP_UP => {
                udp_used = true;
                let client_report = run_udp_up(
                    &opts.transport,
                    server_data_addr,
                    opts.duration,
                    opts.udp_size,
                    plan.bandwidth_bps,
                )
                .await?;
                let server_report = TestReport::read(&mut control).await?;
                client_report.write(&mut control).await?;
                (client_report, server_report)
            }
            KIND_UDP_DOWN => {
                udp_used = true;
                let client_report = run_udp_down(
                    &opts.transport,
                    server_data_addr,
                    opts.duration,
                    opts.udp_size,
                )
                .await?;
                let server_report = TestReport::read(&mut control).await?;
                client_report.write(&mut control).await?;
                (client_report, server_report)
            }
            _ => unreachable!(),
        };

        if print {
            print_result(&plan, &client_report, &server_report);
        }
        outcomes.push(BenchOutcome {
            plan,
            client: client_report,
            server: server_report,
        });
    }

    TestRequest::close().write(&mut control).await?;
    let _ = control.shutdown().await;

    if print && udp_used {
        println!();
        println!(
            "note: one-way latency is relative to the path delay of the first packet; \
             use it to compare runs between the same hosts (e.g. proxy vs direct), \
             not as an absolute measurement."
        );
    }

    Ok(outcomes)
}

// ------------- in-process server (--serve) -------------

struct LocalServerGuard {
    addr: SocketAddr,
    _shutdown: oneshot::Sender<()>,
}

async fn spawn_local_server(opts: &BenchOptions) -> anyhow::Result<LocalServerGuard> {
    let bind: IpAddr = "127.0.0.1".parse().unwrap();
    let port = opts.serve_port.unwrap_or(0);
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    tokio::spawn(async move {
        if let Err(e) = server::run_until_signal(bind, port, ready_tx, shutdown_rx).await {
            tracing::error!("local bench server error: {e}");
        }
    });
    let addr = ready_rx.await?;
    Ok(LocalServerGuard {
        addr,
        _shutdown: shutdown_tx,
    })
}

// ------------- planning -------------

fn build_plans(opts: &BenchOptions) -> Vec<Plan> {
    let want_tcp = matches!(opts.mode, BenchMode::Tcp | BenchMode::Both);
    let want_udp = matches!(opts.mode, BenchMode::Udp | BenchMode::Both);
    let want_up = matches!(opts.direction, BenchDirection::Up | BenchDirection::Both);
    let want_down = matches!(opts.direction, BenchDirection::Down | BenchDirection::Both);

    let mut plans = Vec::new();
    if want_tcp {
        if want_up {
            plans.push(Plan {
                kind: KIND_TCP_UP,
                bandwidth_bps: 0,
                label: "TCP upload".into(),
            });
        }
        if want_down {
            plans.push(Plan {
                kind: KIND_TCP_DOWN,
                bandwidth_bps: 0,
                label: "TCP download".into(),
            });
        }
    }
    if want_udp {
        for &bw in &opts.udp_bandwidth {
            if want_up {
                plans.push(Plan {
                    kind: KIND_UDP_UP,
                    bandwidth_bps: bw,
                    label: format!("UDP upload @ {}", format_rate_bps(bw)),
                });
            }
            if want_down {
                plans.push(Plan {
                    kind: KIND_UDP_DOWN,
                    bandwidth_bps: bw,
                    label: format!("UDP download @ {}", format_rate_bps(bw)),
                });
            }
        }
    }
    plans
}

// ------------- TCP -------------

async fn run_tcp_data(
    transport: &Transport,
    server_data_addr: SocketAddr,
    kind: u8,
    duration: Duration,
    chunk: usize,
) -> anyhow::Result<TestReport> {
    let target = synthetic_target(server_data_addr);
    let mut stream = transport.connect_tcp(&target).await?;

    let start = Instant::now();
    let mut bytes: u64 = 0;

    if kind == KIND_TCP_UP {
        let buf = vec![0xa5u8; chunk];
        let deadline = start + duration;
        while Instant::now() < deadline {
            match stream.write(&buf).await {
                Ok(0) => break,
                Ok(n) => bytes += n as u64,
                Err(_) => break,
            }
        }
        let _ = stream.shutdown().await;
    } else {
        let mut buf = vec![0u8; chunk];
        let deadline = start + duration + STRAGGLE_GRACE;
        loop {
            let n = tokio::select! {
                r = stream.read(&mut buf) => r?,
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => 0,
            };
            if n == 0 {
                break;
            }
            bytes += n as u64;
        }
    }

    Ok(TestReport {
        bytes,
        duration_actual_ns: start.elapsed().as_nanos() as u64,
        ..Default::default()
    })
}

// ------------- UDP -------------

async fn run_udp_up(
    transport: &Transport,
    server_udp_addr: SocketAddr,
    duration: Duration,
    datagram_size: usize,
    bandwidth_bps: u64,
) -> anyhow::Result<TestReport> {
    let socket = BenchUdpSocket::open(transport, server_udp_addr).await?;
    let mut buf = vec![0u8; datagram_size.max(UDP_HEADER_LEN)];

    let session_start = Instant::now();

    // Hello: seq=0, ts=0.
    encode_udp_header(&mut buf[..UDP_HEADER_LEN], 0, 0);
    socket.send(&buf).await?;

    let deadline = session_start + duration;
    let pacer = Pacer::new(bandwidth_bps, buf.len());
    let mut seq: u64 = 1;
    let mut bytes: u64 = buf.len() as u64; // include hello

    'outer: loop {
        if Instant::now() >= deadline {
            break;
        }
        // The hello (seq 0) was already sent before the loop. Compare against
        // the next packet index to send (seq), which equals total packets sent
        // so far including hello.
        let due = pacer.target_packets_now();
        while seq < due {
            if Instant::now() >= deadline {
                break 'outer;
            }
            let send_ts = session_start.elapsed().as_nanos() as u64;
            encode_udp_header(&mut buf[..UDP_HEADER_LEN], seq, send_ts);
            match socket.send(&buf).await {
                Ok(()) => {
                    bytes += buf.len() as u64;
                    seq += 1;
                }
                Err(_) => break 'outer,
            }
        }
        pacer.sleep_until_packet(seq).await;
    }

    Ok(TestReport {
        bytes,
        packets: seq,
        max_seq: seq.saturating_sub(1),
        duration_actual_ns: session_start.elapsed().as_nanos() as u64,
        ..Default::default()
    })
}

async fn run_udp_down(
    transport: &Transport,
    server_udp_addr: SocketAddr,
    duration: Duration,
    datagram_size: usize,
) -> anyhow::Result<TestReport> {
    let socket = BenchUdpSocket::open(transport, server_udp_addr).await?;
    let mut hello = vec![0u8; datagram_size.max(UDP_HEADER_LEN)];
    encode_udp_header(&mut hello[..UDP_HEADER_LEN], 0, 0);
    socket.send(&hello).await?;

    let mut buf = vec![0u8; datagram_size.max(UDP_HEADER_LEN) + 64];
    let mut stats = UdpStats::default();
    let mut session_start: Option<Instant> = None;

    let test_start = Instant::now();
    let stop_at = test_start + duration + STRAGGLE_GRACE;

    loop {
        let r = tokio::select! {
            r = socket.recv(&mut buf) => r,
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(stop_at)) => break,
        };
        let n = match r {
            Ok(n) => n,
            Err(_) => break,
        };
        if session_start.is_none() {
            session_start = Some(Instant::now());
        }
        let s = session_start.unwrap();
        let recv_ts_ns = s.elapsed().as_nanos() as u64;
        if let Some((seq, send_ts_ns)) = decode_udp_header(&buf[..n]) {
            stats.record(seq, send_ts_ns, recv_ts_ns, n);
        }
    }

    let duration_actual_ns = session_start
        .map(|s| s.elapsed().as_nanos() as u64)
        .unwrap_or(0);
    Ok(stats.into_report(duration_actual_ns))
}

// ------------- helpers -------------

fn synthetic_target(addr: SocketAddr) -> TestTarget {
    TestTarget {
        host: addr.ip().to_string(),
        port: addr.port(),
        path: String::new(),
        use_tls: false,
        resolved_addr: Some(addr.ip()),
    }
}

// ------------- formatting -------------

fn print_result(plan: &Plan, client: &TestReport, server: &TestReport) {
    println!("== {} ==", plan.label);
    match plan.kind {
        KIND_TCP_UP => {
            let dur = client.duration_actual_ns as f64 / 1e9;
            let rate = (client.bytes as f64 * 8.0) / dur;
            println!(
                "  sent: {}    rate: {}    duration: {:.2}s",
                format_bytes(client.bytes),
                format_rate_bps(rate as u64),
                dur
            );
            // Server reports actual bytes received — useful sanity check.
            println!(
                "  recv (server side): {}",
                format_bytes(server.bytes)
            );
        }
        KIND_TCP_DOWN => {
            let dur = client.duration_actual_ns as f64 / 1e9;
            let rate = (client.bytes as f64 * 8.0) / dur;
            println!(
                "  recv: {}    rate: {}    duration: {:.2}s",
                format_bytes(client.bytes),
                format_rate_bps(rate as u64),
                dur
            );
        }
        KIND_UDP_UP => {
            // Sender = client, receiver = server.
            print_udp(plan.bandwidth_bps, client, server);
        }
        KIND_UDP_DOWN => {
            // Sender = server, receiver = client.
            print_udp(plan.bandwidth_bps, server, client);
        }
        _ => {}
    }
    println!();
}

fn print_udp(target_bw: u64, sender: &TestReport, receiver: &TestReport) {
    let send_dur = sender.duration_actual_ns as f64 / 1e9;
    // Use the sender's wall-clock duration as the denominator for both rates,
    // since "received throughput" is most meaningfully measured against the
    // time the sender took to push that data — the receiver's locally
    // measured window includes the straggle-grace tail and would understate
    // the rate.
    let send_rate = if send_dur > 0.0 {
        sender.bytes as f64 * 8.0 / send_dur
    } else {
        0.0
    };
    let recv_rate = if send_dur > 0.0 {
        receiver.bytes as f64 * 8.0 / send_dur
    } else {
        0.0
    };
    let loss = if sender.packets > 0 {
        let lost = sender.packets.saturating_sub(receiver.packets);
        (lost as f64 / sender.packets as f64) * 100.0
    } else {
        0.0
    };
    let avg_lat_ns = if receiver.packets > 0 {
        receiver.latency_sum_ns / receiver.packets
    } else {
        0
    };

    println!(
        "  sent:  {:>7} pkt / {:>10}    rate: {:>12}",
        sender.packets,
        format_bytes(sender.bytes),
        format_rate_bps(send_rate as u64),
    );
    println!(
        "  recv:  {:>7} pkt / {:>10}    rate: {:>12}",
        receiver.packets,
        format_bytes(receiver.bytes),
        format_rate_bps(recv_rate as u64),
    );
    println!(
        "  loss: {:.2}%   ooo: {}   dup: {}   jitter: {}",
        loss,
        receiver.ooo,
        receiver.dup,
        format_ms(receiver.jitter_ns),
    );
    println!(
        "  latency min/avg/max: {} / {} / {}    target: {}",
        format_ms(receiver.latency_min_ns),
        format_ms(avg_lat_ns),
        format_ms(receiver.latency_max_ns),
        format_rate_bps(target_bw),
    );
}

pub fn format_rate_bps(bps: u64) -> String {
    let v = bps as f64;
    if v >= 1e9 {
        format!("{:.2} Gbit/s", v / 1e9)
    } else if v >= 1e6 {
        format!("{:.2} Mbit/s", v / 1e6)
    } else if v >= 1e3 {
        format!("{:.2} Kbit/s", v / 1e3)
    } else {
        format!("{} bit/s", bps)
    }
}

pub fn format_bytes(b: u64) -> String {
    let v = b as f64;
    if v >= 1e9 {
        format!("{:.2} GB", v / 1e9)
    } else if v >= 1e6 {
        format!("{:.2} MB", v / 1e6)
    } else if v >= 1e3 {
        format!("{:.2} KB", v / 1e3)
    } else {
        format!("{} B", b)
    }
}

pub fn format_ms(ns: u64) -> String {
    let v = ns as f64 / 1e6;
    format!("{:.2} ms", v)
}

// ------------- bandwidth list parsing -------------

pub fn parse_bandwidth_list(s: &str) -> anyhow::Result<Vec<u64>> {
    s.split(',')
        .map(|p| parse_bandwidth(p.trim()))
        .collect()
}

#[cfg(test)]
mod parse_bandwidth_tests {
    use super::*;

    #[test]
    fn plain_integer() {
        assert_eq!(parse_bandwidth("1000").unwrap(), 1000);
    }

    #[test]
    fn si_suffixes() {
        assert_eq!(parse_bandwidth("1k").unwrap(), 1_000);
        assert_eq!(parse_bandwidth("1K").unwrap(), 1_000);
        assert_eq!(parse_bandwidth("5M").unwrap(), 5_000_000);
        assert_eq!(parse_bandwidth("2G").unwrap(), 2_000_000_000);
    }

    #[test]
    fn fractional_with_suffix() {
        assert_eq!(parse_bandwidth("1.5M").unwrap(), 1_500_000);
        assert_eq!(parse_bandwidth("0.5G").unwrap(), 500_000_000);
    }

    #[test]
    fn list_of_bandwidths() {
        let v = parse_bandwidth_list("1M,5M,10M").unwrap();
        assert_eq!(v, vec![1_000_000, 5_000_000, 10_000_000]);
    }

    #[test]
    fn list_handles_whitespace() {
        let v = parse_bandwidth_list("1M , 5M ,10M").unwrap();
        assert_eq!(v, vec![1_000_000, 5_000_000, 10_000_000]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_bandwidth("abc").is_err());
        assert!(parse_bandwidth("").is_err());
    }
}

pub fn parse_bandwidth(s: &str) -> anyhow::Result<u64> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty bandwidth value");
    }
    let (num, mult) = match s.chars().last() {
        Some('K' | 'k') => (&s[..s.len() - 1], 1_000_u64),
        Some('M' | 'm') => (&s[..s.len() - 1], 1_000_000_u64),
        Some('G' | 'g') => (&s[..s.len() - 1], 1_000_000_000_u64),
        _ => (s, 1u64),
    };
    let n: f64 = num
        .parse()
        .map_err(|_| anyhow!("invalid bandwidth value: {s}"))?;
    Ok((n * mult as f64) as u64)
}
