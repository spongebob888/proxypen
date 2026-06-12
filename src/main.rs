use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use proxypen::{
    DirectConfig, HttpServerConfig, InterfaceSpec, PressOptions, PressProtocol, ProxyAuth,
    ProxyConfig, ProxyPen, TestTarget, Transport,
    bench::{
        BenchDirection, BenchMode, BenchOptions, parse_bandwidth_list, run_client, run_server,
    },
    direct::parse_interface,
    dns::{self, DnsConfig},
    press,
    run_http_server,
};
use url::Url;

#[derive(Parser)]
#[command(
    name = "proxypen",
    about = "Test HTTP/1, HTTP/2, HTTP/3 over a SOCKS5 proxy or directly. \
             Includes a TCP/UDP throughput benchmark."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    test_args: TestArgs,
}

#[derive(Subcommand)]
enum Command {
    /// HTTP/1, HTTP/2, HTTP/3 protocol test (same as the flat invocation).
    Test(TestArgs),
    /// Run a TCP/UDP throughput benchmark against a server.
    Benchmark(BenchArgs),
    /// Run the standalone bench server (paired with `benchmark` on another host).
    Server(ServerArgs),
    /// Press/retest: measure latency (p50, p99) at given concurrency.
    Press(PressArgs),
    /// Start a local HTTP/1, HTTP/2, HTTP/3 test server.
    ServeHttp(ServeHttpArgs),
}

// ---------------- shared transport flags ----------------

#[derive(Args, Clone)]
struct TransportArgs {
    /// SOCKS5 proxy URL: socks5://[user:pass@]host:port. Omit to test directly.
    #[arg(short = 'p', long, global = true)]
    proxy: Option<String>,

    /// Bind direct connections to this interface (name e.g. "en0", or local IP).
    /// Only valid when --proxy is not set.
    #[arg(short = 'i', long, global = true)]
    interface: Option<String>,

    /// DNS server (IP or IP:PORT, default port 53) used to resolve hostnames.
    /// The query is sent via the same transport: through --proxy when set, or
    /// bound to --interface when set; otherwise plain UDP. Omit to fall back
    /// to the system resolver (and to let the proxy resolve hostnames itself
    /// for `test`/`benchmark`).
    #[arg(long, global = true)]
    dns_server: Option<String>,
}

impl TransportArgs {
    fn build(&self) -> anyhow::Result<Transport> {
        match (&self.proxy, &self.interface) {
            (Some(_), Some(_)) => Err(anyhow::anyhow!(
                "--interface is only valid in direct mode (omit --proxy)"
            )),
            (Some(proxy), None) => Ok(Transport::Socks5(parse_proxy_url(proxy)?)),
            (None, iface) => {
                let interface: Option<InterfaceSpec> = iface.as_deref().map(parse_interface);
                Ok(Transport::Direct(DirectConfig::new(interface)))
            }
        }
    }

    fn build_dns(&self) -> anyhow::Result<Option<DnsConfig>> {
        match &self.dns_server {
            None => Ok(None),
            Some(s) => Ok(Some(DnsConfig::new(dns::parse_server(s)?))),
        }
    }
}

// ---------------- protocol test ----------------

#[derive(Args, Clone)]
struct TestArgs {
    #[command(flatten)]
    transport: TransportArgs,

    /// Target URL: http[s]://host[:port]/path
    #[arg(short = 't', long)]
    target: Option<String>,

    /// Protocol to test
    #[arg(short = 'P', long, default_value = "all")]
    protocol: ProtocolArg,

    /// Timeout per test in seconds
    #[arg(short = 'T', long, default_value = "30")]
    timeout: u64,

    /// Enable verbose logging
    #[arg(short = 'v', long)]
    verbose: bool,

    /// Resolve domain names locally instead of at the proxy
    #[arg(short = 'r', long)]
    resolve: bool,
}

#[derive(Clone, ValueEnum)]
enum ProtocolArg {
    Http1,
    Http2,
    Http3,
    All,
}

