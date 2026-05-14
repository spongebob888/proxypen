use std::time::Duration;

pub mod config;
pub mod error;
pub mod http1;
pub mod http2;
pub mod http3;
pub mod result;
pub mod socks;
pub mod tls;
pub mod udp_socket;

pub use config::{ProxyAuth, ProxyConfig, TestTarget};
pub use error::ProxyPenError;
pub use result::{Protocol, TestResult, TestStatus, Timing};

/// Main entry point for testing SOCKS5 proxy capabilities.
pub struct ProxyPen {
    pub config: ProxyConfig,
}

impl ProxyPen {
    pub fn new(config: ProxyConfig) -> Self {
        Self { config }
    }

    /// Test HTTP/1.1 through the SOCKS5 proxy (TCP CONNECT).
    pub async fn test_http1(&self, target: &TestTarget, timeout: Duration) -> TestResult {
        http1::test(&self.config, target, timeout).await
    }

    /// Test HTTP/2 through the SOCKS5 proxy (TCP CONNECT + TLS with h2 ALPN).
    pub async fn test_http2(&self, target: &TestTarget, timeout: Duration) -> TestResult {
        http2::test(&self.config, target, timeout).await
    }

    /// Test HTTP/3 through the SOCKS5 proxy (UDP ASSOCIATE + QUIC).
    pub async fn test_http3(&self, target: &TestTarget, timeout: Duration) -> TestResult {
        http3::test(&self.config, target, timeout).await
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
