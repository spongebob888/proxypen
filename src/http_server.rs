use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::anyhow;
use bytes::Bytes;
use quinn::crypto::rustls::QuicServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

/// Configuration for the local HTTP test server (all three protocols).
#[derive(Debug, Clone)]
pub struct HttpServerConfig {
    /// IP address to bind on.
    pub bind: IpAddr,
    /// Port for plain HTTP/1.
    pub http1_port: u16,
    /// Port for TLS HTTP/1 + HTTP/2.
    pub http2_port: u16,
    /// Port for QUIC HTTP/3.
    pub http3_port: u16,
    /// Size in bytes of the response body (filled with 'x').
    pub response_size: usize,
}

impl Default for HttpServerConfig {
    fn default() -> Self {
        Self {
            bind: IpAddr::from([127, 0, 0, 1]),
            http1_port: 8080,
            http2_port: 8443,
            http3_port: 8444,
            response_size: 128,
        }
    }
}

/// Start a combined HTTP/1, HTTP/2, HTTP/3 test server.
/// Runs until Ctrl+C or error.
pub async fn run_server(config: HttpServerConfig) -> anyhow::Result<()> {
    // --- Generate self-signed certificate ---
    let (cert_chain, key_der) = generate_self_signed_rcgen()
        .map_err(|e| anyhow!("failed to generate self-signed cert: {e}"))?;

    let cert_chain = Arc::new(cert_chain);
    let key_der = Arc::new(key_der);

    let response_body: Arc<[u8]> =
        Arc::from(vec![b'x'; config.response_size].into_boxed_slice());

    eprintln!("=== HTTP Test Server ===");
    eprintln!("Certificate CN: proxypen-test.local");
    eprintln!();

    let mut handles = vec![];

    // --- HTTP/1 plain ---
    let http1_addr = SocketAddr::new(config.bind, config.http1_port);
    {
        let body = response_body.clone();
        let handle = tokio::spawn(async move {
            run_http1_plain(http1_addr, body).await
        });
        eprintln!("HTTP/1  (plain) → http://{http1_addr}");
        handles.push(handle);
    }

    // --- HTTP/1 TLS + HTTP/2 ---
    let http2_addr = SocketAddr::new(config.bind, config.http2_port);
    {
        let cert_chain = cert_chain.clone();
        let key_der = key_der.clone();
        let body = response_body.clone();
        let handle = tokio::spawn(async move {
            run_tls_server(http2_addr, cert_chain, key_der, body).await
        });
        eprintln!("HTTP/2  (TLS)   → https://{http2_addr}");
        handles.push(handle);
    }

    // --- HTTP/3 QUIC ---
    let http3_addr = SocketAddr::new(config.bind, config.http3_port);
    {
        let cert_chain = cert_chain.clone();
        let key_der = key_der.clone();
        let body = response_body.clone();
        let handle = tokio::spawn(async move {
            run_http3_server(http3_addr, cert_chain, key_der, body).await
        });
        eprintln!("HTTP/3  (QUIC)  → https://{http3_addr}");
        handles.push(handle);
    }

    eprintln!();
    eprintln!("Press Ctrl+C to stop.");
    eprintln!();
    eprintln!("Test commands:");
    eprintln!(
        "  proxypen press -t http://{bind}:{h1} -P http1 -c 10 -n 100",
        bind = config.bind,
        h1 = config.http1_port
    );
    eprintln!(
        "  proxypen press -t https://{bind}:{h2} -P http2 -c 10 -n 100 --insecure",
        bind = config.bind,
        h2 = config.http2_port
    );
    eprintln!(
        "  proxypen press -t https://{bind}:{h3} -P http3 -c 10 -n 100 --insecure",
        bind = config.bind,
        h3 = config.http3_port
    );
    eprintln!();

    for handle in handles {
        let _ = handle.await;
    }

    Ok(())
}

// ---------------------------------------------------------------
// HTTP/1 plain server
// ---------------------------------------------------------------

async fn run_http1_plain(addr: SocketAddr, body: Arc<[u8]>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    loop {
        let (mut stream, peer) = listener.accept().await?;
        let body = body.clone();
        tokio::spawn(async move {
            let _ = serve_http1(&mut stream, &body).await;
            tracing::trace!("http/1 plain: {peer} closed");
        });
    }
}