// ---------------- benchmark client ----------------

#[derive(Args, Clone)]
struct BenchArgs {
    #[command(flatten)]
    transport: TransportArgs,

    /// Target HOST:PORT of the bench server. Required unless --serve is set.
    #[arg(short = 't', long)]
    target: Option<String>,

    /// Spin up an in-process bench server on localhost (one-shot test mode).
    #[arg(long)]
    serve: bool,

    /// Optional fixed port for the in-process server (default: ephemeral).
    #[arg(long)]
    serve_port: Option<u16>,

    /// Which transport(s) to benchmark.
    #[arg(short = 'm', long, default_value = "both")]
    mode: BenchModeArg,

    /// Direction(s) to benchmark.
    #[arg(short = 'd', long, default_value = "both")]
    direction: BenchDirectionArg,

    /// Test duration per direction, in seconds.
    #[arg(short = 'D', long, default_value = "10")]
    duration: u64,

    /// UDP target bandwidth(s) — comma-separated, SI suffixes (K/M/G).
    #[arg(long, default_value = "10M")]
    udp_bandwidth: String,

    /// UDP datagram size in bytes (includes the 16-byte header).
    #[arg(long, default_value = "1200")]
    udp_size: usize,

    /// TCP write/read chunk size in bytes.
    #[arg(long, default_value = "65536")]
    tcp_chunk: usize,

    /// Enable verbose logging
    #[arg(short = 'v', long)]
    verbose: bool,
}

#[derive(Clone, ValueEnum)]
enum BenchModeArg {
    Tcp,
    Udp,
    Both,
}
impl From<BenchModeArg> for BenchMode {
    fn from(v: BenchModeArg) -> Self {
        match v {
            BenchModeArg::Tcp => BenchMode::Tcp,
            BenchModeArg::Udp => BenchMode::Udp,
            BenchModeArg::Both => BenchMode::Both,
        }
    }
}

#[derive(Clone, ValueEnum)]
enum BenchDirectionArg {
    Up,
    Down,
    Both,
}
impl From<BenchDirectionArg> for BenchDirection {
    fn from(v: BenchDirectionArg) -> Self {
        match v {
            BenchDirectionArg::Up => BenchDirection::Up,
            BenchDirectionArg::Down => BenchDirection::Down,
            BenchDirectionArg::Both => BenchDirection::Both,
        }
    }
}

// ---------------- bench server ----------------

#[derive(Args, Clone)]
struct ServerArgs {
    /// Address to bind on.
    #[arg(short = 'b', long, default_value = "0.0.0.0")]
    bind: IpAddr,

    /// TCP control port.
    #[arg(long, default_value = "5555")]
    port: u16,

    /// Enable verbose logging
    #[arg(short = 'v', long)]
    verbose: bool,
}

// ---------------- press / retest ----------------

#[derive(Args, Clone)]
struct PressArgs {
    #[command(flatten)]
    transport: TransportArgs,

    /// Target URL: http[s]://host[:port]/path
    #[arg(short = 't', long)]
    target: String,

    /// Protocol to press-test
    #[arg(short = 'P', long, default_value = "http1")]
    protocol: PressProtocolArg,

    /// Number of concurrent connections
    #[arg(short = 'c', long, default_value = "10")]
    concurrency: usize,

    /// Total number of requests to send
    #[arg(short = 'n', long, default_value = "100")]
    num_requests: usize,

    /// Timeout per request in seconds
    #[arg(short = 'T', long, default_value = "30")]
    timeout: u64,

    /// Skip TLS certificate verification (for local test servers)
    #[arg(long)]
    insecure: bool,

    /// Enable verbose logging
    #[arg(short = 'v', long)]
    verbose: bool,

    /// Resolve domain names locally instead of at the proxy
    #[arg(short = 'r', long)]
    resolve: bool,
}

#[derive(Clone, ValueEnum)]
enum PressProtocolArg {
    Http1,
    Http2,
    Http3,
}

