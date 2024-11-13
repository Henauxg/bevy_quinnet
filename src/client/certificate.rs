use std::{
    collections::HashMap,
    error::Error,
    fmt,
    fs::File,
    io::{BufRead, BufReader, Write},
    path::Path,
    sync::{Arc, Mutex},
};

use bevy::{prelude::Event, utils::tracing::warn};
use futures::executor::block_on;
use rustls::pki_types::{CertificateDer, ServerName as RustlsServerName, UnixTime};
use tokio::sync::{mpsc, oneshot};

use crate::shared::{certificate::CertificateFingerprint, error::QuinnetError};

use super::{ClientAsyncMessage, ConnectionLocalId, DEFAULT_KNOWN_HOSTS_FILE};

/// Default certificate behavior is to abort the connection
pub const DEFAULT_CERT_VERIFIER_BEHAVIOUR: CertVerifierBehaviour =
    CertVerifierBehaviour::ImmediateAction(CertVerifierAction::AbortConnection);

/// Event raised when a user/app interaction is needed for the server's certificate validation
#[derive(Event)]
pub struct CertInteractionEvent {
    /// Local id of the concerned connection
    pub connection_id: ConnectionLocalId,
    /// The current status of the verification
    pub status: CertVerificationStatus,
    /// Server & Certificate info
    pub info: CertVerificationInfo,
    /// Mutex for interior mutability
    pub(crate) action_sender: Mutex<Option<oneshot::Sender<CertVerifierAction>>>,
}

impl CertInteractionEvent {
    /// Handler to call on a [`CertInteractionEvent`] to apply a specific [`CertVerifierAction`]
    pub fn apply_cert_verifier_action(
        &self,
        action: CertVerifierAction,
    ) -> Result<(), QuinnetError> {
        let mut sender = self.action_sender.lock()?;
        if let Some(sender) = sender.take() {
            match sender.send(action) {
                Ok(_) => Ok(()),
                Err(_) => Err(QuinnetError::InternalChannelClosed),
            }
        } else {
            Err(QuinnetError::CertificateActionAlreadyApplied)
        }
    }
}

/// Event raised when a new certificate is trusted
#[derive(Event)]
pub struct CertTrustUpdateEvent {
    /// Local id of the concerned connection
    pub connection_id: ConnectionLocalId,
    /// Info about the newly trusted certificate
    pub cert_info: CertVerificationInfo,
}

/// Event raised when a connection is aborted during the certificate verification
#[derive(Event)]
pub struct CertConnectionAbortEvent {
    /// Local id of the concerned connection
    pub connection_id: ConnectionLocalId,
    /// The status of the certificate verification before aborting the connection
    pub status: CertVerificationStatus,
    /// Info about the certificate
    pub cert_info: CertVerificationInfo,
}

/// How the client should handle the server certificate.
#[derive(Debug, Clone)]
pub enum CertificateVerificationMode {
    /// No verification will be done on the server certificate
    SkipVerification,
    /// Client will only trust a server certificate signed by a conventional certificate authority
    SignedByCertificateAuthority,
    /// The client will use a Trust on first authentication scheme (<https://en.wikipedia.org/wiki/Trust_on_first_use>) configured by a [`TrustOnFirstUseConfig`].
    TrustOnFirstUse(TrustOnFirstUseConfig),
}

/// Configuration of the Trust on first authentication scheme for server certificates
///
/// # Example
///
/// ```
/// use bevy_quinnet::client::certificate::TrustOnFirstUseConfig;
/// let config = TrustOnFirstUseConfig {
///     known_hosts: bevy_quinnet::client::certificate::KnownHosts::HostsFile(
///         "my_own_hosts_file".to_string(),
///     ),
///     ..Default::default()
/// };
/// ```
#[derive(Debug, Clone)]
pub struct TrustOnFirstUseConfig {
    /// known_hosts stores all the already known and trusted endpoints
    pub known_hosts: KnownHosts,
    /// verifier_behaviour stores the [`CertVerifierBehaviour`] that the certificate verifier will adopt for each possible [`CertVerificationStatus`]
    pub verifier_behaviour: HashMap<CertVerificationStatus, CertVerifierBehaviour>,
}

