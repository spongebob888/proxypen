use tokio::net::TcpStream;

use crate::config::{ProxyConfig, TestTarget};
use crate::direct::{self, DirectConfig};
use crate::error::Result;
use crate::socks::connector;

#[derive(Debug, Clone)]
pub enum Transport {
    Socks5(ProxyConfig),
    Direct(DirectConfig),
}

impl Transport {
    /// Open a TCP connection to `target` via this transport.
    pub async fn connect_tcp(&self, target: &TestTarget) -> Result<TcpStream> {
        match self {
            Transport::Socks5(cfg) => connector::connect(cfg, target).await,
            Transport::Direct(cfg) => direct::connect_tcp(target, cfg.interface.as_ref()).await,
        }
    }
}
