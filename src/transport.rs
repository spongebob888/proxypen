use std::io;
use std::net::SocketAddr;

use shadowquic::msgs::socks5::SocksAddr;
use tokio::net::{TcpStream, UdpSocket};

use crate::config::{ProxyConfig, TestTarget};
use crate::direct::{self, DirectConfig};
use crate::error::Result;
use crate::socks::{associate, connector};
use crate::udp_socket::{decode_socks5_udp_header, encode_socks5_udp_header};

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

    /// Open a connection-oriented UDP socket whose `send`/`recv` operate on
    /// raw payloads, hiding any SOCKS5 UDP framing.
    pub async fn open_udp(&self, target: SocketAddr) -> Result<TransportUdpSocket> {
        match self {
            Transport::Direct(cfg) => {
                let std_socket = direct::build_udp_socket(target.is_ipv6(), cfg.interface.as_ref())?;
                let socket = UdpSocket::from_std(std_socket)?;
                socket.connect(target).await?;
                Ok(TransportUdpSocket::Direct { socket })
            }
            Transport::Socks5(cfg) => {
                let assoc = associate::associate(cfg).await?;
                Ok(TransportUdpSocket::Socks5 {
                    socket: assoc.socket,
                    target: SocksAddr::from(target),
                    _control: assoc.control,
                })
            }
        }
    }

    pub fn is_direct(&self) -> bool {
        matches!(self, Transport::Direct(_))
    }
}

/// A UDP socket that targets a fixed destination, hiding SOCKS5 UDP framing
/// from the caller. The same wire format is reused for the bench data plane
/// and for DNS queries.
pub enum TransportUdpSocket {
    Direct {
        socket: UdpSocket,
    },
    Socks5 {
        socket: UdpSocket,
        target: SocksAddr,
        // Keeps the TCP control connection alive for the duration of the UDP
        // association.
        _control: TcpStream,
    },
}

impl TransportUdpSocket {
    pub async fn send(&self, payload: &[u8]) -> io::Result<()> {
        match self {
            Self::Direct { socket } => {
                socket.send(payload).await?;
                Ok(())
            }
            Self::Socks5 { socket, target, .. } => {
                let header = encode_socks5_udp_header(target);
                let mut buf = Vec::with_capacity(header.len() + payload.len());
                buf.extend_from_slice(&header);
                buf.extend_from_slice(payload);
                socket.send(&buf).await?;
                Ok(())
            }
        }
    }

    /// Receive one datagram payload. Returns the number of payload bytes
    /// written into `buf` (the SOCKS5 header, if any, is stripped).
    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Direct { socket } => socket.recv(buf).await,
            Self::Socks5 { socket, .. } => {
                let mut tmp = vec![0u8; buf.len() + 256];
                let n = socket.recv(&mut tmp).await?;
                let (_addr, hlen) = decode_socks5_udp_header(&tmp[..n])?;
                let payload = &tmp[hlen..n];
                let copy = payload.len().min(buf.len());
                buf[..copy].copy_from_slice(&payload[..copy]);
                Ok(copy)
            }
        }
    }
}
