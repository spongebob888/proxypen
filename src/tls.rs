use rustls::ClientConfig;
use webpki_roots::TLS_SERVER_ROOTS;

use crate::error::Result;

/// Create a rustls ClientConfig with system root certificates.
/// Optionally configure ALPN protocols (e.g., ["h2"] for HTTP/2).
pub fn make_tls_config(alpn: Option<Vec<Vec<u8>>>) -> Result<ClientConfig> {
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(TLS_SERVER_ROOTS.iter().cloned());

    let mut config = ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    if let Some(alpn_protos) = alpn {
        config.alpn_protocols = alpn_protos;
    }

    Ok(config)
}
