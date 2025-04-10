use std::{
    fs::{self, File},
    io::BufReader,
    path::Path,
};

use bevy::log::{trace, warn};

use crate::shared::{certificate::CertificateFingerprint, error::QuinnetError};

/// Represents the origin of a certificate.
#[derive(Debug, Clone)]
pub enum CertOrigin {
    /// Indicates that the certificate was generated.
    Generated {
        /// Contains the hostname used when generating the certificate.
        server_hostname: String,
    },
    /// Indicates that the certificate was loaded from a file.
    Loaded,
}

/// How the server should retrieve its certificate.
#[derive(Debug, Clone)]
pub enum CertificateRetrievalMode {
    /// The server will always generate a new self-signed certificate when starting up,
    GenerateSelfSigned {
        /// Used as the subject of the certificate.
        server_hostname: String,
    },
    /// Try to load cert & key from files `cert_file``and `key_file`.
    LoadFromFile {
        /// Path of the file containing the certificate in PEM form
        cert_file: String,
        /// Path of the file containing the private key in PEM form
        key_file: String,
    },
    /// Try to load cert & key from files `cert_file``and `key_file`.
    /// If the files do not exist, generates a self-signed certificate.
    LoadFromFileOrGenerateSelfSigned {
        /// Path of the file containing the certificate in PEM form
        cert_file: String,
        /// Path of the file containing the private key in PEM form
        key_file: String,
        /// Saves the generated certificate to disk if enabled.
        save_on_disk: bool,
        /// Used as the subject of the certificate.
        server_hostname: String,
    },
}

/// Represents a server certificate.
pub struct ServerCertificate {
    /// The server's certificate chain.
    pub cert_chain: Vec<rustls::pki_types::CertificateDer<'static>>,
    /// The server's private key.
    pub priv_key: rustls::pki_types::PrivateKeyDer<'static>,
    /// The fingerprint of the server's main certificate (first in the chain)
    pub fingerprint: CertificateFingerprint,
}

fn read_cert_from_files(
    cert_file: &String,
    key_file: &String,
) -> Result<ServerCertificate, QuinnetError> {
    let mut cert_chain_reader = BufReader::new(File::open(cert_file)?);
    let cert_chain: Vec<rustls::pki_types::CertificateDer> =
        rustls_pemfile::certs(&mut cert_chain_reader).collect::<Result<_, _>>()?;

    assert!(cert_chain.len() >= 1);

    let mut key_reader = BufReader::new(File::open(key_file)?);
    let priv_key = rustls_pemfile::private_key(&mut key_reader)?.expect("private key is present");
    let fingerprint = CertificateFingerprint::from(&cert_chain[0]);

    Ok(ServerCertificate {
        cert_chain,
        priv_key,
        fingerprint,
    })
}

fn write_cert_to_files(
    cert: &rcgen::CertifiedKey,
    cert_file: &String,
    key_file: &String,
) -> Result<(), QuinnetError> {
    for file in vec![cert_file, key_file] {
        if let Some(parent) = std::path::Path::new(file).parent() {
            std::fs::create_dir_all(parent)?;
        }
    }

    fs::write(cert_file, cert.cert.pem())?;
    fs::write(key_file, cert.key_pair.serialize_pem())?;

    Ok(())
}

fn generate_self_signed_certificate(
    server_host: &String,
) -> Result<(ServerCertificate, rcgen::CertifiedKey), QuinnetError> {
    let generated = rcgen::generate_simple_self_signed(vec![server_host.into()])?;

    let priv_key_der =
        rustls::pki_types::PrivatePkcs8KeyDer::from(generated.key_pair.serialize_der()).into();
    let cert_der = generated.cert.der();
    let fingerprint = CertificateFingerprint::from(cert_der);

    Ok((
        ServerCertificate {
            cert_chain: vec![cert_der.clone()],
            priv_key: priv_key_der,
            fingerprint,
        },
        generated,
    ))
}

pub(crate) fn retrieve_certificate(
    cert_mode: CertificateRetrievalMode,
) -> Result<ServerCertificate, QuinnetError> {
    match cert_mode {
        CertificateRetrievalMode::GenerateSelfSigned { server_hostname } => {
            let (server_cert, _rcgen_cert) = generate_self_signed_certificate(&server_hostname)?;
            trace!("Generatied a new self-signed certificate");
            Ok(server_cert)
        }
        CertificateRetrievalMode::LoadFromFile {
            cert_file,
            key_file,
        } => {
            let server_cert = read_cert_from_files(&cert_file, &key_file)?;
            trace!("Successfuly loaded cert and key from files");
            Ok(server_cert)
        }
        CertificateRetrievalMode::LoadFromFileOrGenerateSelfSigned {
            save_on_disk,
            cert_file,
            key_file,
            server_hostname,
        } => {
            if Path::new(&cert_file).exists() && Path::new(&key_file).exists() {
                let server_cert = read_cert_from_files(&cert_file, &key_file)?;
                trace!("Successfuly loaded cert and key from files");
                Ok(server_cert)
            } else {
                warn!("{} and/or {} do not exist, could not load existing certificate. Generating a new self-signed certificate.", cert_file, key_file);
                let (server_cert, rcgen_cert) = generate_self_signed_certificate(&server_hostname)?;
                if save_on_disk {
                    write_cert_to_files(&rcgen_cert, &cert_file, &key_file)?;
                    trace!("Successfuly saved cert and key to files");
                }
                Ok(server_cert)
            }
        }
    }
}
