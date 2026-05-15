use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::TcpStream;

use crate::config::TestTarget;
use crate::error::{ProxyPenError, Result};

#[derive(Debug, Clone)]
pub enum InterfaceSpec {
    Name(String),
    Address(IpAddr),
}

impl InterfaceSpec {
    pub fn describe(&self) -> String {
        match self {
            InterfaceSpec::Name(n) => n.clone(),
            InterfaceSpec::Address(a) => a.to_string(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DirectConfig {
    pub interface: Option<InterfaceSpec>,
}

impl DirectConfig {
    pub fn new(interface: Option<InterfaceSpec>) -> Self {
        Self { interface }
    }
}

/// Parse the user-supplied --interface value. Tries an IP address first,
/// otherwise treats it as an interface name (e.g. "en0").
pub fn parse_interface(s: &str) -> InterfaceSpec {
    if let Ok(ip) = IpAddr::from_str(s) {
        InterfaceSpec::Address(ip)
    } else {
        InterfaceSpec::Name(s.to_string())
    }
}

/// Resolve a TestTarget to a concrete SocketAddr (used by direct mode).
pub async fn resolve_target(target: &TestTarget) -> Result<SocketAddr> {
    if let Some(ip) = target.resolved_addr {
        return Ok(SocketAddr::new(ip, target.port));
    }
    if let Ok(ip) = target.host.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, target.port));
    }
    tokio::net::lookup_host(format!("{}:{}", target.host, target.port))
        .await?
        .next()
        .ok_or_else(|| {
            ProxyPenError::InvalidConfig(format!("DNS lookup returned no addresses for {}", target.host))
        })
}

#[cfg(unix)]
fn apply_interface(socket: &Socket, iface: &InterfaceSpec, ipv6: bool) -> Result<()> {
    match iface {
        InterfaceSpec::Address(ip) => {
            socket
                .bind(&SocketAddr::new(*ip, 0).into())
                .map_err(|e| ProxyPenError::InvalidConfig(format!("bind to source IP {ip}: {e}")))?;
        }
        InterfaceSpec::Name(name) => bind_by_name(socket, name, ipv6)?,
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_interface(_socket: &Socket, _iface: &InterfaceSpec, _ipv6: bool) -> Result<()> {
    Err(ProxyPenError::InvalidConfig(
        "--interface is not supported on this platform".into(),
    ))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn bind_by_name(socket: &Socket, name: &str, ipv6: bool) -> Result<()> {
    use std::ffi::CString;
    use std::num::NonZeroU32;

    let cname = CString::new(name)
        .map_err(|_| ProxyPenError::InvalidConfig(format!("invalid interface name: {name}")))?;
    let idx = unsafe { libc::if_nametoindex(cname.as_ptr()) };
    if idx == 0 {
        return Err(ProxyPenError::InvalidConfig(format!(
            "unknown interface: {name}"
        )));
    }
    let nz = NonZeroU32::new(idx).expect("if_nametoindex returned nonzero");
    let res = if ipv6 {
        socket.bind_device_by_index_v6(Some(nz))
    } else {
        socket.bind_device_by_index_v4(Some(nz))
    };
    res.map_err(|e| ProxyPenError::InvalidConfig(format!("bind to interface {name}: {e}")))
}

#[cfg(target_os = "linux")]
fn bind_by_name(socket: &Socket, name: &str, _ipv6: bool) -> Result<()> {
    socket.bind_device(Some(name.as_bytes())).map_err(|e| {
        ProxyPenError::InvalidConfig(format!(
            "bind to interface {name}: {e} (SO_BINDTODEVICE typically requires CAP_NET_RAW or root)"
        ))
    })
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
#[cfg(unix)]
fn bind_by_name(_socket: &Socket, _name: &str, _ipv6: bool) -> Result<()> {
    Err(ProxyPenError::InvalidConfig(
        "interface name binding not supported on this platform; specify a local IP instead".into(),
    ))
}

/// Open a TCP connection directly to the target, optionally bound to a
/// specific interface (by name) or source IP.
pub async fn connect_tcp(
    target: &TestTarget,
    iface: Option<&InterfaceSpec>,
) -> Result<TcpStream> {
    let addr = resolve_target(target).await?;

    if iface.is_none() {
        let stream = TcpStream::connect(addr).await?;
        stream.set_nodelay(true)?;
        return Ok(stream);
    }

    #[cfg(not(unix))]
    {
        let _ = addr;
        return Err(ProxyPenError::InvalidConfig(
            "--interface requires a unix-like OS".into(),
        ));
    }

    #[cfg(unix)]
    {
        use std::os::fd::{FromRawFd, IntoRawFd};

        let domain = if addr.is_ipv6() { Domain::IPV6 } else { Domain::IPV4 };
        let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
        apply_interface(&socket, iface.unwrap(), addr.is_ipv6())?;
        socket.set_nonblocking(true)?;

        let tcp_socket: tokio::net::TcpSocket =
            unsafe { tokio::net::TcpSocket::from_raw_fd(socket.into_raw_fd()) };
        let stream = tcp_socket.connect(addr).await?;
        stream.set_nodelay(true)?;
        Ok(stream)
    }
}

/// Build a std UDP socket suitable for use with `quinn::Endpoint::new`.
/// The socket is non-blocking and bound to a wildcard local port (or to
/// the supplied source IP). When `iface` is an interface name, the OS-level
/// `IP_BOUND_IF` / `SO_BINDTODEVICE` option is applied before bind.
pub fn build_udp_socket(
    target_is_v6: bool,
    iface: Option<&InterfaceSpec>,
) -> Result<std::net::UdpSocket> {
    let domain = if target_is_v6 { Domain::IPV6 } else { Domain::IPV4 };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;

    match iface {
        Some(InterfaceSpec::Address(ip)) => {
            socket
                .bind(&SocketAddr::new(*ip, 0).into())
                .map_err(|e| {
                    ProxyPenError::InvalidConfig(format!("bind UDP to source IP {ip}: {e}"))
                })?;
        }
        Some(InterfaceSpec::Name(name)) => {
            #[cfg(unix)]
            {
                bind_by_name(&socket, name, target_is_v6)?;
            }
            #[cfg(not(unix))]
            {
                let _ = name;
                return Err(ProxyPenError::InvalidConfig(
                    "--interface is not supported on this platform".into(),
                ));
            }
            let any = wildcard_addr(target_is_v6);
            socket.bind(&any.into())?;
        }
        None => {
            let any = wildcard_addr(target_is_v6);
            socket.bind(&any.into())?;
        }
    }

    socket.set_nonblocking(true)?;
    Ok(socket.into())
}

fn wildcard_addr(ipv6: bool) -> SocketAddr {
    if ipv6 {
        "[::]:0".parse().unwrap()
    } else {
        "0.0.0.0:0".parse().unwrap()
    }
}
