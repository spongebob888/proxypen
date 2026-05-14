use std::io;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ProxyPenError {
    #[error("SOCKS5 error: {0}")]
    Socks(String),

    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("TLS error: {0}")]
    Tls(String),

    #[error("QUIC error: {0}")]
    Quic(String),

    #[error("timeout")]
    Timeout,

    #[error("invalid config: {0}")]
    InvalidConfig(String),
}

impl From<shadowquic::error::SError> for ProxyPenError {
    fn from(e: shadowquic::error::SError) -> Self {
        ProxyPenError::Socks(e.to_string())
    }
}

impl From<h2::Error> for ProxyPenError {
    fn from(e: h2::Error) -> Self {
        ProxyPenError::Http(format!("h2: {e}"))
    }
}

impl From<quinn::ConnectionError> for ProxyPenError {
    fn from(e: quinn::ConnectionError) -> Self {
        ProxyPenError::Quic(e.to_string())
    }
}

impl From<quinn::ConnectError> for ProxyPenError {
    fn from(e: quinn::ConnectError) -> Self {
        ProxyPenError::Quic(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ProxyPenError>;