impl From<PressProtocolArg> for PressProtocol {
    fn from(v: PressProtocolArg) -> Self {
        match v {
            PressProtocolArg::Http1 => PressProtocol::Http1,
            PressProtocolArg::Http2 => PressProtocol::Http2,
            PressProtocolArg::Http3 => PressProtocol::Http3,
        }
    }
}

// ---------------- HTTP test server ----------------

#[derive(Args, Clone)]
struct ServeHttpArgs {
    /// Address to bind on.
    #[arg(short = 'b', long, default_value = "127.0.0.1")]
    bind: IpAddr,

    /// Port for plain HTTP/1 server.
    #[arg(long, default_value = "8080")]
    http1_port: u16,

    /// Port for TLS HTTP/2 server.
    #[arg(long, default_value = "8443")]
    http2_port: u16,

    /// Port for QUIC HTTP/3 server.
    #[arg(long, default_value = "8444")]
    http3_port: u16,

    /// Response body size in bytes.
    #[arg(long, default_value = "128")]
    response_size: usize,
}

// ---------------- parsing helpers ----------------

fn parse_proxy_url(raw: &str) -> anyhow::Result<ProxyConfig> {
    let url_str = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("socks5://{raw}")
    };

    let url = Url::parse(&url_str)?;

    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("proxy URL missing host"))?;
    let port = url.port().unwrap_or(1080);
    let addr = format!("{host}:{port}");

    let auth = if !url.username().is_empty() {
        Some(ProxyAuth {
            username: url.username().to_string(),
            password: url.password().unwrap_or("").to_string(),
        })
    } else {
        None
    };

    Ok(ProxyConfig { addr, auth })
}

fn parse_target_url(raw: &str, insecure: bool) -> anyhow::Result<TestTarget> {
    let url = Url::parse(raw)?;

    let use_tls = url.scheme() == "https";
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("target URL missing host"))?
        .to_string();
    let port = url.port().unwrap_or(if use_tls { 443 } else { 80 });
    let path = url.path().to_string();

    Ok(TestTarget {
        host,
        port,
        path,
        use_tls,
        resolved_addr: None,
        danger_accept_invalid_certs: insecure,
    })
}

async fn parse_bench_target(
    raw: &str,
    dns: Option<&DnsConfig>,
    transport: &Transport,
) -> anyhow::Result<SocketAddr> {
    if let Ok(addr) = raw.parse::<SocketAddr>() {
        return Ok(addr);
    }
    if let Some(dns) = dns {
        let (host, port) = split_host_port(raw)?;
        let ip = dns::resolve_a(transport, dns, host).await?;
        return Ok(SocketAddr::from((ip, port)));
    }
    tokio::net::lookup_host(raw)
        .await?
        .next()
        .ok_or_else(|| anyhow::anyhow!("could not resolve {raw}"))
}

fn split_host_port(raw: &str) -> anyhow::Result<(&str, u16)> {
    let (host, port) = raw
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("expected HOST:PORT, got '{raw}'"))?;
    let port: u16 = port
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid port in '{raw}'"))?;
    Ok((host, port))
}

fn install_logging(verbose: bool) {
    let filter = if verbose { "debug" } else { "warn" };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

// ---------------- command dispatch ----------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let cli = Cli::parse();

    let cmd = cli.command.unwrap_or(Command::Test(cli.test_args));
    match cmd {
        Command::Test(args) => run_test(args).await,
        Command::Benchmark(args) => run_benchmark(args).await,
        Command::Server(args) => run_bench_server(args).await,
        Command::Press(args) => run_press(args).await,
        Command::ServeHttp(args) => run_serve_http(args).await,
    }
}

