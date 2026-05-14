use std::net::{SocketAddr, ToSocketAddrs};

use shadowquic::msgs::socks5::SocksAddr;
use tokio::net::{TcpStream, UdpSocket};

use crate::config::ProxyConfig;
use crate::error::{ProxyPenError, Result};
use crate::socks::handshake;

/// Holds the state of a SOCKS5 UDP association.
pub struct UdpAssociation {
    /// Local UDP socket connected to the proxy's relay endpoint.
    pub socket: UdpSocket,
    /// The proxy's UDP relay address.
    pub relay_addr: SocketAddr,
    /// TCP control stream — must stay alive for the UDP association to remain valid.
    pub control: TcpStream,
}

/// Establish a SOCKS5 UDP ASSOCIATE session.
/// Returns a UdpAssociation with a socket connected to the relay.
pub async fn associate(config: &ProxyConfig) -> Result<UdpAssociation> {
    let mut stream = TcpStream::connect(&config.addr).await?;
    stream.set_nodelay(true)?;

    handshake::authenticate(&mut stream, config.auth.as_ref()).await?;

    // Use 0.0.0.0:0 as bind hint (we don't know our addr yet)
    let bind_hint = SocksAddr::from(SocketAddr::from(([0, 0, 0, 0], 0u16)));
    let reply = handshake::send_udp_associate(&mut stream, bind_hint).await?;

    // Resolve the relay address from the reply
    let mut relay_addr = reply
        .bind_addr
        .to_socket_addrs()
        .map_err(|e| ProxyPenError::Socks(format!("cannot resolve relay addr: {e}")))?
        .next()
        .ok_or_else(|| ProxyPenError::Socks("relay addr resolved to nothing".into()))?;

    // Some proxies return 0.0.0.0:port — substitute with the proxy's IP
    if relay_addr.ip().is_unspecified() {
        let proxy_addr: SocketAddr = config
            .addr
            .parse()
            .or_else(|_| {
                config
                    .addr
                    .to_socket_addrs()
                    .map(|mut it| it.next().unwrap())
            })
            .map_err(|e| ProxyPenError::Socks(format!("cannot parse proxy addr: {e}")))?;
        relay_addr.set_ip(proxy_addr.ip());
    }

    // Bind local UDP socket and connect to relay
    let bind_addr = if relay_addr.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind_addr).await?;
    socket.connect(relay_addr).await?;

    Ok(UdpAssociation {
        socket,
        relay_addr,
        control: stream,
    })
}
