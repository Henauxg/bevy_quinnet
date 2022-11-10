use std::{
    collections::HashMap,
    error::Error,
    fmt,
    fs::File,
    io::{BufRead, BufReader, Write},
    path::Path,
    sync::{Arc, Mutex},
};

use bevy::prelude::warn;
use futures::executor::block_on;
use rustls::ServerName;
use tokio::sync::{mpsc, oneshot};

use crate::QuinnetError;

use super::{InternalAsyncMessage, DEFAULT_KNOWN_HOSTS_FILE};

pub const DEFAULT_CERT_VERIFIER_BEHAVIOUR: CertVerifierBehaviour =
    CertVerifierBehaviour::ImmediateAction(CertVerifierAction::AbortConnection);

/// Event raised when a user/app interaction is needed for the server's certificate validation
pub struct CertificateInteractionEvent {
    pub status: CertVerificationStatus,
    /// Mutex for interior mutability
    pub(crate) action_sender: Mutex<Option<oneshot::Sender<CertVerifierAction>>>,
}

impl CertificateInteractionEvent {
    pub fn apply_cert_verifier_action(&self, action: CertVerifierAction) {
        let mut sender = self.action_sender.lock().unwrap();
        if let Some(sender) = sender.take() {
            sender.send(action).unwrap()
        }
    }
}

/// Event raised when a new certificate is trusted
pub struct CertificateUpdateEvent {
    pub server_name: ServName,
    pub fingerprint: CertificateFingerprint,
}

/// How the client should handle the server certificate.
#[derive(Debug, Clone)]
pub enum CertificateVerificationMode {
    /// No verification will be done on the server certificate
    SkipVerification,
    /// Client will only trust a server certificate signed by a conventional certificate authority
    SignedByCertificateAuthority,
    /// The client will look up the server identifier in [`KnownHosts`].
    /// TODO Revamp doc
    /// - If no identifier exists yet for this server, the client will accept the given server's certificate and return it.
    /// - If some certificate already existed for this server, and the received one is different, an error will be raised.
    TrustOnFirstUse(TrustOnFirstUseConfig),
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum CertVerificationStatus {
    /// First time connecting to this host.
    UnknownCertificate,
    /// The certificate fingerprint does not match the one in the known hosts fingerprints store.
    UntrustedCertificate,
    /// Known host and certificate matching the one in the known hosts fingerprints store.
    TrustedCertificate,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ServName(ServerName);

impl fmt::Display for ServName {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            ServerName::DnsName(dns) => fmt::Display::fmt(dns.as_ref(), f),
            ServerName::IpAddress(ip) => fmt::Display::fmt(&ip, f),
            _ => todo!(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TrustOnFirstUseConfig {
    pub(crate) known_hosts: KnownHosts,
    pub(crate) verifier_behaviour: HashMap<CertVerificationStatus, CertVerifierBehaviour>,
}

impl Default for TrustOnFirstUseConfig {
    fn default() -> Self {
        TrustOnFirstUseConfig {
            known_hosts: KnownHosts::HostsFile(DEFAULT_KNOWN_HOSTS_FILE.to_string()),
            verifier_behaviour: HashMap::from([
                (
                    CertVerificationStatus::UnknownCertificate,
                    CertVerifierBehaviour::ImmediateAction(CertVerifierAction::TrustAndStore),
                ),
                (
                    CertVerificationStatus::UntrustedCertificate,
                    CertVerifierBehaviour::RequestClientAction,
                ),
                (
                    CertVerificationStatus::TrustedCertificate,
                    CertVerifierBehaviour::ImmediateAction(CertVerifierAction::TrustOnce),
                ),
            ]),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum CertVerifierBehaviour {
    /// Raises an event to the client app (containing the cert info) and waits for an API call
    RequestClientAction,
    /// Take action immediately, see [`CertVerifierAction`].
    ImmediateAction(CertVerifierAction),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum CertVerifierAction {
    /// Abort the connection and raise an error (containing the cert info)
    AbortConnection,
    /// Continue the connection discarding the cert info
    TrustOnce,
    /// Continue the connection and add the cert info to the store
    TrustAndStore,
}

/// SHA-256 hash of the certificate data in DER form
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CertificateFingerprint([u8; 32]);

impl From<&rustls::Certificate> for CertificateFingerprint {
    fn from(cert: &rustls::Certificate) -> CertificateFingerprint {
        let hash = ring::digest::digest(&ring::digest::SHA256, &cert.0);
        let fingerprint_bytes = hash.as_ref().try_into().unwrap();
        CertificateFingerprint(fingerprint_bytes)
    }
}

impl fmt::Display for CertificateFingerprint {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&base64::encode(&self.0), f)
    }
}

pub type CertStore = HashMap<ServName, CertificateFingerprint>;

#[derive(Debug, Clone)]
pub enum KnownHosts {
    Store(CertStore),
    HostsFile(String),
}

/// Implementation of `ServerCertVerifier` that verifies everything as trustworthy.
pub(crate) struct SkipServerVerification;

impl SkipServerVerification {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl rustls::client::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _server_name: &rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: std::time::SystemTime,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::ServerCertVerified::assertion())
    }
}

/// Implementation of `ServerCertVerifier` that follows the Trust on first use authentication scheme.
pub(crate) struct TofuServerVerification {
    store: CertStore,
    verifier_behaviour: HashMap<CertVerificationStatus, CertVerifierBehaviour>,
    to_sync_client: mpsc::Sender<InternalAsyncMessage>,

    /// If present, the file where new fingerprints should be stored
    hosts_file: Option<String>,
}

impl TofuServerVerification {
    pub(crate) fn new(
        store: CertStore,
        verifier_behaviour: HashMap<CertVerificationStatus, CertVerifierBehaviour>,
        to_sync_client: mpsc::Sender<InternalAsyncMessage>,
        hosts_file: Option<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            verifier_behaviour,
            to_sync_client,
            hosts_file,
        })
    }

    fn apply_verifier_behaviour_for_status(
        &self,
        status: CertVerificationStatus,
        server_name: &ServName,
        fingerprint: CertificateFingerprint,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        let behaviour = self
            .verifier_behaviour
            .get(&status)
            .unwrap_or(&DEFAULT_CERT_VERIFIER_BEHAVIOUR);
        match behaviour {
            CertVerifierBehaviour::ImmediateAction(action) => {
                self.apply_verifier_immediate_action(action, server_name, fingerprint)
            }
            CertVerifierBehaviour::RequestClientAction => {
                let (action_sender, cert_action_recv) = oneshot::channel::<CertVerifierAction>();
                self.to_sync_client
                    .try_send(InternalAsyncMessage::CertificateActionRequest {
                        status,
                        action_sender,
                    })
                    .unwrap();
                match block_on(cert_action_recv) {
                    Ok(action) => {
                        self.apply_verifier_immediate_action(&action, server_name, fingerprint)
                    }
                    Err(err) => Err(rustls::Error::InvalidCertificateData(format!(
                        "Failed to receive CertVerifierAction: {}",
                        err
                    ))),
                }
            }
        }
    }

    fn apply_verifier_immediate_action(
        &self,
        action: &CertVerifierAction,
        server_name: &ServName,
        fingerprint: CertificateFingerprint,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        match action {
            CertVerifierAction::AbortConnection => Err(rustls::Error::InvalidCertificateData(
                format!("CertVerifierAction requested to abort the connection"),
            )),
            CertVerifierAction::TrustOnce => Ok(rustls::client::ServerCertVerified::assertion()),
            CertVerifierAction::TrustAndStore => {
                // If we need to store them to a file
                if let Some(file) = &self.hosts_file {
                    let mut store_clone = self.store.clone();
                    store_clone.insert(server_name.clone(), fingerprint.clone());
                    if let Err(store_error) = store_known_hosts_to_file(&file, &store_clone) {
                        return Err(rustls::Error::General(format!(
                            "Failed to store new certificate entry: {}",
                            store_error
                        )));
                    }
                }
                // In all cases raise an event containing the new certificate entry
                match self
                    .to_sync_client
                    .try_send(InternalAsyncMessage::TrustedCertificateUpdate {
                        server_name: server_name.clone(),
                        fingerprint,
                    }) {
                    Ok(_) => Ok(rustls::client::ServerCertVerified::assertion()),
                    Err(_) => Err(rustls::Error::General(format!(
                        "Failed to signal new trusted certificate entry"
                    ))),
                }
            }
        }
    }
}

impl rustls::client::ServerCertVerifier for TofuServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _server_name: &rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: std::time::SystemTime,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        // TODO Could add some optional validity checks on the cert content.
        let status;
        let fingerprint = CertificateFingerprint::from(_end_entity);
        let server_name = ServName(_server_name.clone());
        if let Some(known_fingerprint) = self.store.get(&server_name) {
            if *known_fingerprint == fingerprint {
                status = Some(CertVerificationStatus::TrustedCertificate);
            } else {
                status = Some(CertVerificationStatus::UntrustedCertificate);
            }
        } else {
            status = Some(CertVerificationStatus::UnknownCertificate);
        }
        match status {
            Some(status) => {
                self.apply_verifier_behaviour_for_status(status, &server_name, fingerprint)
            }
            None => Err(rustls::Error::InvalidCertificateData(format!(
                "Internal error, no CertVerificationStatus"
            ))),
        }
    }
}

