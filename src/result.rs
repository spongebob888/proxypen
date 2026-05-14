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
    pub socks_handshake: Duration,
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
                let socks = self.timing.socks_handshake.as_millis();
                let ttfb = self.timing.first_byte.as_millis();

                write!(f, "{:<10} OK {} ({total}ms) socks:{socks}ms", proto, status)?;

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
