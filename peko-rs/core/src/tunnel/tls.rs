//! Shared TLS primitives for the PekoHub tunnel.
//!
//! This module exposes certificate/SPKI pinning, root-store construction,
//! and rustls client/server config builders. It lived under
//! `tunnel::direct` until sprint 3 Phase 12b retired the direct
//! transport; `tunnel::client` (the PekoHub tunnel) is the remaining
//! consumer. The server-side builders are retained for the tunnel's
//! own test surface.

use std::path::Path;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use sha2::Digest;

/// Errors that can occur while building or verifying TLS configuration.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("Failed to read TLS file {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("Failed to parse certificate chain: {0}")]
    CertParse(String),
    #[error("Certificate file contains no valid certificates: {0}")]
    EmptyCert(String),
    #[error("Failed to parse private key: {0}")]
    KeyParse(String),
    #[error("No supported private key found in {0}")]
    NoKeyFound(String),
    #[error("Invalid client cert/key pair: {0}")]
    InvalidClientAuth(String),
    #[error("Failed to build TLS verifier: {0}")]
    VerifierBuild(String),
    #[error("Failed to add CA certificate: {0}")]
    AddCa(String),
    #[error("Invalid pinned_cert_sha256: {0}")]
    InvalidPin(String),
    #[error("Certificate pin mismatch")]
    PinMismatch,
}

/// Build a root certificate store from an optional custom CA path.
///
/// If `ca_path` is `Some`, the file is loaded and its certificates are
/// added to an otherwise empty store. If `None`, the WebPKI root store is
/// used.
pub fn build_root_cert_store(ca_path: Option<&Path>) -> Result<rustls::RootCertStore, TlsError> {
    let mut roots = rustls::RootCertStore::empty();

    if let Some(ca_path) = ca_path {
        let ca_pem = std::fs::read(ca_path).map_err(|e| TlsError::Read {
            path: ca_path.display().to_string(),
            source: e,
        })?;
        let certs = rustls_pemfile::certs(&mut ca_pem.as_slice())
            .map_err(|e| TlsError::CertParse(e.to_string()))?;
        if certs.is_empty() {
            return Err(TlsError::EmptyCert(ca_path.display().to_string()));
        }
        for cert in certs {
            roots
                .add(cert.into())
                .map_err(|e| TlsError::AddCa(e.to_string()))?;
        }
    } else {
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }

    Ok(roots)
}

/// Build a rustls client config from raw TLS options.
///
/// `pinned_cert_sha256` is an optional base64-encoded SHA-256 fingerprint of
/// the expected end-entity certificate.
pub fn build_client_config(
    ca_path: Option<&Path>,
    cert_path: Option<&Path>,
    key_path: Option<&Path>,
    pinned_cert_sha256: Option<&str>,
) -> Result<Arc<rustls::ClientConfig>, TlsError> {
    let roots = build_root_cert_store(ca_path)?;
    let roots = Arc::new(roots);

    let default_verifier = rustls::client::WebPkiServerVerifier::builder(roots.clone())
        .build()
        .map_err(|e| TlsError::VerifierBuild(e.to_string()))?;

    let builder = rustls::ClientConfig::builder().with_root_certificates(roots);

    // ADR-057 transport audit: a half-configured mTLS (one of
    // cert/key set, the other missing) used to silently fall back to
    // no client auth — an operator believing mTLS was on while it was
    // off. Hard-error instead.
    let mut config = match (cert_path, key_path) {
        (Some(cert_path), Some(key_path)) => {
            let cert_chain = load_cert_chain(cert_path)?;
            let key = load_private_key(key_path)?;
            builder
                .with_client_auth_cert(cert_chain, key)
                .map_err(|e| TlsError::InvalidClientAuth(e.to_string()))?
        }
        (None, None) => builder.with_no_client_auth(),
        (Some(_), None) => {
            return Err(TlsError::InvalidClientAuth(
                "tls.cert_path set without tls.key_path — mTLS is half-configured; \
                 set both or neither"
                    .to_string(),
            ))
        }
        (None, Some(_)) => {
            return Err(TlsError::InvalidClientAuth(
                "tls.key_path set without tls.cert_path — mTLS is half-configured; \
                 set both or neither"
                    .to_string(),
            ))
        }
    };

    if let Some(pinned_sha256) = pinned_cert_sha256 {
        let expected = BASE64
            .decode(pinned_sha256)
            .map_err(|e| TlsError::InvalidPin(e.to_string()))?;
        config
            .dangerous()
            .set_certificate_verifier(Arc::new(PinningServerCertVerifier {
                inner: default_verifier,
                expected,
            }));
    }

    Ok(Arc::new(config))
}

