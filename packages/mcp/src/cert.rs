//! A self-signed P-256 dev certificate, generated in-memory at startup. The
//! browser pins it by `base64url(SHA-256(DER))` via WebTransport's
//! `serverCertificateHashes`, which mandates a validity window of <= 14 days —
//! so this cert lives 10 days and is regenerated each server start.

use anyhow::{Context, Result};
use base64::Engine;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

pub struct GeneratedCert {
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
}

impl GeneratedCert {
    pub fn new(hostname: &str) -> Result<Self> {
        let key_pair =
            KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).context("generate P-256 key pair")?;
        let mut dname = DistinguishedName::new();
        dname.push(DnType::CommonName, "awsm-audio-mcp self-signed");
        let mut params =
            CertificateParams::new(vec![hostname.to_string()]).context("certificate params")?;
        params.distinguished_name = dname;
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now;
        params.not_after = now + time::Duration::days(10);
        let cert = params
            .self_signed(&key_pair)
            .context("self-sign certificate")?;
        Ok(Self {
            cert_der: cert.der().to_vec(),
            key_der: key_pair.serialize_der(),
        })
    }

    /// `base64url(SHA-256(DER))` — the value the browser pins.
    pub fn hash_base64url(&self) -> String {
        let digest = Sha256::digest(&self.cert_der);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
    }

    pub fn rustls_cert(&self) -> CertificateDer<'static> {
        CertificateDer::from(self.cert_der.clone())
    }

    pub fn rustls_key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::from(PrivatePkcs8KeyDer::from(self.key_der.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn generates_cert_with_pinnable_hash() {
        let cert = GeneratedCert::new("localhost").expect("generate cert");
        let hash = cert.hash_base64url();
        assert!(!hash.is_empty(), "hash should be non-empty");
        // base64url(SHA-256(..)) decodes to exactly 32 bytes.
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(hash.as_bytes())
            .expect("hash is valid base64url");
        assert_eq!(decoded.len(), 32, "SHA-256 digest is 32 bytes");
    }
}
