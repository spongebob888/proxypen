use std::time::Duration;

use clap::{Parser, ValueEnum};
use proxypen::{ProxyAuth, ProxyConfig, ProxyPen, TestTarget};
use url::Url;

#[derive(Parser)]
#[command(name = "proxypen", about = "Test SOCKS5 proxy with HTTP/1, HTTP/2, HTTP/3")]
struct Cli {
    /// SOCKS5 proxy URL: socks5://[user:pass@]host:port
    #[arg(short = 'p', long)]
    proxy: String,

    /// Target URL: http[s]://host[:port]/path
    #[arg(short = 't', long)]
    target: String,

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

fn parse_proxy_url(raw: &str) -> anyhow::Result<ProxyConfig> {
    // Normalize: if no scheme, add socks5://
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

fn parse_target_url(raw: &str) -> anyhow::Result<TestTarget> {
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
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Install ring as the default crypto provider (needed when both ring and aws-lc-rs features are enabled)
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let cli = Cli::parse();

    // Setup logging
    let filter = if cli.verbose { "debug" } else { "warn" };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    let config = parse_proxy_url(&cli.proxy)?;
    let mut target = parse_target_url(&cli.target)?;
    let timeout = Duration::from_secs(cli.timeout);

    if cli.resolve {
        target.resolve_local().await?;
    }

    let pen = ProxyPen::new(config);

    println!(
        "Testing proxy {} -> {}:{}",
        cli.proxy, target.host, target.port
    );
    println!();

    let results = match cli.protocol {
        ProtocolArg::Http1 => vec![pen.test_http1(&target, timeout).await],
        ProtocolArg::Http2 => vec![pen.test_http2(&target, timeout).await],
        ProtocolArg::Http3 => vec![pen.test_http3(&target, timeout).await],
        ProtocolArg::All => pen.test_all(&target, timeout).await,
    };

    for result in &results {
        println!("{result}");
    }

    // Exit with error code if any test failed
    let all_success = results
        .iter()
        .all(|r| matches!(r.status, proxypen::TestStatus::Success));
    if !all_success {
        std::process::exit(1);
    }

    Ok(())
}