impl Default for TrustOnFirstUseConfig {
    /// Returns the default [`TrustOnFirstUseConfig`]
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

/// Status of the server's certificate verification.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum CertVerificationStatus {
    /// First time connecting to this host.
    UnknownCertificate,
    /// The certificate fingerprint does not match the one in the known hosts fingerprints store.
    UntrustedCertificate,
    /// This is a known host and the certificate is matching the one in the known hosts fingerprints store.
    TrustedCertificate,
}

/// Info onthe server's certificate.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CertVerificationInfo {
    /// Name of the server
    pub server_name: ServerName,
    /// Fingerprint of the received certificate
    pub fingerprint: CertificateFingerprint,
    /// If any, previously knwon fingerprint for this server
    pub known_fingerprint: Option<CertificateFingerprint>,
}

/// Encodes ways a client can know the expected name of the server. See [`rustls::pki_types::ServerName`]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ServerName(RustlsServerName<'static>);

impl fmt::Display for ServerName {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.to_str(), f)
    }
}

/// Describes how a certificate verifier should act when facing a certain [`CertVerificationStatus`]
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum CertVerifierBehaviour {
    /// Raises an event to the client app (containing the cert info) and waits for an API call
    RequestClientAction,
    /// Take action immediately, see [`CertVerifierAction`].
    ImmediateAction(CertVerifierAction),
}

/// Possible actions that a certificate verifier or a user can take for a certifcate & connection
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum CertVerifierAction {
    /// Abort the connection and raise an error (containing the cert info)
    AbortConnection,
    /// Accept the server's certificate and continue the connection, but discard the certificate's info. They will not be stored nor available as an event.
    TrustOnce,
    /// Accept the server's certificate and continue the connection. A [`CertTrustUpdateEvent`] will be raised containing the certificate's info.
    /// If the certificate store ([`KnownHosts`]) is a file, this action also adds the certificate's info to the store file. Else it is up to the user to update its own store with the content of [`CertTrustUpdateEvent`].
    TrustAndStore,
}

/// Certificate fingerprint storage
pub type CertStore = HashMap<ServerName, CertificateFingerprint>;

/// Certificate fingerprint storage as a value or as a file
#[derive(Debug, Clone)]
pub enum KnownHosts {
    /// Directly contains the server name to fingerprint mapping
    Store(CertStore),
    /// Path of a file caontaing the server name to fingerprint mapping.
    HostsFile(String), // TODO More on the file format + the limitations
}

/// Implementation of `ServerCertVerifier` that verifies everything as trustworthy.
#[derive(Debug)]
pub(crate) struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl SkipServerVerification {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self(Arc::new(rustls::crypto::ring::default_provider())))
    }
}

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &RustlsServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Implementation of `ServerCertVerifier` that follows the Trust on first use authentication scheme.
#[derive(Debug)]
pub(crate) struct TofuServerVerification {
    store: CertStore,
    verifier_behaviour: HashMap<CertVerificationStatus, CertVerifierBehaviour>,
    to_sync_client: mpsc::Sender<ClientAsyncMessage>,

