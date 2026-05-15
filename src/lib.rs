use std::time::Duration;

pub mod bench;
pub mod config;
pub mod direct;
pub mod error;
pub mod http1;
pub mod http2;
pub mod http3;
pub mod result;
pub mod socks;
pub mod tls;
pub mod transport;
pub mod udp_socket;

pub use config::{ProxyAuth, ProxyConfig, TestTarget};
pub use direct::{DirectConfig, InterfaceSpec};
pub use error::ProxyPenError;
pub use result::{Protocol, TestResult, TestStatus, Timing};
pub use transport::Transport;

/// Main entry point for testing proxy / direct HTTP connectivity.
pub struct ProxyPen {
    pub transport: Transport,
}

impl ProxyPen {
    pub fn new(transport: Transport) -> Self {
        Self { transport }
    }

    /// Test HTTP/1.1 over the configured transport.
    pub async fn test_http1(&self, target: &TestTarget, timeout: Duration) -> TestResult {
        http1::test(&self.transport, target, timeout).await
    }

    /// Test HTTP/2 over the configured transport (TLS with h2 ALPN).
    pub async fn test_http2(&self, target: &TestTarget, timeout: Duration) -> TestResult {
        http2::test(&self.transport, target, timeout).await
    }

    /// Test HTTP/3 over the configured transport (QUIC).
    pub async fn test_http3(&self, target: &TestTarget, timeout: Duration) -> TestResult {
        http3::test(&self.transport, target, timeout).await
    }

    /// Test all protocols and return results.
    pub async fn test_all(&self, target: &TestTarget, timeout: Duration) -> Vec<TestResult> {
        let mut results = Vec::new();
        results.push(self.test_http1(target, timeout).await);
        results.push(self.test_http2(target, timeout).await);
        results.push(self.test_http3(target, timeout).await);
        results
    }
}