/// Load a PEM-encoded certificate chain from disk.
pub fn load_cert_chain(
    path: &Path,
) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>, TlsError> {
    let pem = std::fs::read(path).map_err(|e| TlsError::Read {
        path: path.display().to_string(),
        source: e,
    })?;
    let certs = rustls_pemfile::certs(&mut pem.as_slice())
        .map_err(|e| TlsError::CertParse(e.to_string()))?;
    if certs.is_empty() {
        return Err(TlsError::EmptyCert(path.display().to_string()));
    }
    Ok(certs.into_iter().map(|c| c.into()).collect())
}

/// Load a PEM-encoded private key from disk.
///
/// Supports PKCS#8 and RSA private keys.
pub fn load_private_key(
    path: &Path,
) -> Result<rustls::pki_types::PrivateKeyDer<'static>, TlsError> {
    let pem = std::fs::read(path).map_err(|e| TlsError::Read {
        path: path.display().to_string(),
        source: e,
    })?;

    if let Some(key) = rustls_pemfile::pkcs8_private_keys(&mut pem.as_slice())
        .map_err(|e| TlsError::KeyParse(e.to_string()))?
        .into_iter()
        .next()
    {
        return Ok(rustls::pki_types::PrivateKeyDer::try_from(key)
            .map_err(|e| TlsError::KeyParse(e.to_string()))?);
    }

    if let Some(key) = rustls_pemfile::rsa_private_keys(&mut pem.as_slice())
        .map_err(|e| TlsError::KeyParse(e.to_string()))?
        .into_iter()
        .next()
    {
        return Ok(rustls::pki_types::PrivateKeyDer::try_from(key)
            .map_err(|e| TlsError::KeyParse(e.to_string()))?);
    }

    Err(TlsError::NoKeyFound(path.display().to_string()))
}

/// Verifier that delegates to the default WebPKI verifier and then checks
/// the end-entity certificate fingerprint against a configured pin.
#[derive(Debug)]
pub struct PinningServerCertVerifier {
    inner: Arc<dyn rustls::client::danger::ServerCertVerifier>,
    expected: Vec<u8>,
}

impl PinningServerCertVerifier {
    /// Create a new pinning verifier wrapping the default WebPKI verifier.
    ///
    /// `expected` is the raw SHA-256 digest of the expected end-entity
    /// certificate.
    #[must_use]
    pub fn new(
        inner: Arc<dyn rustls::client::danger::ServerCertVerifier>,
        expected: Vec<u8>,
    ) -> Self {
        Self { inner, expected }
    }
}

impl rustls::client::danger::ServerCertVerifier for PinningServerCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        )?;

        let actual = sha2::Sha256::digest(end_entity.as_ref());
        if actual.as_slice() != self.expected {
            return Err(rustls::Error::General(
                "server certificate does not match configured pin".to_string(),
            ));
        }

        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_build_client_config_with_webpki_defaults() {
        let config = build_client_config(None, None, None, None).unwrap();
        assert!(config.alpn_protocols.is_empty());
    }

    #[test]
    fn test_build_client_config_rejects_missing_ca() {
        let result = build_client_config(Some(Path::new("/nonexistent/ca.crt")), None, None, None);
        assert!(matches!(result, Err(TlsError::Read { .. })));
    }

    #[test]
    fn test_build_client_config_with_invalid_pin() {
        let result = build_client_config(None, None, None, Some("not-base64!!!"));
        assert!(matches!(result, Err(TlsError::InvalidPin(_))));
    }

    #[test]
    fn test_load_cert_chain_rejects_empty_file() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("empty.crt");
        std::fs::write(&path, b"").unwrap();
        let result = load_cert_chain(&path);
        assert!(matches!(result, Err(TlsError::EmptyCert(_))));
    }

    #[test]
    fn test_load_private_key_rejects_empty_file() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("empty.key");
        std::fs::write(&path, b"").unwrap();
        let result = load_private_key(&path);
        assert!(matches!(result, Err(TlsError::NoKeyFound(_))));
    }
}
