//! Minimal DNS resolver that issues a single A query over the supplied
//! [`Transport`]. The query inherits the transport's routing — bound to
//! `--interface` in direct mode, or relayed through SOCKS5 UDP ASSOCIATE in
//! proxy mode.
//!
//! Only A records are returned. AAAA is out of scope for now.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use crate::error::{ProxyPenError, Result};
use crate::transport::Transport;

pub const DEFAULT_DNS_PORT: u16 = 53;
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_LABEL_LEN: usize = 63;
const MAX_NAME_LEN: usize = 255;

#[derive(Debug, Clone, Copy)]
pub struct DnsConfig {
    pub server: SocketAddr,
}

impl DnsConfig {
    pub fn new(server: SocketAddr) -> Self {
        Self { server }
    }
}

/// Parse `host[:port]` (or a bare IP) into a SocketAddr suitable for use as
/// a DNS server address. Default port is 53.
pub fn parse_server(raw: &str) -> Result<SocketAddr> {
    if let Ok(sa) = raw.parse::<SocketAddr>() {
        return Ok(sa);
    }
    if let Ok(ip) = raw.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, DEFAULT_DNS_PORT));
    }
    Err(ProxyPenError::InvalidConfig(format!(
        "DNS server must be an IP or IP:PORT, got '{raw}'"
    )))
}

/// Resolve `hostname` to an IPv4 address by querying `dns.server` via the
/// supplied transport.
pub async fn resolve_a(
    transport: &Transport,
    dns: &DnsConfig,
    hostname: &str,
) -> Result<Ipv4Addr> {
    if hostname.len() > MAX_NAME_LEN {
        return Err(ProxyPenError::InvalidConfig(format!(
            "DNS hostname too long: {hostname}"
        )));
    }

    let socket = transport.open_udp(dns.server).await?;
    let id = random_id();
    let query = encode_query(id, hostname)?;
    socket.send(&query).await?;

    let mut buf = vec![0u8; 1500];
    let n = tokio::time::timeout(QUERY_TIMEOUT, socket.recv(&mut buf))
        .await
        .map_err(|_| {
            ProxyPenError::InvalidConfig(format!("DNS query to {} timed out", dns.server))
        })??;

    decode_a_response(id, &buf[..n])
}

fn random_id() -> u16 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (n & 0xFFFF) as u16
}

fn encode_query(id: u16, hostname: &str) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(64);
    // Header: ID, flags, QDCOUNT=1, others=0
    buf.extend_from_slice(&id.to_be_bytes());
    buf.extend_from_slice(&0x0100u16.to_be_bytes()); // RD = 1, standard query
    buf.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    buf.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    buf.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    buf.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT

    for label in hostname.split('.') {
        if label.is_empty() {
            continue;
        }
        let bytes = label.as_bytes();
        if bytes.len() > MAX_LABEL_LEN {
            return Err(ProxyPenError::InvalidConfig(format!(
                "DNS label too long: {label}"
            )));
        }
        buf.push(bytes.len() as u8);
        buf.extend_from_slice(bytes);
    }
    buf.push(0); // root
    buf.extend_from_slice(&1u16.to_be_bytes()); // QTYPE = A
    buf.extend_from_slice(&1u16.to_be_bytes()); // QCLASS = IN
    Ok(buf)
}

