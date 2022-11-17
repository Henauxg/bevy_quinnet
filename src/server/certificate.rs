use std::{
    error::Error,
    fs::{self, File},
    io::BufReader,
    path::Path,
};

use bevy::prelude::{trace, warn};

use crate::shared::CertificateFingerprint;

/// Event raised when a certificate is retrieved on the server
#[derive(Debug, Clone)]
pub struct CertificateRetrievedEvent {
    pub fingerprint: CertificateFingerprint,
}

#[derive(Debug, Clone)]
pub enum CertOrigin {
    Generated { server_host: String },
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

fn read_certs_from_files(
    cert_file: &String,
    key_file: &String,
) -> Result<
    (
        Vec<rustls::Certificate>,
        rustls::PrivateKey,
        CertificateFingerprint,
    ),
    Box<dyn Error>,
> {
    let mut cert_chain_reader = BufReader::new(File::open(cert_file)?);
    let certs: Vec<rustls::Certificate> = rustls_pemfile::certs(&mut cert_chain_reader)?
        .into_iter()
        .map(rustls::Certificate)
        .collect();

    let mut key_reader = BufReader::new(File::open(key_file)?);
    let mut keys = rustls_pemfile::pkcs8_private_keys(&mut key_reader)?;

    assert_eq!(keys.len(), 1);
    let key = rustls::PrivateKey(keys.remove(0));

    assert!(certs.len() >= 1);
    let fingerprint = CertificateFingerprint::from(&certs[0]);

    Ok((certs, key, fingerprint))
}

fn write_certs_to_files(
    cert: &rcgen::Certificate,
    cert_file: &String,
    key_file: &String,
) -> Result<(), Box<dyn Error>> {
    let pem_cert = cert.serialize_pem()?;
    let pem_key = cert.serialize_private_key_pem();

    fs::write(cert_file, pem_cert)?;
    fs::write(key_file, pem_key)?;

    Ok(())
}

fn generate_self_signed_certificate(
    server_host: &String,
) -> Result<
    (
        Vec<rustls::Certificate>,
        rustls::PrivateKey,
        rcgen::Certificate,
        CertificateFingerprint,
    ),
    Box<dyn Error>,
> {
    let cert = rcgen::generate_simple_self_signed(vec![server_host.into()]).unwrap();
    let cert_der = cert.serialize_der().unwrap();
    let priv_key = rustls::PrivateKey(cert.serialize_private_key_der());
    let rustls_cert = rustls::Certificate(cert_der.clone());
    let fingerprint = CertificateFingerprint::from(&rustls_cert);
    let cert_chain = vec![rustls_cert];

    Ok((cert_chain, priv_key, cert, fingerprint))
}

pub(crate) fn retrieve_certificate(
    server_host: &String,
    cert_mode: CertificateRetrievalMode,
) -> Result<
    (
        Vec<rustls::Certificate>,
        rustls::PrivateKey,
        CertificateFingerprint,
    ),
    Box<dyn Error>,
> {
    match cert_mode {
        CertificateRetrievalMode::GenerateSelfSigned => {
            trace!("Generating a new self-signed certificate");
            match generate_self_signed_certificate(server_host) {
                Ok((cert_chain, priv_key, _rcgen_cert, fingerprint)) => {
                    Ok((cert_chain, priv_key, fingerprint))
                }
                Err(e) => Err(e),
            }
        }
        CertificateRetrievalMode::LoadFromFile {
            cert_file,
            key_file,
        } => match read_certs_from_files(&cert_file, &key_file) {
            Ok((cert_chain, priv_key, fingerprint)) => {
                trace!("Successfuly loaded cert and key from files");
                Ok((cert_chain, priv_key, fingerprint))
            }
            Err(e) => Err(e),
        },
        CertificateRetrievalMode::LoadFromFileOrGenerateSelfSigned {
            save_on_disk,
            cert_file,
            key_file,
        } => {
            if Path::new(&cert_file).exists() && Path::new(&key_file).exists() {
                match read_certs_from_files(&cert_file, &key_file) {
                    Ok((cert_chain, priv_key, fingerprint)) => {
                        trace!("Successfuly loaded cert and key from files");
                        Ok((cert_chain, priv_key, fingerprint))
                    }
                    Err(e) => Err(e),
                }
            } else {
                warn!("{} and/or {} do not exist, could not load existing certificate. Generating a self-signed one.", cert_file, key_file);
                match generate_self_signed_certificate(server_host) {
                    Ok((cert_chain, priv_key, rcgen_cert, fingerprint)) => {
                        if save_on_disk {
                            write_certs_to_files(&rcgen_cert, &cert_file, &key_file)?;
                            trace!("Successfuly saved cert and key to files");
                        }
                        Ok((cert_chain, priv_key, fingerprint))
                    }
                    Err(e) => Err(e),
                }
            }
        }
    }
}
