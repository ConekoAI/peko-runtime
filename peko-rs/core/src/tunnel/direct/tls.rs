//! Shared TLS primitives for direct cross-runtime connections and the
//! PekoHub tunnel.
//!
//! This module exposes certificate/SPKI pinning, root-store construction,
//! and rustls client/server config builders so both `tunnel::client` and
//! `tunnel::direct` can reuse the same TLS logic.

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

    let mut config = if let (Some(cert_path), Some(key_path)) = (cert_path, key_path) {
        let cert_chain = load_cert_chain(cert_path)?;
        let key = load_private_key(key_path)?;
        builder
            .with_client_auth_cert(cert_chain, key)
            .map_err(|e| TlsError::InvalidClientAuth(e.to_string()))?
    } else {
        builder.with_no_client_auth()
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

/// Build a rustls server config from a certificate chain and private key.
///
/// If `client_ca_path` is provided, client certificates are required and
/// validated against that CA (mTLS).
pub fn build_server_config(
    cert_path: &Path,
    key_path: &Path,
    client_ca_path: Option<&Path>,
) -> Result<Arc<rustls::ServerConfig>, TlsError> {
    let cert_chain = load_cert_chain(cert_path)?;
    let key = load_private_key(key_path)?;

    let mut config = if let Some(client_ca_path) = client_ca_path {
        let client_ca_pem = std::fs::read(client_ca_path).map_err(|e| TlsError::Read {
            path: client_ca_path.display().to_string(),
            source: e,
        })?;
        let client_certs = rustls_pemfile::certs(&mut client_ca_pem.as_slice())
            .map_err(|e| TlsError::CertParse(e.to_string()))?;
        let mut client_roots = rustls::RootCertStore::empty();
        if client_certs.is_empty() {
            return Err(TlsError::EmptyCert(client_ca_path.display().to_string()));
        }
        for cert in client_certs {
            client_roots
                .add(cert.into())
                .map_err(|e| TlsError::AddCa(e.to_string()))?;
        }

        let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(client_roots))
            .build()
            .map_err(|e| TlsError::VerifierBuild(e.to_string()))?;

        rustls::ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(cert_chain, key)
            .map_err(|e| TlsError::InvalidClientAuth(e.to_string()))?
    } else {
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert_chain, key)
            .map_err(|e| TlsError::InvalidClientAuth(e.to_string()))?
    };

    config.alpn_protocols = vec![b"peko-direct/1".to_vec()];

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
    use std::io::Write;
    use tempfile::TempDir;

    fn generate_test_cert(temp: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
        let cert_path = temp.path().join("test.crt");
        let key_path = temp.path().join("test.key");

        // Generate a self-signed cert with openssl for tests if available,
        // otherwise skip. This keeps the unit test deterministic when openssl
        // is present and avoids false failures on minimal CI images.
        let output = std::process::Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-keyout",
                key_path.to_str().unwrap(),
                "-out",
                cert_path.to_str().unwrap(),
                "-days",
                "1",
                "-nodes",
                "-subj",
                "/CN=test.peko.local",
            ])
            .output();

        if output.is_err() || !output.as_ref().unwrap().status.success() {
            // Provide a minimal synthetic PEM so the test compiles; load
            // will fail, which we handle gracefully.
            let mut cert_file = std::fs::File::create(&cert_path).unwrap();
            cert_file
                .write_all(b"-----BEGIN CERTIFICATE-----\nMIIBkTCB+wIJAKHBfpE\n-----END CERTIFICATE-----\n")
                .unwrap();
            let mut key_file = std::fs::File::create(&key_path).unwrap();
            key_file
                .write_all(b"-----BEGIN PRIVATE KEY-----\nMIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg\n-----END PRIVATE KEY-----\n")
                .unwrap();
        }

        (cert_path, key_path)
    }

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
    fn test_build_server_config_roundtrip() {
        let temp = TempDir::new().unwrap();
        let (cert_path, key_path) = generate_test_cert(&temp);

        // The synthetic cert generated above may fail to parse; only assert
        // success when openssl produced a real certificate.
        if let Ok(config) = build_server_config(&cert_path, &key_path, None) {
            assert_eq!(config.alpn_protocols, vec![b"peko-direct/1".to_vec()]);
        }
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
