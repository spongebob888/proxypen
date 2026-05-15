use std::fmt;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Http1,
    Http2,
    Http3,
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Protocol::Http1 => write!(f, "HTTP/1.1"),
            Protocol::Http2 => write!(f, "HTTP/2"),
            Protocol::Http3 => write!(f, "HTTP/3"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum TestStatus {
    Success,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct Timing {
    /// Time to complete the SOCKS5 handshake (proxy modes only).
    pub socks_handshake: Option<Duration>,
    /// Time to complete the direct TCP connect (direct mode, TCP-based protocols).
    pub tcp_connect: Option<Duration>,
    pub tls_handshake: Option<Duration>,
    pub first_byte: Duration,
    pub total: Duration,
}

#[derive(Debug, Clone)]
pub struct TestResult {
    pub protocol: Protocol,
    pub status: TestStatus,
    pub http_status: Option<u16>,
    pub timing: Timing,
    pub response_size: Option<usize>,
}

impl fmt::Display for TestResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let proto = format!("[{}]", self.protocol);
        match &self.status {
            TestStatus::Success => {
                let status = self.http_status.unwrap_or(0);
                let total = self.timing.total.as_millis();
                let ttfb = self.timing.first_byte.as_millis();

                write!(f, "{:<10} OK {} ({total}ms)", proto, status)?;

                if let Some(socks) = self.timing.socks_handshake {
                    write!(f, " socks:{}ms", socks.as_millis())?;
                }

                if let Some(tcp) = self.timing.tcp_connect {
                    write!(f, " tcp:{}ms", tcp.as_millis())?;
                }

                if let Some(tls) = self.timing.tls_handshake {
                    write!(f, " tls:{}ms", tls.as_millis())?;
                }

                write!(f, " ttfb:{ttfb}ms")?;

                if let Some(size) = self.response_size {
                    write!(f, " size:{size}B")?;
                }
                Ok(())
            }
            TestStatus::Failed(err) => {
                write!(f, "{:<10} FAILED: {}", proto, err)
            }
        }
    }
}
