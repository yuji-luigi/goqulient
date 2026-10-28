//! Port of GokuCore's `TLSFactory` for rustls: prefer/require encrypt without verifying, verifyCA checks the chain only,
//! verifyFull the chain and the host name. A CA file replaces the platform roots (as NIOSSL's `trustRoots = .file`).
use crate::error::{DbError, Result};
use crate::profile::{SslMode, SslSettings};
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use std::sync::Arc;
use tokio_postgres_rustls::MakeRustlsConnect;

/// The connector for `settings`; with `disable` it is never used (tokio-postgres doesn't negotiate TLS) but the cancel
/// token still needs one.
pub fn connector(settings: &SslSettings) -> Result<MakeRustlsConnect> {
    // Shared rule for every driver: ring is the process-wide provider (no aws-lc-rs / openssl in the tree).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let bad = |m: String| DbError::InvalidConfiguration(m);
    let verifier: Arc<dyn ServerCertVerifier> = match settings.mode {
        SslMode::Disable | SslMode::Prefer | SslMode::Require => {
            Arc::new(NoVerification(provider.signature_verification_algorithms))
        }
        SslMode::VerifyCa | SslMode::VerifyFull => {
            let ca = expand(&settings.ca_certificate_path);
            let full: Arc<dyn ServerCertVerifier> = if ca.is_empty() {
                Arc::new(
                    rustls_platform_verifier::Verifier::new(provider.clone())
                        .map_err(|e| bad(format!("System certificates: {e}")))?,
                )
            } else {
                let unreadable = |e: String| bad(format!("CA certificate not readable: {ca} ({e})"));
                let mut roots = RootCertStore::empty();
                for cert in CertificateDer::pem_file_iter(&ca).map_err(|e| unreadable(e.to_string()))? {
                    roots.add(cert.map_err(|e| unreadable(e.to_string()))?).map_err(|e| unreadable(e.to_string()))?;
                }
                if roots.is_empty() {
                    return Err(unreadable("no certificates".into()));
                }
                WebPkiServerVerifier::builder_with_provider(roots.into(), provider.clone())
                    .build()
                    .map_err(|e| unreadable(e.to_string()))?
            };
            if settings.mode == SslMode::VerifyCa { Arc::new(ChainOnly(full)) } else { full }
        }
    };
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| bad(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier);
    let (cert, key) = (expand(&settings.client_certificate_path), expand(&settings.client_key_path));
    let config = match (cert.is_empty(), key.is_empty()) {
        (true, true) => builder.with_no_client_auth(),
        (false, false) => {
            let chain = CertificateDer::pem_file_iter(&cert)
                .and_then(|it| it.collect::<Result<Vec<_>, _>>())
                .map_err(|e| bad(format!("Client certificate not readable: {cert} ({e})")))?;
            let key =
                PrivateKeyDer::from_pem_file(&key).map_err(|e| bad(format!("Client key not readable: {key} ({e})")))?;
            builder.with_client_auth_cert(chain, key).map_err(|e| bad(format!("Client certificate: {e}")))?
        }
        _ => return Err(bad("Client certificate and key must both be set.".into())),
    };
    Ok(MakeRustlsConnect::new(config))
}

fn expand(path: &str) -> String {
    let p = path.trim();
    match p.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') => {
            match std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
                Ok(home) => format!("{home}{rest}"),
                Err(_) => p.to_owned(),
            }
        }
        _ => p.to_owned(),
    }
}

/// prefer / require: encrypted, but any certificate is accepted (libpq's semantics, and GokuCore's).
#[derive(Debug)]
struct NoVerification(WebPkiSupportedAlgorithms);

impl ServerCertVerifier for NoVerification {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

/// verifyCA: the full verifier, minus the host-name check (webpki checks the chain before the name).
#[derive(Debug)]
struct ChainOnly(Arc<dyn ServerCertVerifier>);

impl ServerCertVerifier for ChainOnly {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self.0.verify_server_cert(end_entity, intermediates, server_name, ocsp, now) {
            Err(rustls::Error::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
            )) => Ok(ServerCertVerified::assertion()),
            other => other,
        }
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls12_signature(message, cert, dss)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls13_signature(message, cert, dss)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(mode: SslMode) -> SslSettings {
        SslSettings { mode, ..SslSettings::default() }
    }

    #[test]
    fn builds_every_mode_and_rejects_bad_files() {
        for mode in SslMode::ALL {
            assert!(connector(&settings(*mode)).is_ok(), "{mode:?}");
        }
        let mut s = settings(SslMode::VerifyFull);
        s.ca_certificate_path = "/nonexistent/ca.pem".into();
        let e = connector(&s).err().unwrap();
        assert!(matches!(&e, DbError::InvalidConfiguration(m) if m.contains("CA certificate not readable")), "{e}");
        let mut s = settings(SslMode::Require);
        s.client_certificate_path = "/x.pem".into();
        assert!(matches!(connector(&s), Err(DbError::InvalidConfiguration(m)) if m.contains("must both be set")));
    }

    #[test]
    fn tilde_expands_to_home() {
        assert_eq!(expand("  /a/b "), "/a/b");
        assert_eq!(expand("~user/x"), "~user/x");
        if let Ok(home) = std::env::var("HOME") {
            assert_eq!(expand("~/ca.pem"), format!("{home}/ca.pem"));
        }
    }
}