async fn run_test(args: TestArgs) -> anyhow::Result<()> {
    install_logging(args.verbose);
    let transport = args.transport.build()?;
    let dns_cfg = args.transport.build_dns()?;
    let target_url = args
        .target
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("--target is required"))?;
    let mut target = parse_target_url(target_url, false)?;
    let timeout = Duration::from_secs(args.timeout);

    if let Some(dns) = &dns_cfg {
        // --dns-server implies local resolution through the configured
        // transport. Skip the system resolver path.
        if target.host.parse::<IpAddr>().is_err() {
            let ip = dns::resolve_a(&transport, dns, &target.host).await?;
            target.resolved_addr = Some(IpAddr::V4(ip));
        }
    } else if args.resolve {
        target.resolve_local().await?;
    }

    let pen = ProxyPen::new(transport);

    println!("{}", header_line(&args, &target));
    println!();

    let results = match args.protocol {
        ProtocolArg::Http1 => vec![pen.test_http1(&target, timeout).await],
        ProtocolArg::Http2 => vec![pen.test_http2(&target, timeout).await],
        ProtocolArg::Http3 => vec![pen.test_http3(&target, timeout).await],
        ProtocolArg::All => pen.test_all(&target, timeout).await,
    };

    for result in &results {
        println!("{result}");
    }

    let all_success = results
        .iter()
        .all(|r| matches!(r.status, proxypen::TestStatus::Success));
    if !all_success {
        std::process::exit(1);
    }

    Ok(())
}

fn header_line(args: &TestArgs, target: &TestTarget) -> String {
    match &args.transport.proxy {
        Some(proxy) => format!(
            "Testing proxy {} -> {}:{}",
            proxy, target.host, target.port
        ),
        None => {
            let mut line = format!("Testing direct -> {}:{}", target.host, target.port);
            if let Some(iface) = &args.transport.interface {
                line.push_str(&format!(" (interface: {iface})"));
            }
            line
        }
    }
}

async fn run_benchmark(args: BenchArgs) -> anyhow::Result<()> {
    install_logging(args.verbose);

    let transport = args.transport.build()?;
    let dns_cfg = args.transport.build_dns()?;
    let bandwidths = parse_bandwidth_list(&args.udp_bandwidth)?;

    let target = match &args.target {
        Some(t) => Some(parse_bench_target(t, dns_cfg.as_ref(), &transport).await?),
        None => None,
    };

    let opts = BenchOptions {
        transport,
        target,
        serve: args.serve,
        serve_port: args.serve_port,
        mode: args.mode.into(),
        direction: args.direction.into(),
        duration: Duration::from_secs(args.duration),
        udp_bandwidth: bandwidths,
        udp_size: args.udp_size,
        tcp_chunk: args.tcp_chunk,
    };
    let _outcomes = run_client(opts).await?;
    Ok(())
}

async fn run_bench_server(args: ServerArgs) -> anyhow::Result<()> {
    install_logging(args.verbose);
    run_server(args.bind, args.port).await
}

// ---------------- press ----------------

async fn run_press(args: PressArgs) -> anyhow::Result<()> {
    install_logging(args.verbose);
    let transport = args.transport.build()?;
    let dns_cfg = args.transport.build_dns()?;
    let mut target = parse_target_url(&args.target, args.insecure)?;
    let timeout = Duration::from_secs(args.timeout);

    if let Some(dns) = &dns_cfg {
        if target.host.parse::<IpAddr>().is_err() {
            let ip = dns::resolve_a(&transport, dns, &target.host).await?;
            target.resolved_addr = Some(IpAddr::V4(ip));
        }
    } else if args.resolve {
        target.resolve_local().await?;
    }

    let protocol: PressProtocol = args.protocol.into();

    let opts = PressOptions {
        transport,
        target,
        protocol,
        concurrency: args.concurrency,
        num_requests: args.num_requests,
        timeout,
    };

    let result = press(&opts).await;
    println!("{result}");

    if result.error_count > 0 && result.success_count == 0 {
        std::process::exit(1);
    }

    Ok(())
}

// ---------------- HTTP test server ----------------

async fn run_serve_http(args: ServeHttpArgs) -> anyhow::Result<()> {
    let config = HttpServerConfig {
        bind: args.bind,
        http1_port: args.http1_port,
        http2_port: args.http2_port,
        http3_port: args.http3_port,
        response_size: args.response_size,
    };
    run_http_server(config).await
}
