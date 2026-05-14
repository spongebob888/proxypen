use std::fmt;
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpPoller};
use shadowquic::msgs::socks5::{AddrOrDomain, SocksAddr, VarVec};
use tokio::net::UdpSocket;

/// A UDP socket wrapper that transparently adds/strips SOCKS5 UDP headers.
/// Implements quinn's `AsyncUdpSocket` trait so it can be used with a quinn Endpoint
/// to send QUIC traffic through a SOCKS5 UDP relay.
pub struct SocksUdpSocket {
    inner: UdpSocket,
    target: SocksAddr,
    /// The SocketAddr that quinn will use for connect(). We always report this
    /// as the source in RecvMeta so quinn can route packets to the correct connection.
    remote_addr: SocketAddr,
}

impl SocksUdpSocket {
    /// Create a new SocksUdpSocket.
    /// `inner` should already be connected to the SOCKS5 relay endpoint.
    /// `target` is the ultimate destination for QUIC packets (the HTTP/3 server).
    /// `remote_addr` is the address quinn will use in connect_with() — must match.
    pub fn new(inner: UdpSocket, target: SocksAddr, remote_addr: SocketAddr) -> Self {
        Self {
            inner,
            target,
            remote_addr,
        }
    }
}

impl fmt::Debug for SocksUdpSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SocksUdpSocket")
            .field("local_addr", &self.inner.local_addr())
            .field("target", &self.target.to_string())
            .finish()
    }
}

impl AsyncUdpSocket for SocksUdpSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(SocksUdpPoller {
            socket: self.clone(),
        })
    }

    fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
        let header = encode_socks5_udp_header(&self.target);
        let mut buf = Vec::with_capacity(header.len() + transmit.contents.len());
        buf.extend_from_slice(&header);
        buf.extend_from_slice(transmit.contents);
        self.inner.try_send(&buf).map(|_| ())
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let mut recv_buf = [0u8; 65535];
        let mut read_buf = tokio::io::ReadBuf::new(&mut recv_buf);

        match self.inner.poll_recv(cx, &mut read_buf) {
            Poll::Ready(Ok(())) => {
                let filled = read_buf.filled();
                if filled.is_empty() {
                    return Poll::Ready(Ok(0));
                }

                // Strip SOCKS5 UDP header to get the QUIC payload
                let (_addr, header_len) = match decode_socks5_udp_header(filled) {
                    Ok(v) => v,
                    Err(e) => return Poll::Ready(Err(e)),
                };
                let payload = &filled[header_len..];
                if payload.is_empty() {
                    return Poll::Ready(Ok(0));
                }

                let copy_len = payload.len().min(bufs[0].len());
                bufs[0][..copy_len].copy_from_slice(&payload[..copy_len]);
                meta[0] = RecvMeta {
                    addr: self.remote_addr,
                    dst_ip: None,
                    ecn: None,
                    len: copy_len,
                    stride: copy_len,
                };

                Poll::Ready(Ok(1))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn may_fragment(&self) -> bool {
        false
    }

    fn max_transmit_segments(&self) -> usize {
        1
    }

    fn max_receive_segments(&self) -> usize {
        1
    }
}

#[derive(Debug)]
struct SocksUdpPoller {
    socket: Arc<SocksUdpSocket>,
}

impl UdpPoller for SocksUdpPoller {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.socket.inner.poll_send_ready(cx)
    }
}

/// Encode a SOCKS5 UDP request header (synchronous, for the hot path).
/// Format: RSV (2 bytes, 0x0000) + FRAG (1 byte, 0x00) + ATYP + ADDR + PORT
pub fn encode_socks5_udp_header(dst: &SocksAddr) -> Vec<u8> {
    let mut buf = Vec::with_capacity(32);
    // RSV
    buf.push(0x00);
    buf.push(0x00);
    // FRAG
    buf.push(0x00);
    // Address
    match &dst.addr {
        AddrOrDomain::V4(ip) => {
            buf.push(0x01); // ATYP IPv4
            buf.extend_from_slice(ip);
        }
        AddrOrDomain::V6(ip) => {
            buf.push(0x04); // ATYP IPv6
            buf.extend_from_slice(ip);
        }
        AddrOrDomain::Domain(domain) => {
            buf.push(0x03); // ATYP Domain
            buf.push(domain.len);
            buf.extend_from_slice(&domain.contents[..domain.len as usize]);
        }
    }
    // Port (big-endian)
    buf.push((dst.port >> 8) as u8);
    buf.push((dst.port & 0xff) as u8);
    buf
}

/// Decode a SOCKS5 UDP request header from a buffer.
/// Returns (source SocksAddr, header length in bytes).
pub fn decode_socks5_udp_header(buf: &[u8]) -> io::Result<(SocksAddr, usize)> {
    if buf.len() < 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "UDP header too short",
        ));
    }

    // RSV (2 bytes) + FRAG (1 byte)
    let frag = buf[2];
    if frag != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "fragmented UDP not supported",
        ));
    }

    let atyp = buf[3];
    let (addr, offset) = match atyp {
        0x01 => {
            // IPv4: 4 bytes
            if buf.len() < 4 + 4 + 2 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "IPv4 addr too short",
                ));
            }
            let mut ip = [0u8; 4];
            ip.copy_from_slice(&buf[4..8]);
            (AddrOrDomain::V4(ip), 8)
        }
        0x04 => {
            // IPv6: 16 bytes
            if buf.len() < 4 + 16 + 2 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "IPv6 addr too short",
                ));
            }
            let mut ip = [0u8; 16];
            ip.copy_from_slice(&buf[4..20]);
            (AddrOrDomain::V6(ip), 20)
        }
        0x03 => {
            // Domain: 1 byte len + N bytes
            if buf.len() < 5 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "domain header too short",
                ));
            }
            let domain_len = buf[4] as usize;
            if buf.len() < 5 + domain_len + 2 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "domain addr too short",
                ));
            }
            let domain = VarVec {
                len: domain_len as u8,
                contents: buf[5..5 + domain_len].to_vec(),
            };
            (AddrOrDomain::Domain(domain), 5 + domain_len)
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown address type: {atyp:#x}"),
            ));
        }
    };

    // Port (2 bytes, big-endian)
    let port = u16::from_be_bytes([buf[offset], buf[offset + 1]]);
    let total_header_len = offset + 2;

    Ok((SocksAddr { addr, port }, total_header_len))
}
