use std::fmt;

/// SHA-256 hash of the certificate data in DER form
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CertificateFingerprint([u8; 32]);

impl CertificateFingerprint {
    /// Wraps a buffer into a [`CertificateFingerprint`]
    pub fn new(buf: [u8; 32]) -> Self {
        CertificateFingerprint(buf)
    }

    /// Encodes the wrapped buffer content to base64
    pub fn to_base64(&self) -> String {
        base64::encode(&self.0)
    }
}

impl From<&rustls::pki_types::CertificateDer<'_>> for CertificateFingerprint {
    fn from(cert: &rustls::pki_types::CertificateDer<'_>) -> CertificateFingerprint {
        let hash = ring::digest::digest(&ring::digest::SHA256, &cert);
        let fingerprint_bytes = hash.as_ref().try_into().unwrap();
        CertificateFingerprint(fingerprint_bytes)
    }
}

impl fmt::Display for CertificateFingerprint {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.to_base64(), f)
    }
}
