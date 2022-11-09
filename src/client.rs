use std::{
    collections::HashMap,
    error::Error,
    fs::File,
    io::{BufRead, BufReader},
    net::SocketAddr,
    sync::Arc,
};

use bevy::prelude::*;
use bytes::Bytes;
use futures::{channel::oneshot, executor::block_on, sink::SinkExt};
use futures_util::StreamExt;
use quinn::{ClientConfig, Endpoint};
use rustls::Certificate;
use serde::Deserialize;
use tokio::{
    runtime::Runtime,
    sync::{
        broadcast,
        mpsc::{
            self,
            error::{TryRecvError, TrySendError},
        },
    },
    task::JoinSet,
};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

use crate::{QuinnetError, DEFAULT_KILL_MESSAGE_QUEUE_SIZE, DEFAULT_MESSAGE_QUEUE_SIZE};

pub const DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE: usize = 100;
pub const DEFAULT_KNOWN_HOSTS_FILE: &str = "quinnet/known_hosts";
pub const DEFAULT_CERT_VERIFIER_BEHAVIOUR: CertVerifierBehaviour =
    CertVerifierBehaviour::ImmediateAction(CertVerifierAction::AbortConnection);

/// Connection event raised when the client just connected to the server. Raised in the CoreStage::PreUpdate stage.
pub struct ConnectionEvent;
/// ConnectionLost event raised when the client is considered disconnected from the server. Raised in the CoreStage::PreUpdate stage.
pub struct ConnectionLostEvent;

/// Event raised when a user/app action is needed for the server's cerrtificate validation.
pub struct TofuCertificateEvent {
    pub status: CertVerificationStatus,
    pub action_sender: oneshot::Sender<CertVerifierAction>,
}

/// Configuration of the client, used when connecting to a server
#[derive(Debug, Deserialize, Clone)]
pub struct ClientConfigurationData {
    server_host: String,
    server_port: u16,
    local_bind_host: String,
    local_bind_port: u16,
}

