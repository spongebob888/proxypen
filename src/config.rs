use std::net::{IpAddr, SocketAddr};

use shadowquic::msgs::socks5::SocksAddr;

#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub addr: String,
    pub auth: Option<ProxyAuth>,
}

#[derive(Debug, Clone)]
pub struct ProxyAuth {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone)]
pub struct TestTarget {
    pub host: String,
    pub port: u16,
    pub path: String,
    pub use_tls: bool,
    /// Locally resolved IP address (when --resolve is used).
    pub resolved_addr: Option<IpAddr>,
}

impl TestTarget {
    /// Resolve the host locally using system DNS.
    pub async fn resolve_local(&mut self) -> std::io::Result<()> {
        if self.host.parse::<IpAddr>().is_ok() {
            return Ok(());
        }
        let addr = tokio::net::lookup_host(format!("{}:{}", self.host, self.port))
            .await?
            .next()
            .map(|sa| sa.ip());
        self.resolved_addr = addr;
        Ok(())
    }

    pub fn to_socks_addr(&self) -> SocksAddr {
        if let Some(ip) = self.resolved_addr {
            return SocksAddr::from(SocketAddr::new(ip, self.port));
        }
        if let Ok(ip) = self.host.parse::<IpAddr>() {
            SocksAddr::from(SocketAddr::new(ip, self.port))
        } else {
            SocksAddr::from_domain(self.host.clone(), self.port)
        }
    }

    pub fn authority(&self) -> String {
        if self.port == 80 || self.port == 443 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}
