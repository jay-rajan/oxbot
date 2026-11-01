//! The Farm's own TLS certificate. Gates pin its fingerprint, so no outside
//! authority is involved and a look-alike "Farm" is refused.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::TlsAcceptor;

/// The Farm's certificate and key.
#[derive(Debug)]
pub struct Identity {
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
    /// SHA-256 of the certificate, as lowercase hex. Gates pin this.
    pub fingerprint: String,
}

impl Identity {
    /// Loads the certificate from `dir`, or makes one (the key readable only
    /// by this account) the first time.
    ///
    /// # Errors
    /// Fails if the files can't be read, made or saved.
    pub fn load_or_create(dir: &Path) -> anyhow::Result<Self> {
        let cert_path = dir.join("farm-cert.der");
        let key_path = dir.join("farm-key.der");
        let (cert, key) = if cert_path.exists() && key_path.exists() {
            (std::fs::read(&cert_path)?, std::fs::read(&key_path)?)
        } else {
            std::fs::create_dir_all(dir)?;
            let made = rcgen::generate_simple_self_signed(vec![oxbot_gate::CERT_NAME.to_owned()])
                .context("couldn't make the Farm's certificate")?;
            let cert = made.cert.der().to_vec();
            let key = made.signing_key.serialize_der();
            crate::secret::write_private(&key_path, &key)?;
            std::fs::write(&cert_path, &cert)?;
            tracing::info!("made the Farm's certificate for the Gate door");
            (cert, key)
        };
        Ok(Self {
            fingerprint: oxbot_gate::fingerprint(&cert),
            cert: CertificateDer::from(cert),
            key: PrivatePkcs8KeyDer::from(key),
        })
    }

    /// A TLS acceptor that answers with this certificate.
    pub(super) fn tls(&self) -> anyhow::Result<TlsAcceptor> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(
                vec![self.cert.clone()],
                PrivateKeyDer::Pkcs8(self.key.clone_key()),
            )?;
        Ok(TlsAcceptor::from(Arc::new(config)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_certificate_is_made_once_and_kept_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let first = Identity::load_or_create(tmp.path()).unwrap();
        let again = Identity::load_or_create(tmp.path()).unwrap();
        assert_eq!(first.fingerprint, again.fingerprint);
        assert_eq!(first.fingerprint.len(), 64);
        let mode = std::fs::metadata(tmp.path().join("farm-key.der"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
