use std::net::{IpAddr, SocketAddr};

use shadowquic::msgs::socks5::SocksAddr;

use crate::dns::DnsFamily;

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
    /// Skip TLS certificate verification (for testing with self-signed certs).
    pub danger_accept_invalid_certs: bool,
}

impl TestTarget {
    /// Resolve the host locally using system DNS.
    pub async fn resolve_local(&mut self) -> std::io::Result<()> {
        self.resolve_local_with_family(None).await
    }

    /// Resolve the host locally using system DNS, optionally filtering by IP family.
    pub async fn resolve_local_with_family(
        &mut self,
        family: Option<DnsFamily>,
    ) -> std::io::Result<()> {
        if self.host.parse::<IpAddr>().is_ok() {
            return Ok(());
        }
        let addr = tokio::net::lookup_host(format!("{}:{}", self.host, self.port))
            .await?
            .find(|sa| match family {
                Some(DnsFamily::Ipv4) => sa.is_ipv4(),
                Some(DnsFamily::Ipv6) => sa.is_ipv6(),
                None => true,
            })
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