pub fn decode_a_response(expected_id: u16, buf: &[u8]) -> Result<Ipv4Addr> {
    if buf.len() < 12 {
        return Err(ProxyPenError::InvalidConfig(
            "DNS response too short".into(),
        ));
    }
    let id = u16::from_be_bytes([buf[0], buf[1]]);
    if id != expected_id {
        return Err(ProxyPenError::InvalidConfig(format!(
            "DNS response ID mismatch (got {id}, expected {expected_id})"
        )));
    }
    let flags = u16::from_be_bytes([buf[2], buf[3]]);
    let rcode = flags & 0x000F;
    if rcode != 0 {
        return Err(ProxyPenError::InvalidConfig(format!(
            "DNS server returned RCODE {rcode}"
        )));
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    if an == 0 {
        return Err(ProxyPenError::InvalidConfig(
            "DNS response contains no answers".into(),
        ));
    }

    let mut pos = 12;
    for _ in 0..qd {
        pos = skip_name(buf, pos)?;
        if pos + 4 > buf.len() {
            return Err(ProxyPenError::InvalidConfig(
                "DNS question truncated".into(),
            ));
        }
        pos += 4; // QTYPE + QCLASS
    }

    for _ in 0..an {
        pos = skip_name(buf, pos)?;
        if pos + 10 > buf.len() {
            return Err(ProxyPenError::InvalidConfig(
                "DNS answer header truncated".into(),
            ));
        }
        let rtype = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let rdlength = u16::from_be_bytes([buf[pos + 8], buf[pos + 9]]) as usize;
        pos += 10;
        if pos + rdlength > buf.len() {
            return Err(ProxyPenError::InvalidConfig(
                "DNS rdata truncated".into(),
            ));
        }
        if rtype == 1 && rdlength == 4 {
            return Ok(Ipv4Addr::new(
                buf[pos],
                buf[pos + 1],
                buf[pos + 2],
                buf[pos + 3],
            ));
        }
        pos += rdlength;
    }
    Err(ProxyPenError::InvalidConfig(
        "DNS response had no A record".into(),
    ))
}

fn skip_name(buf: &[u8], mut pos: usize) -> Result<usize> {
    loop {
        if pos >= buf.len() {
            return Err(ProxyPenError::InvalidConfig(
                "DNS name truncated".into(),
            ));
        }
        let len = buf[pos];
        if len == 0 {
            return Ok(pos + 1);
        }
        if len & 0xC0 == 0xC0 {
            // Compression pointer: 2 bytes total, no further parsing required.
            if pos + 1 >= buf.len() {
                return Err(ProxyPenError::InvalidConfig(
                    "DNS name pointer truncated".into(),
                ));
            }
            return Ok(pos + 2);
        }
        pos += 1 + len as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_server_accepts_ip_only() {
        let sa = parse_server("1.2.3.4").unwrap();
        assert_eq!(sa.port(), DEFAULT_DNS_PORT);
        assert_eq!(sa.ip().to_string(), "1.2.3.4");
    }

    #[test]
    fn parse_server_accepts_ip_port() {
        let sa = parse_server("1.2.3.4:5353").unwrap();
        assert_eq!(sa.port(), 5353);
    }

    #[test]
    fn parse_server_rejects_hostname() {
        assert!(parse_server("dns.example.com").is_err());
    }

    #[test]
    fn encode_then_decode_roundtrip() {
        let id: u16 = 0x1234;
        let q = encode_query(id, "example.com").unwrap();
        // Build a synthetic response: copy the query, set ANCOUNT=1, append
        // a compressed-pointer answer with A 93.184.216.34.
        let mut r = q.clone();
        r[2] = 0x81; // QR=1, RD=1
        r[3] = 0x80; // RA=1, RCODE=0
        r[6] = 0x00;
        r[7] = 0x01; // ANCOUNT = 1
        // Answer: name pointer to offset 12, TYPE=A, CLASS=IN, TTL=60, RDLEN=4, IP
        r.extend_from_slice(&[0xC0, 0x0C]);
        r.extend_from_slice(&1u16.to_be_bytes());
        r.extend_from_slice(&1u16.to_be_bytes());
        r.extend_from_slice(&60u32.to_be_bytes());
        r.extend_from_slice(&4u16.to_be_bytes());
        r.extend_from_slice(&[93, 184, 216, 34]);

        let ip = decode_a_response(id, &r).unwrap();
        assert_eq!(ip, Ipv4Addr::new(93, 184, 216, 34));
    }

    #[test]
    fn decode_rejects_wrong_id() {
        let mut r = vec![0u8; 12];
        r[0] = 0; r[1] = 1; // id=1
        r[6] = 0; r[7] = 1; // ANCOUNT=1 (not consulted because id mismatch fires first)
        assert!(decode_a_response(99, &r).is_err());
    }

    #[test]
    fn decode_rejects_nxdomain() {
        let mut r = vec![0u8; 12];
        r[0] = 0; r[1] = 7; // id=7
        r[2] = 0x81; r[3] = 0x83; // RCODE=3 NXDOMAIN
        assert!(decode_a_response(7, &r).is_err());
    }
}