async fn serve_http1(
    stream: &mut (impl AsyncReadExt + AsyncWriteExt + Unpin),
    body: &[u8],
) -> anyhow::Result<()> {
    let mut buf = vec![0u8; 8192];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }

    let response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/plain\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         Server: proxypen-http-server\r\n\
         \r\n",
        body.len()
    );

    stream.write_all(response.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await?;
    Ok(())
}

// ---------------------------------------------------------------
// TLS server (HTTP/1.1 + HTTP/2 over ALPN)
// ---------------------------------------------------------------

async fn run_tls_server(
    addr: SocketAddr,
    cert_chain: Arc<Vec<CertificateDer<'static>>>,
    key_der: Arc<PrivatePkcs8KeyDer<'static>>,
    body: Arc<[u8]>,
) -> anyhow::Result<()> {
    let tls_config = make_server_tls_config(cert_chain, key_der, &[b"h2".to_vec(), b"http/1.1".to_vec()])?;
    let acceptor = TlsAcceptor::from(Arc::new(tls_config));

    let listener = TcpListener::bind(addr).await?;
    loop {
        let (stream, peer) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let body = body.clone();
        tokio::spawn(async move {
            match acceptor.accept(stream).await {
                Ok(tls_stream) => {
                    let negotiated = tls_stream
                        .get_ref()
                        .1
                        .alpn_protocol()
                        .map(|p| String::from_utf8_lossy(p).to_string());

                    match negotiated.as_deref() {
                        Some("h2") => {
                            if let Err(e) = serve_http2(tls_stream, &body).await {
                                tracing::warn!("http/2 serve {peer}: {e}");
                            }
                        }
                        _ => {
                            let mut stream = tls_stream;
                            let _ = serve_http1_over_tls(&mut stream, &body).await;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("tls accept {peer}: {e}");
                }
            }
            tracing::trace!("tls: {peer} closed");
        });
    }
}

async fn serve_http1_over_tls(
    stream: &mut tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    body: &[u8],
) -> anyhow::Result<()> {
    let mut buf = vec![0u8; 8192];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }

    let response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/plain\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         Server: proxypen-http-server\r\n\
         \r\n",
        body.len()
    );

    stream.write_all(response.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await?;
    Ok(())
}

async fn serve_http2(
    stream: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    body: &[u8],
) -> anyhow::Result<()> {
    use h2::server;
    let body_bytes = Bytes::copy_from_slice(body);

    let mut h2_conn = server::handshake(stream).await?;

    while let Some(result) = h2_conn.accept().await {
        match result {
            Ok((request, mut respond)) => {
                tracing::trace!("h2 request: {} {}", request.method(), request.uri());
                let body = body_bytes.clone();
                tokio::spawn(async move {
                    let response = http::Response::builder()
                        .status(200)
                        .header("content-type", "text/plain")
                        .header("server", "proxypen-http-server")
                        .body(())
                        .unwrap();

                    match respond.send_response(response, false) {
                        Ok(mut send_stream) => {
                            let _ = send_stream.send_data(body, true);
                        }
                        Err(e) => {
                            tracing::warn!("h2 send error: {e}");
                        }
                    }
                });
            }
            Err(e) => {
                tracing::warn!("h2 accept error: {e}");
                break;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------
// HTTP/3 QUIC server
// ---------------------------------------------------------------

async fn run_http3_server(
    addr: SocketAddr,
    cert_chain: Arc<Vec<CertificateDer<'static>>>,
    key_der: Arc<PrivatePkcs8KeyDer<'static>>,
    body: Arc<[u8]>,
) -> anyhow::Result<()> {
    let tls_config = make_server_tls_config(cert_chain, key_der, &[b"h3".to_vec()])?;
    let quic_config: QuicServerConfig = tls_config
        .try_into()
        .map_err(|e| anyhow!("QUIC server config: {e}"))?;

    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_config));
    let mut transport_config = quinn::TransportConfig::default();
    transport_config.max_idle_timeout(Some(
        quinn::IdleTimeout::from(quinn::VarInt::from_u32(30_000)),
    ));
    server_config.transport_config(Arc::new(transport_config));

    let runtime = quinn::default_runtime()
        .ok_or_else(|| anyhow!("no async runtime for QUIC"))?;

    let socket = std::net::UdpSocket::bind(addr)?;
    socket.set_nonblocking(true)?;

    let endpoint = quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(server_config),
        socket,
        runtime,
    )
    .map_err(|e| anyhow!("QUIC endpoint: {e}"))?;

    eprintln!("HTTP/3 server listening on {addr}");

    while let Some(incoming) = endpoint.accept().await {
        let body = body.clone();
        tokio::spawn(async move {
            match incoming.await {
                Ok(conn) => {
                    if let Err(e) = serve_http3_conn(conn, &body).await {
                        tracing::warn!("h3 serve error: {e}");
                    }
                }
                Err(e) => {
                    tracing::warn!("QUIC accept error: {e}");
                }
            }
        });
    }
    Ok(())
}

async fn serve_http3_conn(conn: quinn::Connection, body: &[u8]) -> anyhow::Result<()> {
    let h3_conn = h3_quinn::Connection::new(conn);
    let mut h3_server = h3::server::builder()
        .build(h3_conn)
        .await
        .map_err(|e| anyhow!("h3 server builder: {e}"))?;

    let body_bytes = Bytes::copy_from_slice(body);
    loop {
        match h3_server.accept().await {
            Ok(Some(resolver)) => {
                let body = body_bytes.clone();
                tokio::spawn(async move {
                    match resolver.resolve_request().await {
                        Ok((req, mut stream)) => {
                            tracing::trace!("h3 request: {} {}", req.method(), req.uri());
                            let resp = http::Response::builder()
                                .status(200)
                                .header("content-type", "text/plain")
                                .header("server", "proxypen-http-server")
                                .body(())
                                .unwrap();

                            if let Err(e) = stream.send_response(resp).await {
                                tracing::warn!("h3 send error: {e}");
                                return;
                            }
                            if let Err(e) = stream.send_data(body).await {
                                tracing::warn!("h3 send data error: {e}");
                                return;
                            }
                            if let Err(e) = stream.finish().await {
                                tracing::warn!("h3 finish error: {e}");
                            }
                        }
                        Err(e) => {
                            tracing::warn!("h3 resolve request error: {e}");
                        }
                    }
                });
            }
            Ok(None) => break,
            Err(e) => {
                tracing::warn!("h3 accept error: {e}");
                break;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------
// Certificate generation (rcgen)
// ---------------------------------------------------------------

fn generate_self_signed_rcgen() -> anyhow::Result<(Vec<CertificateDer<'static>>, PrivatePkcs8KeyDer<'static>)> {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;

    let mut params = rcgen::CertificateParams::new(vec![
        "proxypen-test.local".to_string(),
        "localhost".to_string(),
    ])?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        "proxypen-test.local",
    );
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);

    let cert = params.self_signed(&key)?;

    let cert_der: CertificateDer<'static> = CertificateDer::from(cert.der().to_vec());
    let key_der_vec = key.serialize_der();
    let key_der = PrivatePkcs8KeyDer::from(key_der_vec);

    Ok((vec![cert_der], key_der))
}

// ---------------------------------------------------------------
// TLS server config builder
// ---------------------------------------------------------------

fn make_server_tls_config(
    cert_chain: Arc<Vec<CertificateDer<'static>>>,
    key_der: Arc<PrivatePkcs8KeyDer<'static>>,
    alpn: &[Vec<u8>],
) -> anyhow::Result<ServerConfig> {
    let key_inner = key_der.secret_pkcs8_der().to_vec();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_inner));

    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain.to_vec(), key)?;

    if !alpn.is_empty() {
        config.alpn_protocols = alpn.to_vec();
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    // ---- HttpServerConfig ----

    #[test]
    fn config_default_values() {
        let c = HttpServerConfig::default();
        assert_eq!(c.bind, IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
        assert_eq!(c.http1_port, 8080);
        assert_eq!(c.http2_port, 8443);
        assert_eq!(c.http3_port, 8444);
        assert_eq!(c.response_size, 128);
    }

    // ---- Certificate generation ----

    #[test]
    fn certificate_generation_produces_non_empty_der() {
        let (cert_chain, key_der) = generate_self_signed_rcgen().expect("cert gen");
        assert_eq!(cert_chain.len(), 1);
        assert!(!cert_chain[0].is_empty());
        assert!(!key_der.secret_pkcs8_der().is_empty());

        // Basic sanity: DER should start with SEQUENCE (0x30)
        assert_eq!(cert_chain[0].as_ref()[0], 0x30);
        // Key should be PKCS#8 (starts with SEQUENCE)
        assert_eq!(key_der.secret_pkcs8_der()[0], 0x30);
    }

    // ---- TLS server config ----

    /// Call once in TLS-related tests (install_default is idempotent).
    fn ensure_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    #[test]
    fn tls_config_with_alpn() {
        ensure_crypto_provider();
        let (cert_chain, key_der) = generate_self_signed_rcgen().unwrap();
        let cert_chain = Arc::new(cert_chain);
        let key_der = Arc::new(key_der);

        let cfg = make_server_tls_config(
            cert_chain,
            key_der,
            &[b"h2".to_vec(), b"http/1.1".to_vec()],
        )
        .expect("tls config with alpn");

        assert_eq!(cfg.alpn_protocols.len(), 2);
        assert_eq!(cfg.alpn_protocols[0], b"h2");
        assert_eq!(cfg.alpn_protocols[1], b"http/1.1");
    }

    #[test]
    fn tls_config_without_alpn() {
        ensure_crypto_provider();
        let (cert_chain, key_der) = generate_self_signed_rcgen().unwrap();
        let cert_chain = Arc::new(cert_chain);
        let key_der = Arc::new(key_der);

        let cfg = make_server_tls_config(cert_chain, key_der, &[]).expect("tls config no alpn");
        assert!(cfg.alpn_protocols.is_empty());
    }

    #[test]
    fn tls_config_with_h3_alpn() {
        ensure_crypto_provider();
        let (cert_chain, key_der) = generate_self_signed_rcgen().unwrap();
        let cert_chain = Arc::new(cert_chain);
        let key_der = Arc::new(key_der);

        let cfg =
            make_server_tls_config(cert_chain, key_der, &[b"h3".to_vec()]).expect("tls config h3");
        assert_eq!(cfg.alpn_protocols.len(), 1);
        assert_eq!(cfg.alpn_protocols[0], b"h3");
    }

    // ---- serve_http1 ----

    #[tokio::test]
    async fn serve_http1_responds_200_with_body() {
        let (client, server) = tokio::io::duplex(4096);
        let (mut client_rx, mut client_tx) = tokio::io::split(client);

        let body_data = b"hello test body";
        let server_body = body_data.to_vec();

        let handle = tokio::spawn(async move {
            let (server_rx, server_tx) = tokio::io::split(server);
            let mut stream = tokio::io::join(server_rx, server_tx);
            let _ = serve_http1(&mut stream, &server_body).await;
        });

        client_tx
            .write_all(b"GET / HTTP/1.1\r\nHost: test\r\n\r\n")
            .await
            .unwrap();

        // Read response
        let mut resp = Vec::new();
        client_rx.read_to_end(&mut resp).await.unwrap();

        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.starts_with("HTTP/1.1 200 OK"));
        assert!(resp_str.contains("Content-Length: 15"));
        assert!(resp_str.ends_with("hello test body"));

        handle.await.unwrap();
    }

    #[tokio::test]
    async fn serve_http1_different_body_size() {
        let (client, server) = tokio::io::duplex(4096);
        let (mut client_rx, mut client_tx) = tokio::io::split(client);

        let body = vec![b'x'; 256];

        let handle = tokio::spawn(async move {
            let (server_rx, server_tx) = tokio::io::split(server);
            let mut stream = tokio::io::join(server_rx, server_tx);
            let _ = serve_http1(&mut stream, &body).await;
        });

        client_tx
            .write_all(b"GET /foo HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();

        let mut resp = Vec::new();
        client_rx.read_to_end(&mut resp).await.unwrap();

        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.contains("Content-Length: 256"));
        // Verify body
        let header_end = resp_str.find("\r\n\r\n").unwrap() + 4;
        assert_eq!(resp[header_end..], vec![b'x'; 256]);

        handle.await.unwrap();
    }

    #[tokio::test]
    async fn serve_http1_empty_request_closes_gracefully() {
        let (client, server) = tokio::io::duplex(4096);
        let (client_rx, client_tx) = tokio::io::split(client);

        // Drop the client write half immediately so the server sees EOF
        drop(client_tx);

        let handle = tokio::spawn(async move {
            let (server_rx, server_tx) = tokio::io::split(server);
            let mut stream = tokio::io::join(server_rx, server_tx);
            let result = serve_http1(&mut stream, b"x").await;
            assert!(result.is_ok());
        });

        // Client reads should get nothing (server sends nothing on EOF)
        let mut client_rx = client_rx;
        let mut resp = Vec::new();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client_rx.read_to_end(&mut resp),
        )
        .await;

        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }
}