impl ClientConfigurationData {
    /// Creates a new ClientConfigurationData
    ///
    /// # Arguments
    ///
    /// * `server_host` - Address of the server
    /// * `server_port` - Port that the server is listening on
    /// * `local_bind_host` - Local address to bind to, which should usually be a wildcard address like `0.0.0.0` or `[::]`, which allow communication with any reachable IPv4 or IPv6 address. See [`quinn::endpoint::Endpoint`] for more precision
    /// * `local_bind_port` - Local port to bind to. Use 0 to get an OS-assigned port.. See [`quinn::endpoint::Endpoint`] for more precision
    ///
    /// # Examples
    ///
    /// ```
    /// let config = ClientConfigurationData::new(
    ///         "127.0.0.1".to_string(),
    ///         6000,
    ///         "0.0.0.0".to_string(),
    ///         0,
    ///     );
    /// ```
    pub fn new(
        server_host: String,
        server_port: u16,
        local_bind_host: String,
        local_bind_port: u16,
    ) -> Self {
        Self {
            server_host,
            server_port,
            local_bind_host,
            local_bind_port,
        }
    }
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

#[derive(Debug, Clone)]
pub struct TrustOnFirstUseConfig {
    known_hosts: KnownHosts,
    verifier_behaviour: HashMap<CertVerificationStatus, CertVerifierBehaviour>,
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

// pub enum CertVerificationStatus {}

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

/// Current state of the client driver
#[derive(Debug, PartialEq, Eq)]
enum ClientState {
    Disconnected,
    Connected,
}

#[derive(Debug)]
pub(crate) enum InternalAsyncMessage {
    Connected(Option<Vec<Certificate>>),
    LostConnection,
    CertificateActionRequest {
        status: CertVerificationStatus,
        action_sender: oneshot::Sender<CertVerifierAction>,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum InternalSyncMessage {
    Connect {
        config: ClientConfigurationData,
        cert_mode: CertificateVerificationMode,
    },
}

pub struct Client {
    state: ClientState,
    // TODO Perf: multiple channels
    sender: mpsc::Sender<Bytes>,
    receiver: mpsc::Receiver<Bytes>,
    close_sender: broadcast::Sender<()>,

    pub(crate) internal_receiver: mpsc::Receiver<InternalAsyncMessage>,
    pub(crate) internal_sender: mpsc::Sender<InternalSyncMessage>,
}

impl Client {
    /// Connect to a server with the given [ClientConfigurationData] and [CertificateVerificationMode]
    pub fn connect(
        &self,
        config: ClientConfigurationData,
        cert_mode: CertificateVerificationMode,
    ) -> Result<(), QuinnetError> {
        match self
            .internal_sender
            .try_send(InternalSyncMessage::Connect { config, cert_mode })
        {
            Ok(_) => Ok(()),
            Err(_) => Err(QuinnetError::FullQueue),
        }
    }

    /// Disconnect the client. This does not send any message to the server, and simply closes all the connection tasks locally.
    pub fn disconnect(&mut self) -> Result<(), QuinnetError> {
        if self.is_connected() {
            if let Err(_) = self.close_sender.send(()) {
                return Err(QuinnetError::ChannelClosed);
            }
        }
        self.state = ClientState::Disconnected;
        Ok(())
    }

    pub fn receive_message<T: serde::de::DeserializeOwned>(
        &mut self,
    ) -> Result<Option<T>, QuinnetError> {
        match self.receive_payload()? {
            Some(payload) => match bincode::deserialize(&payload) {
                Ok(msg) => Ok(Some(msg)),
                Err(_) => Err(QuinnetError::Deserialization),
            },
            None => Ok(None),
        }
    }

    pub fn send_message<T: serde::Serialize>(&self, message: T) -> Result<(), QuinnetError> {
        match bincode::serialize(&message) {
            Ok(payload) => self.send_payload(payload),
            Err(_) => Err(QuinnetError::Serialization),
        }
    }

    pub fn send_payload<T: Into<Bytes>>(&self, payload: T) -> Result<(), QuinnetError> {
        match self.sender.try_send(payload.into()) {
            Ok(_) => Ok(()),
            Err(err) => match err {
                TrySendError::Full(_) => Err(QuinnetError::FullQueue),
                TrySendError::Closed(_) => Err(QuinnetError::ChannelClosed),
            },
        }
    }

    pub fn receive_payload(&mut self) -> Result<Option<Bytes>, QuinnetError> {
        match self.receiver.try_recv() {
            Ok(msg_payload) => Ok(Some(msg_payload)),
            Err(err) => match err {
                TryRecvError::Empty => Ok(None),
                TryRecvError::Disconnected => Err(QuinnetError::ChannelClosed),
            },
        }
    }

    pub fn is_connected(&self) -> bool {
        return self.state == ClientState::Connected;
    }
}

/// Implementation of `ServerCertVerifier` that verifies everything as trustworthy.
struct SkipServerVerification;

impl SkipServerVerification {
    fn new() -> Arc<Self> {
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

#[derive(Debug, Clone)]
pub enum KnownHosts {
    Store(HashMap<rustls::ServerName, CertificateFingerprint>),
    HostsFile(String),
}

/// Implementation of `ServerCertVerifier` that follows the Trust on first use authentication scheme.
struct TofuServerVerification {
    store: HashMap<rustls::ServerName, CertificateFingerprint>,
    verifier_behaviour: HashMap<CertVerificationStatus, CertVerifierBehaviour>,
    to_sync_client: mpsc::Sender<InternalAsyncMessage>,
}

impl TofuServerVerification {
    fn new(
        store: HashMap<rustls::ServerName, CertificateFingerprint>,
        verifier_behaviour: HashMap<CertVerificationStatus, CertVerifierBehaviour>,
        to_sync_client: mpsc::Sender<InternalAsyncMessage>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            verifier_behaviour,
            to_sync_client,
        })
    }

    fn apply_verifier_behaviour_for_status(
        &self,
        status: CertVerificationStatus,
        fingerprint: CertificateFingerprint,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        let behaviour = self
            .verifier_behaviour
            .get(&status)
            .unwrap_or(&DEFAULT_CERT_VERIFIER_BEHAVIOUR);
        match behaviour {
            CertVerifierBehaviour::ImmediateAction(action) => {
                TofuServerVerification::apply_verifier_immediate_action(action, fingerprint)
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
                    Ok(action) => TofuServerVerification::apply_verifier_immediate_action(
                        &action,
                        fingerprint,
                    ),
                    Err(_) => Err(rustls::Error::InvalidCertificateData(format!(
                        "Failed to receive CertVerifierAction"
                    ))),
                }
            }
        }
    }

    fn apply_verifier_immediate_action(
        action: &CertVerifierAction,
        fingerprint: CertificateFingerprint,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        match action {
            CertVerifierAction::AbortConnection => {
                // TODO Error handling: details
                Err(rustls::Error::InvalidCertificateData(format!(
                    "CertVerifierAction requested to abort the connection"
                )))
            }
            CertVerifierAction::TrustOnce => Ok(rustls::client::ServerCertVerified::assertion()),
            CertVerifierAction::TrustAndStore => {
                // TODO Fix: Store the fingerprint
                todo!();
                Ok(rustls::client::ServerCertVerified::assertion())
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
        if let Some(known_fingerprint) = self.store.get(_server_name) {
            if *known_fingerprint == fingerprint {
                status = Some(CertVerificationStatus::TrustedCertificate);
            } else {
                status = Some(CertVerificationStatus::UntrustedCertificate);
            }
        } else {
            status = Some(CertVerificationStatus::UnknownCertificate);
        }
        match status {
            Some(status) => self.apply_verifier_behaviour_for_status(status, fingerprint),
            None => Err(rustls::Error::InvalidCertificateData(format!(
                "Internal error, no CertVerificationStatus"
            ))),
        }
    }
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

fn parse_known_host_line(
    line: String,
) -> Result<(rustls::ServerName, CertificateFingerprint), Box<dyn Error>> {
    todo!()
}

fn load_known_hosts_from_file(
    file_path: &String,
) -> Result<HashMap<rustls::ServerName, CertificateFingerprint>, Box<dyn Error>> {
    let mut store = HashMap::new();
    for line in BufReader::new(File::open(file_path)?).lines() {
        let entry = parse_known_host_line(line?)?;
        store.insert(entry.0, entry.1);
    }
    Ok(store)
}

fn load_known_hosts_store_from_config(
    known_host_config: KnownHosts,
) -> Result<HashMap<rustls::ServerName, CertificateFingerprint>, Box<dyn Error>> {
    match known_host_config {
        KnownHosts::Store(store) => Ok(store),
        // TODO Fix: Create a file if none exists + raise a warning
        KnownHosts::HostsFile(file) => load_known_hosts_from_file(&file),
    }
}

fn configure_client(
    cert_mode: CertificateVerificationMode,
    to_sync_client: mpsc::Sender<InternalAsyncMessage>,
) -> Result<ClientConfig, Box<dyn Error>> {
    match cert_mode {
        CertificateVerificationMode::SkipVerification => {
            let crypto = rustls::ClientConfig::builder()
                .with_safe_defaults()
                .with_custom_certificate_verifier(SkipServerVerification::new())
                .with_no_client_auth();

            Ok(ClientConfig::new(Arc::new(crypto)))
        }
        CertificateVerificationMode::SignedByCertificateAuthority => {
            Ok(ClientConfig::with_native_roots())
        }
        CertificateVerificationMode::TrustOnFirstUse(config) => {
            let store = load_known_hosts_store_from_config(config.known_hosts)?;
            let crypto = rustls::ClientConfig::builder()
                .with_safe_defaults()
                .with_custom_certificate_verifier(TofuServerVerification::new(
                    store,
                    config.verifier_behaviour,
                    to_sync_client,
                ))
                .with_no_client_auth();
            Ok(ClientConfig::new(Arc::new(crypto)))
        }
    }
}

async fn connection_task(
    config: ClientConfigurationData,
    cert_mode: CertificateVerificationMode,
    to_sync_client: mpsc::Sender<InternalAsyncMessage>,
    close_sender: tokio::sync::broadcast::Sender<()>,
    mut close_receiver: tokio::sync::broadcast::Receiver<()>,
    mut to_server_receiver: mpsc::Receiver<Bytes>,
    from_server_sender: mpsc::Sender<Bytes>,
) {
    let server_adr_str = format!("{}:{}", config.server_host, config.server_port);
    let srv_host = config.server_host.clone();
    let local_bind_adr = format!("{}:{}", config.local_bind_host, config.local_bind_port);

    info!("Trying to connect to server on: {} ...", server_adr_str);

    let server_addr: SocketAddr = server_adr_str
        .parse()
        .expect("Failed to parse server address");

    let client_cfg =
        configure_client(cert_mode, to_sync_client.clone()).expect("Failed to configure client");

    let mut endpoint = Endpoint::client(local_bind_adr.parse().unwrap())
        .expect("Failed to create client endpoint");
    endpoint.set_default_client_config(client_cfg);

    let new_connection = endpoint
        .connect(server_addr, &srv_host) // TODO Clean: error handling
        .expect("Failed to connect: configuration error")
        .await;
    match new_connection {
        Err(e) => error!("Error while connecting: {}", e),
        Ok(mut new_connection) => {
            info!(
                "Connected to {}",
                new_connection.connection.remote_address()
            );

            to_sync_client
                .send(InternalAsyncMessage::Connected(None))
                .await
                .expect("Failed to signal connection to sync client");

            let send = new_connection
                .connection
                .open_uni()
                .await
                .expect("Failed to open send stream");
            let mut frame_send = FramedWrite::new(send, LengthDelimitedCodec::new());

            let close_sender_clone = close_sender.clone();
            let _network_sends = tokio::spawn(async move {
                tokio::select! {
                    _ = close_receiver.recv() => {
                        trace!("Unidirectional send Stream forced to disconnected")
                    }
                    _ = async {
                        while let Some(msg_bytes) = to_server_receiver.recv().await {
                            if let Err(err) = frame_send.send(msg_bytes).await {
                                error!("Error while sending, {}", err); // TODO Clean: error handling
                                error!("Client seems disconnected, closing resources");
                                if let Err(_) = close_sender_clone.send(()) {
                                    error!("Failed to close all client streams & resources")
                                }
                                to_sync_client.send(
                                    InternalAsyncMessage::LostConnection)
                                    .await
                                    .expect("Failed to signal connection lost to sync client");
                            }
                        }
                    } => {
                        trace!("Unidirectional send Stream ended")
                    }
                }
            });

            let mut uni_receivers: JoinSet<()> = JoinSet::new();
            let mut close_receiver = close_sender.subscribe();
            let _network_reads = tokio::spawn(async move {
                tokio::select! {
                    _ = close_receiver.recv() => {
                        trace!("New Stream listener forced to disconnected")
                    }
                    _ = async {
                        while let Some(Ok(recv)) = new_connection.uni_streams.next().await {
                            let mut frame_recv = FramedRead::new(recv, LengthDelimitedCodec::new());
                            let from_server_sender = from_server_sender.clone();

                            uni_receivers.spawn(async move {
                                while let Some(Ok(msg_bytes)) = frame_recv.next().await {
                                    from_server_sender.send(msg_bytes.into()).await.unwrap(); // TODO Clean: error handling
                                }
                            });
                        }
                    } => {
                        trace!("New Stream listener ended ")
                    }
                }
                uni_receivers.shutdown().await;
                trace!("All unidirectional stream receivers cleaned");
            });
        }
    }
}

fn start_async_client(mut commands: Commands, runtime: Res<Runtime>) {
    let (from_server_sender, from_server_receiver) =
        mpsc::channel::<Bytes>(DEFAULT_MESSAGE_QUEUE_SIZE);
    let (to_server_sender, to_server_receiver) = mpsc::channel::<Bytes>(DEFAULT_MESSAGE_QUEUE_SIZE);

    let (to_sync_client, from_async_client) =
        mpsc::channel::<InternalAsyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);
    let (to_async_client, mut from_sync_client) =
        mpsc::channel::<InternalSyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);

    // Create a close channel for this connection
    let (close_sender, close_receiver): (
        tokio::sync::broadcast::Sender<()>,
        tokio::sync::broadcast::Receiver<()>,
    ) = broadcast::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);

    commands.insert_resource(Client {
        state: ClientState::Disconnected,
        sender: to_server_sender,
        receiver: from_server_receiver,
        close_sender: close_sender.clone(),
        internal_receiver: from_async_client,
        internal_sender: to_async_client.clone(),
    });

    // Async client
    runtime.spawn(async move {
        // Wait for a connection signal before starting client
        if let Some(message) = from_sync_client.recv().await {
            match message {
                InternalSyncMessage::Connect { config, cert_mode } => {
                    connection_task(
                        config,
                        cert_mode,
                        to_sync_client,
                        close_sender,
                        close_receiver,
                        to_server_receiver,
                        from_server_sender,
                    )
                    .await;
                }
            }
        }
    });
}

// Receive messages from the async client tasks and update the sync client.
fn update_sync_client(
    mut client: ResMut<Client>,
    mut connection_events: EventWriter<ConnectionEvent>,
    mut connection_lost_events: EventWriter<ConnectionLostEvent>,
    mut certificate_events: EventWriter<TofuCertificateEvent>,
) {
    while let Ok(message) = client.internal_receiver.try_recv() {
        match message {
            InternalAsyncMessage::Connected(_) => {
                client.state = ClientState::Connected;
                connection_events.send(ConnectionEvent);
            }
            InternalAsyncMessage::LostConnection => {
                client.state = ClientState::Disconnected;
                connection_lost_events.send(ConnectionLostEvent);
            }
            InternalAsyncMessage::CertificateActionRequest {
                status,
                action_sender,
            } => {
                certificate_events.send(TofuCertificateEvent {
                    status,
                    action_sender,
                });
            }
        }
    }
}

pub struct QuinnetClientPlugin {}

impl Default for QuinnetClientPlugin {
    fn default() -> Self {
        Self {}
    }
}

impl Plugin for QuinnetClientPlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<ConnectionEvent>()
            .add_event::<ConnectionLostEvent>()
            .add_event::<TofuCertificateEvent>()
            // StartupStage::PreStartup so that resources created in commands are available to default startup_systems
            .add_startup_system_to_stage(StartupStage::PreStartup, start_async_client)
            .add_system(update_sync_client);

        if app.world.get_resource_mut::<Runtime>().is_none() {
            app.insert_resource(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .unwrap(),
            );
        }
    }
}
