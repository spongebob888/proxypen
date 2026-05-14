use tokio::net::TcpStream;

use crate::config::{ProxyConfig, TestTarget};
use crate::error::Result;
use crate::socks::handshake;

/// Establish a SOCKS5 TCP CONNECT tunnel and return the connected stream.
/// The returned TcpStream is tunneled through the proxy to the target.
pub async fn connect(config: &ProxyConfig, target: &TestTarget) -> Result<TcpStream> {
    let mut stream = TcpStream::connect(&config.addr).await?;
    stream.set_nodelay(true)?;

    handshake::authenticate(&mut stream, config.auth.as_ref()).await?;

    let dst = target.to_socks_addr();
    let _reply = handshake::send_connect(&mut stream, dst).await?;

    Ok(stream)
}