    /// If present, the file where new fingerprints should be stored
    hosts_file: Option<String>,

    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl TofuServerVerification {
    pub(crate) fn new(
        store: CertStore,
        verifier_behaviour: HashMap<CertVerificationStatus, CertVerifierBehaviour>,
        to_sync_client: mpsc::Sender<ClientAsyncMessage>,
        hosts_file: Option<String>,
        provider: Arc<rustls::crypto::CryptoProvider>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            verifier_behaviour,
            to_sync_client,
            hosts_file,
            provider,
        })
    }

    fn apply_verifier_behaviour_for_status(
        &self,
        status: CertVerificationStatus,
        cert_info: CertVerificationInfo,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let behaviour = self
            .verifier_behaviour
            .get(&status)
            .unwrap_or(&DEFAULT_CERT_VERIFIER_BEHAVIOUR);
        match behaviour {
            CertVerifierBehaviour::ImmediateAction(action) => {
                self.apply_verifier_immediate_action(action, status, cert_info)
            }
            CertVerifierBehaviour::RequestClientAction => {
                let (action_sender, cert_action_recv) = oneshot::channel::<CertVerifierAction>();
                self.to_sync_client
                    .try_send(ClientAsyncMessage::CertificateInteractionRequest {
                        status: status.clone(),
                        info: cert_info.clone(),
                        action_sender,
                    })
                    .unwrap();
                match block_on(cert_action_recv) {
                    Ok(action) => self.apply_verifier_immediate_action(&action, status, cert_info),
                    Err(err) => Err(rustls::Error::General(format!(
                        "Failed to receive CertVerifierAction from client: {}",
                        err
                    ))),
                }
            }
        }
    }

    fn apply_verifier_immediate_action(
        &self,
        action: &CertVerifierAction,
        status: CertVerificationStatus,
        cert_info: CertVerificationInfo,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        match action {
            CertVerifierAction::AbortConnection => {
                match self
                    .to_sync_client
                    .try_send(ClientAsyncMessage::CertificateConnectionAbort {
                        status: status,
                        cert_info,
                    }) {
                    Ok(_) => Err(rustls::Error::General(format!(
                        "CertVerifierAction requested to abort the connection"
                    ))),
                    Err(_) => Err(rustls::Error::General(format!(
                        "Failed to signal CertificateConnectionAbort"
                    ))),
                }
            }
            CertVerifierAction::TrustOnce => {
                Ok(rustls::client::danger::ServerCertVerified::assertion())
            }
            CertVerifierAction::TrustAndStore => {
                // If we need to store them to a file
                if let Some(file) = &self.hosts_file {
                    let mut store_clone = self.store.clone();
                    store_clone
                        .insert(cert_info.server_name.clone(), cert_info.fingerprint.clone());
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
                    .try_send(ClientAsyncMessage::CertificateTrustUpdate(cert_info))
                {
                    Ok(_) => Ok(rustls::client::danger::ServerCertVerified::assertion()),
                    Err(_) => Err(rustls::Error::General(format!(
                        "Failed to signal new trusted certificate entry"
                    ))),
                }
            }
        }
    }
}

impl rustls::client::danger::ServerCertVerifier for TofuServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &RustlsServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        // TODO Could add some optional validity checks on the cert content.
        let status;
        let server_name = ServerName(_server_name.to_owned());
        let known_fingerprint = self.store.get(&server_name).cloned();
        let cert_info = CertVerificationInfo {
            server_name,
            fingerprint: CertificateFingerprint::from(_end_entity),
            known_fingerprint,
        };
        if let Some(ref known_fingerprint) = cert_info.known_fingerprint {
            if *known_fingerprint == cert_info.fingerprint {
                status = CertVerificationStatus::TrustedCertificate;
            } else {
                status = CertVerificationStatus::UntrustedCertificate;
            }
        } else {
            status = CertVerificationStatus::UnknownCertificate;
        }
        self.apply_verifier_behaviour_for_status(status, cert_info)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn store_known_hosts_to_file(file: &String, store: &CertStore) -> Result<(), Box<dyn Error>> {
    let path = std::path::Path::new(file);
    if let Some(prefix) = path.parent() {
        std::fs::create_dir_all(prefix)?;
    }
    let mut store_file = File::create(path)?;
    for entry in store {
        writeln!(store_file, "{} {}", entry.0, entry.1)?;
    }
    Ok(())
}

fn parse_known_host_line(
    line: String,
) -> Result<(ServerName, CertificateFingerprint), Box<dyn Error>> {
    let mut parts = line.split_whitespace();

    let adr_str = parts.next().ok_or(QuinnetError::InvalidHostFile)?;
    let serv_name = ServerName(RustlsServerName::try_from(adr_str)?.to_owned());

    let fingerprint_b64 = parts.next().ok_or(QuinnetError::InvalidHostFile)?;
    let fingerprint_bytes = base64::decode(&fingerprint_b64)?;

    match fingerprint_bytes.try_into() {
        Ok(buf) => Ok((serv_name, CertificateFingerprint::new(buf))),
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
