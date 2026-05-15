use std::io;
use std::net::SocketAddr;

use shadowquic::msgs::socks5::SocksAddr;
use tokio::net::{TcpStream, UdpSocket};

use crate::config::ProxyConfig;
use crate::direct::{self, InterfaceSpec};
use crate::error::Result;
use crate::socks::associate;
use crate::transport::Transport;
use crate::udp_socket::{decode_socks5_udp_header, encode_socks5_udp_header};

/// A connection-oriented UDP socket that hides the SOCKS5 UDP header from
/// callers. `send` always targets the original `target` SocksAddr; `recv`
/// returns just the payload.
pub enum BenchUdpSocket {
    Direct {
        socket: UdpSocket,
    },
    Socks5 {
        socket: UdpSocket,
        target: SocksAddr,
        // Holds the TCP control connection that keeps the UDP association alive.
        _control: TcpStream,
    },
}

impl BenchUdpSocket {
    pub async fn open(transport: &Transport, target: SocketAddr) -> Result<Self> {
        match transport {
            Transport::Direct(cfg) => {
                Self::open_direct(target, cfg.interface.as_ref()).await
            }
            Transport::Socks5(cfg) => Self::open_socks5(cfg, target).await,
        }
    }

    async fn open_direct(target: SocketAddr, iface: Option<&InterfaceSpec>) -> Result<Self> {
        let std_socket = direct::build_udp_socket(target.is_ipv6(), iface)?;
        let socket = UdpSocket::from_std(std_socket)?;
        socket.connect(target).await?;
        Ok(Self::Direct { socket })
    }

    async fn open_socks5(config: &ProxyConfig, target: SocketAddr) -> Result<Self> {
        let assoc = associate::associate(config).await?;
        Ok(Self::Socks5 {
            socket: assoc.socket,
            target: SocksAddr::from(target),
            _control: assoc.control,
        })
    }

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

    /// Receive a single datagram payload. Returns the number of payload bytes
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