fn store_known_hosts_to_file(file: &String, store: &CertStore) -> Result<(), Box<dyn Error>> {
    let path = std::path::Path::new(file);
    let prefix = path.parent().unwrap();
    std::fs::create_dir_all(prefix)?;
    let mut store_file = File::create(path)?;
    for entry in store {
        writeln!(store_file, "{} {}", entry.0, entry.1)?;
    }
    Ok(())
}

fn parse_known_host_line(
    line: String,
) -> Result<(ServName, CertificateFingerprint), Box<dyn Error>> {
    let mut parts = line.split_whitespace();

    let adr_str = parts.next().ok_or(QuinnetError::InvalidHostFile)?;
    let serv_name = ServName(ServerName::try_from(adr_str)?);

    let fingerprint_b64 = parts.next().ok_or(QuinnetError::InvalidHostFile)?;
    let fingerprint_bytes = base64::decode(&fingerprint_b64)?;

    match fingerprint_bytes.try_into() {
        Ok(buf) => Ok((serv_name, CertificateFingerprint(buf))),
        Err(_) => Err(Box::new(QuinnetError::InvalidHostFile)),
    }
}

fn load_known_hosts_from_file(
    file_path: String,
) -> Result<(CertStore, Option<String>), Box<dyn Error>> {
    let mut store = HashMap::new();
    for line in BufReader::new(File::open(&file_path)?).lines() {
        let entry = parse_known_host_line(line?)?;
        store.insert(entry.0, entry.1);
    }
    Ok((store, Some(file_path)))
}

pub(crate) fn load_known_hosts_store_from_config(
    known_host_config: KnownHosts,
) -> Result<(CertStore, Option<String>), Box<dyn Error>> {
    match known_host_config {
        KnownHosts::Store(store) => Ok((store, None)),
        KnownHosts::HostsFile(file) => {
            if !Path::new(&file).exists() {
                warn!(
                    "Known hosts file `{}` not found, no known hosts loaded",
                    file
                );
                Ok((HashMap::new(), Some(file)))
            } else {
                load_known_hosts_from_file(file)
            }
        }
    }
}
