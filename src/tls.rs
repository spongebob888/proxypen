use rustls::ClientConfig;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use webpki_roots::TLS_SERVER_ROOTS;

use crate::error::Result;

/// Create a rustls ClientConfig with system root certificates.
/// Optionally configure ALPN protocols (e.g., ["h2"] for HTTP/2).
/// Set `insecure` to true to skip certificate verification.
pub fn make_tls_config(alpn: Option<Vec<Vec<u8>>>, insecure: bool) -> Result<ClientConfig> {
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(TLS_SERVER_ROOTS.iter().cloned());

    let config_builder = ClientConfig::builder();

    let mut config = if insecure {
        config_builder
            .dangerous()
            .with_custom_certificate_verifier(std::sync::Arc::new(NoVerify))
    } else {
        config_builder
            .with_root_certificates(root_store)
    }
    .with_no_client_auth();

    if let Some(alpn_protos) = alpn {
        config.alpn_protocols = alpn_protos;
    }

    Ok(config)
}

/// A TLS certificate verifier that accepts any certificate.
/// Only for testing with self-signed certs!
#[derive(Debug)]
struct NoVerify;

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA256,
        ]
    }
}
