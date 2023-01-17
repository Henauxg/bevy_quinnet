use std::{
    fs::{self, File},
    io::BufReader,
    path::Path,
};

use bevy::prelude::{trace, warn};

use crate::shared::{CertificateFingerprint, QuinnetError};

/// Represents the origin of a certificate.
#[derive(Debug, Clone)]
pub enum CertOrigin {
    /// Indicates that the certificate was generated. The `server_host` field contains the hostname used when generating the certificate.
    Generated { server_host: String },
    /// Indicates that the certificate was loaded from a file.
    Loaded,
}

/// How the server should retrieve its certificate.
#[derive(Debug, Clone)]
pub enum CertificateRetrievalMode {
    /// The server will always generate a new self-signed certificate when starting up
    GenerateSelfSigned,
    /// Try to load cert & key from files.
    LoadFromFile { cert_file: String, key_file: String },
    /// Try to load cert & key from files.
    /// If the files do not exist, generate a self-signed certificate, and optionally save it to disk.
    LoadFromFileOrGenerateSelfSigned {
        cert_file: String,
        key_file: String,
        save_on_disk: bool,
    },
}

/// Represents a server certificate.
pub struct ServerCertificate {
    /// A vector of [rustls::Certificate] that contains the server's certificate chain.
    pub cert_chain: Vec<rustls::Certificate>,
    /// The server's private key, represented by a [rustls::PrivateKey] struct.
    pub priv_key: rustls::PrivateKey,
    /// The fingerprint of the server's main certificate, represented by a [CertificateFingerprint] struct.
    pub fingerprint: CertificateFingerprint,
}

fn read_certs_from_files(
    cert_file: &String,
    key_file: &String,
) -> Result<ServerCertificate, QuinnetError> {
    let mut cert_chain_reader = BufReader::new(File::open(cert_file)?);
    let cert_chain: Vec<rustls::Certificate> = rustls_pemfile::certs(&mut cert_chain_reader)?
        .into_iter()
        .map(rustls::Certificate)
        .collect();

    let mut key_reader = BufReader::new(File::open(key_file)?);
    let mut keys = rustls_pemfile::pkcs8_private_keys(&mut key_reader)?;

    assert_eq!(keys.len(), 1);
    let priv_key = rustls::PrivateKey(keys.remove(0));

    assert!(cert_chain.len() >= 1);
    let fingerprint = CertificateFingerprint::from(&cert_chain[0]);

    Ok(ServerCertificate {
        cert_chain,
        priv_key,
        fingerprint,
    })
}

fn write_certs_to_files(
    cert: &rcgen::Certificate,
    cert_file: &String,
    key_file: &String,
) -> Result<(), QuinnetError> {
    let pem_cert = cert.serialize_pem()?;
    let pem_key = cert.serialize_private_key_pem();

    for file in vec![cert_file, key_file] {
        let path = std::path::Path::new(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }

    fs::write(cert_file, pem_cert)?;
    fs::write(key_file, pem_key)?;

    Ok(())
}

fn generate_self_signed_certificate(
    server_host: &String,
) -> Result<(ServerCertificate, rcgen::Certificate), QuinnetError> {
    let cert = rcgen::generate_simple_self_signed(vec![server_host.into()])?;
    let cert_der = cert.serialize_der()?;
    let priv_key = rustls::PrivateKey(cert.serialize_private_key_der());
    let rustls_cert = rustls::Certificate(cert_der.clone());
    let fingerprint = CertificateFingerprint::from(&rustls_cert);
    let cert_chain = vec![rustls_cert];

    Ok((
        ServerCertificate {
            cert_chain,
            priv_key,
            fingerprint,
        },
        cert,
    ))
}

pub(crate) fn retrieve_certificate(
    server_host: &String,
    cert_mode: CertificateRetrievalMode,
) -> Result<ServerCertificate, QuinnetError> {
    match cert_mode {
        CertificateRetrievalMode::GenerateSelfSigned => {
            let (server_cert, _rcgen_cert) = generate_self_signed_certificate(server_host)?;
            trace!("Generatied a new self-signed certificate");
            Ok(server_cert)
        }
        CertificateRetrievalMode::LoadFromFile {
            cert_file,
            key_file,
        } => {
            let server_cert = read_certs_from_files(&cert_file, &key_file)?;
            trace!("Successfuly loaded cert and key from files");
            Ok(server_cert)
        }
        CertificateRetrievalMode::LoadFromFileOrGenerateSelfSigned {
            save_on_disk,
            cert_file,
            key_file,
        } => {
            if Path::new(&cert_file).exists() && Path::new(&key_file).exists() {
                let server_cert = read_certs_from_files(&cert_file, &key_file)?;
                trace!("Successfuly loaded cert and key from files");
                Ok(server_cert)
            } else {
                warn!("{} and/or {} do not exist, could not load existing certificate. Generating a new self-signed certificate.", cert_file, key_file);
                let (server_cert, rcgen_cert) = generate_self_signed_certificate(server_host)?;
                if save_on_disk {
                    write_certs_to_files(&rcgen_cert, &cert_file, &key_file)?;
                    trace!("Successfuly saved cert and key to files");
                }
                Ok(server_cert)
            }
        }
    }
}
