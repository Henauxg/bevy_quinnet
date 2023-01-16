use std::{
    collections::{
        hash_map::{Iter, IterMut},
        HashMap,
    },
    error::Error,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use bevy::prelude::*;
use bytes::Bytes;
use futures_util::StreamExt;
use quinn::{ClientConfig, Connection as QuinnConnection, ConnectionError, Endpoint};
use quinn_proto::ConnectionStats;
use serde::Deserialize;
use tokio::{
    runtime::{self},
    sync::{
        broadcast,
        mpsc::{
            self,
            error::{TryRecvError, TrySendError},
        },
        oneshot,
    },
    task::JoinSet,
};
use tokio_util::codec::{FramedRead, LengthDelimitedCodec};

use crate::shared::{
    channel::{
        channels_task, get_channel_id_from_type, Channel, ChannelAsyncMessage, ChannelId,
        ChannelSyncMessage, ChannelType, MultiChannelId,
    },
    AsyncRuntime, QuinnetError, DEFAULT_KILL_MESSAGE_QUEUE_SIZE, DEFAULT_MESSAGE_QUEUE_SIZE,
};

use self::certificate::{
    load_known_hosts_store_from_config, CertConnectionAbortEvent, CertInteractionEvent,
    CertTrustUpdateEvent, CertVerificationInfo, CertVerificationStatus, CertVerifierAction,
    CertificateVerificationMode, SkipServerVerification, TofuServerVerification,
};

pub mod certificate;

pub const DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE: usize = 100;
pub const DEFAULT_KNOWN_HOSTS_FILE: &str = "quinnet/known_hosts";

pub type ConnectionId = u64;

/// Connection event raised when the client just connected to the server. Raised in the CoreStage::PreUpdate stage.
pub struct ConnectionEvent {
    pub id: ConnectionId,
}
/// ConnectionLost event raised when the client is considered disconnected from the server. Raised in the CoreStage::PreUpdate stage.
pub struct ConnectionLostEvent {
    pub id: ConnectionId,
}

/// Configuration of the client, used when connecting to a server
#[derive(Debug, Deserialize, Clone)]
pub struct ConnectionConfiguration {
    server_host: String,
    server_port: u16,
    local_bind_host: String,
    local_bind_port: u16,
}

impl ConnectionConfiguration {
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
    /// use bevy_quinnet::client::ConnectionConfiguration;
    /// let config = ConnectionConfiguration::new(
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

type InternalConnectionRef = QuinnConnection;

/// Current state of a client connection
#[derive(Debug)]
enum ConnectionState {
    Connecting,
    Connected(InternalConnectionRef),
    Disconnected,
}

#[derive(Debug)]
pub(crate) enum ClientAsyncMessage {
    Connected(InternalConnectionRef),
    ConnectionClosed(ConnectionError),
    CertificateInteractionRequest {
        status: CertVerificationStatus,
        info: CertVerificationInfo,
        action_sender: oneshot::Sender<CertVerifierAction>,
    },
    CertificateTrustUpdate(CertVerificationInfo),
    CertificateConnectionAbort {
        status: CertVerificationStatus,
        cert_info: CertVerificationInfo,
    },
}
#[derive(Debug)]
pub(crate) struct ConnectionSpawnConfig {
    connection_config: ConnectionConfiguration,
    cert_mode: CertificateVerificationMode,
    to_sync_client_send: mpsc::Sender<ClientAsyncMessage>,
    to_channels_recv: mpsc::Receiver<ChannelSyncMessage>,
    from_channels_send: mpsc::Sender<ChannelAsyncMessage>,
    close_recv: broadcast::Receiver<()>,
    bytes_from_server_send: mpsc::Sender<Bytes>,
}

#[derive(Debug)]
pub struct Connection {
    state: ConnectionState,
    channels: HashMap<ChannelId, Channel>,
    default_channel: Option<ChannelId>,
    last_gen_id: MultiChannelId,
    bytes_from_server_recv: mpsc::Receiver<Bytes>,
    close_sender: broadcast::Sender<()>,

    pub(crate) from_async_client_recv: mpsc::Receiver<ClientAsyncMessage>,
    pub(crate) to_channels_send: mpsc::Sender<ChannelSyncMessage>,
    pub(crate) from_channels_recv: mpsc::Receiver<ChannelAsyncMessage>,
}

impl Connection {
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

    /// Same as [Connection::receive_message] but will log the error instead of returning it
    pub fn try_receive_message<T: serde::de::DeserializeOwned>(&mut self) -> Option<T> {
        match self.receive_message() {
            Ok(message) => message,
            Err(err) => {
                error!("try_receive_message: {}", err);
                None
            }
        }
    }

    pub fn send_message<T: serde::Serialize>(&self, message: T) -> Result<(), QuinnetError> {
        match self.default_channel {
            Some(channel) => self.send_message_on(channel, message),
            None => Err(QuinnetError::NoDefaultChannel),
        }
    }

    pub fn send_message_on<T: serde::Serialize>(
        &self,
        channel_id: ChannelId,
        message: T,
    ) -> Result<(), QuinnetError> {
        match &self.state {
            ConnectionState::Disconnected => Err(QuinnetError::ConnectionClosed),
            _ => match self.channels.get(&channel_id) {
                Some(channel) => match bincode::serialize(&message) {
                    Ok(payload) => channel.send_payload(payload),
                    Err(_) => Err(QuinnetError::Serialization),
                },
                None => Err(QuinnetError::UnknownChannel(channel_id)),
            },
        }
    }

    /// Same as [Connection::send_message] but will log the error instead of returning it
    pub fn try_send_message<T: serde::Serialize>(&self, message: T) {
        match self.send_message(message) {
            Ok(_) => {}
            Err(err) => error!("try_send_message: {}", err),
        }
    }

    pub fn send_payload<T: Into<Bytes>>(&self, payload: T) -> Result<(), QuinnetError> {
        match self.default_channel {
            Some(channel) => self.send_payload_on(channel, payload),
            None => Err(QuinnetError::NoDefaultChannel),
        }
    }

    pub fn send_payload_on<T: Into<Bytes>>(
        &self,
        channel_id: ChannelId,
        payload: T,
    ) -> Result<(), QuinnetError> {
        match &self.state {
            ConnectionState::Disconnected => Err(QuinnetError::ConnectionClosed),
            _ => match self.channels.get(&channel_id) {
                Some(channel) => channel.send_payload(payload),
                None => Err(QuinnetError::UnknownChannel(channel_id)),
            },
        }
    }

    /// Same as [Connection::send_payload] but will log the error instead of returning it
    pub fn try_send_payload<T: Into<Bytes>>(&self, payload: T) {
        match self.send_payload(payload) {
            Ok(_) => {}
            Err(err) => error!("try_send_payload: {}", err),
        }
    }

    pub fn receive_payload(&mut self) -> Result<Option<Bytes>, QuinnetError> {
        match &self.state {
            ConnectionState::Disconnected => Err(QuinnetError::ConnectionClosed),
            _ => match self.bytes_from_server_recv.try_recv() {
                Ok(msg_payload) => Ok(Some(msg_payload)),
                Err(err) => match err {
                    TryRecvError::Empty => Ok(None),
                    TryRecvError::Disconnected => Err(QuinnetError::InternalChannelClosed),
                },
            },
        }
    }

    /// Same as [Connection::receive_payload] but will log the error instead of returning it
    pub fn try_receive_payload(&mut self) -> Option<Bytes> {
        match self.receive_payload() {
            Ok(payload) => payload,
            Err(err) => {
                error!("try_receive_payload: {}", err);
                None
            }
        }
    }

    /// Immediately prevents new messages from being sent on the connection and signal the connection to closes all its background tasks. Before trully closing, the connection will wait for all buffered messages in all its opened channels to be properly sent according to their respective channel type.
    fn disconnect(&mut self) -> Result<(), QuinnetError> {
        match &self.state {
            ConnectionState::Disconnected => Ok(()),
            _ => {
                self.state = ConnectionState::Disconnected;
                match self.close_sender.send(()) {
                    Ok(_) => Ok(()),
                    Err(_) => {
                        // The only possible error for a send is that there is no active receivers, meaning that the tasks are already terminated.
                        Err(QuinnetError::ConnectionAlreadyClosed)
                    }
                }
            }
        }
    }

    fn try_disconnect(&mut self) {
        match &self.disconnect() {
            Ok(_) => (),
            Err(err) => error!("Failed to properly close clonnection: {}", err),
        }
    }

    pub fn is_connected(&self) -> bool {
        match self.state {
            ConnectionState::Connected(_) => true,
            _ => false,
        }
    }

    /// Returns statistics about the current connection if connected.
    pub fn stats(&self) -> Option<ConnectionStats> {
        match &self.state {
            ConnectionState::Connected(connection) => Some(connection.stats()),
            _ => None,
        }
    }

    /// Opens a channel of the requested [ChannelType] and returns its [ChannelId].
    ///
    /// By default, when starting a [Connection]], Quinnet creates 1 channel instance of each [ChannelType], each with their own [ChannelId]. Among those, there is a `default` channel which will be used when you don't specify the channel. At startup, this default channel is a [ChannelType::OrderedReliable] channel.
    ///
    /// If no channels were previously opened, the opened channel will be the new default channel.
    ///
    /// Can fail if the Connection is closed.
    pub fn open_channel(&mut self, channel_type: ChannelType) -> Result<ChannelId, QuinnetError> {
        let channel_id = get_channel_id_from_type(channel_type, || {
            self.last_gen_id += 1;
            self.last_gen_id
        });
        match self.channels.contains_key(&channel_id) {
            true => Ok(channel_id),
            false => self.create_channel(channel_id),
        }
    }

    /// Closes the channel with the corresponding [ChannelId].
    ///
    /// No new messages will be able to be sent on this channel, however, the channel will properly try to send all the messages that were previously pushed to it, according to its [ChannelType], before fully closing.
    ///
    /// If the closed channel is the current default channel, the default channel gets set to `None`.
    ///
    /// Can fail if the [ChannelId] is unknown, or if the channel is already closed.
    pub fn close_channel(&mut self, channel_id: ChannelId) -> Result<(), QuinnetError> {
        match self.channels.remove(&channel_id) {
            Some(channel) => {
                if Some(channel_id) == self.default_channel {
                    self.default_channel = None;
                }
                channel.close()
            }
            None => Err(QuinnetError::UnknownChannel(channel_id)),
        }
    }

    /// Set the default channel
    pub fn set_default_channel(&mut self, channel_id: ChannelId) {
        self.default_channel = Some(channel_id);
    }

    /// Get the default Channel Id
    pub fn get_default_channel(&self) -> Option<ChannelId> {
        self.default_channel
    }

    fn create_channel(&mut self, channel_id: ChannelId) -> Result<ChannelId, QuinnetError> {
        let (bytes_to_channel_send, bytes_to_channel_recv) =
            mpsc::channel::<Bytes>(DEFAULT_MESSAGE_QUEUE_SIZE);
        let (channel_close_send, channel_close_recv) =
            mpsc::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);

        match self
            .to_channels_send
            .try_send(ChannelSyncMessage::CreateChannel {
                channel_id,
                bytes_to_channel_recv,
                channel_close_recv,
            }) {
            Ok(_) => {
                let channel = Channel::new(bytes_to_channel_send, channel_close_send);
                self.channels.insert(channel_id, channel);
                if self.default_channel.is_none() {
                    self.default_channel = Some(channel_id);
                }

                Ok(channel_id)
            }
            Err(err) => match err {
                TrySendError::Full(_) => Err(QuinnetError::FullQueue),
                TrySendError::Closed(_) => Err(QuinnetError::InternalChannelClosed),
            },
        }
    }
}

#[derive(Resource)]
pub struct Client {
    runtime: runtime::Handle,
    connections: HashMap<ConnectionId, Connection>,
    last_gen_id: ConnectionId,
    default_connection_id: Option<ConnectionId>,
}

impl Client {
    /// Returns the default connection or None.
    pub fn get_connection(&self) -> Option<&Connection> {
        match self.default_connection_id {
            Some(id) => self.connections.get(&id),
            None => None,
        }
    }

    /// Returns the default connection as mut or None.
    pub fn get_connection_mut(&mut self) -> Option<&mut Connection> {
        match self.default_connection_id {
            Some(id) => self.connections.get_mut(&id),
            None => None,
        }
    }

    /// Returns the default connection. **Warning**, this function panics if there is no default connection.
    pub fn connection(&self) -> &Connection {
        self.connections
            .get(&self.default_connection_id.unwrap())
            .unwrap()
    }

    /// Returns the default connection as mut. **Warning**, this function panics if there is no default connection.
    pub fn connection_mut(&mut self) -> &mut Connection {
        self.connections
            .get_mut(&self.default_connection_id.unwrap())
            .unwrap()
    }

    /// Returns the requested connection.
    pub fn get_connection_by_id(&self, id: ConnectionId) -> Option<&Connection> {
        self.connections.get(&id)
    }

    /// Returns the requested connection as mut.
    pub fn get_connection_mut_by_id(&mut self, id: ConnectionId) -> Option<&mut Connection> {
        self.connections.get_mut(&id)
    }

    /// Returns an iterator over all connections
    pub fn connections(&self) -> Iter<ConnectionId, Connection> {
        self.connections.iter()
    }

    /// Returns an iterator over all connections as muts
    pub fn connections_mut(&mut self) -> IterMut<ConnectionId, Connection> {
        self.connections.iter_mut()
    }

    /// Open a connection to a server with the given [ConnectionConfiguration] and [CertificateVerificationMode]. The connection will raise an event when fully connected, see [ConnectionEvent]
    ///
    /// Returns the [ConnectionId], and the default [ChannelId]
    pub fn open_connection(
        &mut self,
        config: ConnectionConfiguration,
        cert_mode: CertificateVerificationMode,
    ) -> Result<(ConnectionId, ChannelId), QuinnetError> {
        let (bytes_from_server_send, bytes_from_server_recv) =
            mpsc::channel::<Bytes>(DEFAULT_MESSAGE_QUEUE_SIZE);

        let (to_sync_client_send, from_async_client_recv) =
            mpsc::channel::<ClientAsyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);
        let (from_channels_send, from_channels_recv) =
            mpsc::channel::<ChannelAsyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);
        let (to_channels_send, to_channels_recv) =
            mpsc::channel::<ChannelSyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);

        // Create a close channel for this connection
        let (close_sender, close_receiver) = broadcast::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);

        let mut connection = Connection {
            state: ConnectionState::Connecting,
            channels: HashMap::new(),
            last_gen_id: 0,
            default_channel: None,
            bytes_from_server_recv,
            close_sender: close_sender.clone(),
            from_async_client_recv,
            to_channels_send,
            from_channels_recv,
        };
        // Create default channels
        let ordered_reliable_id = connection.open_channel(ChannelType::OrderedReliable)?;
        connection.open_channel(ChannelType::UnorderedReliable)?;
        connection.open_channel(ChannelType::Unreliable)?;

        // Async connection
        self.runtime.spawn(async move {
            connection_task(ConnectionSpawnConfig {
                connection_config: config,
                cert_mode,
                to_channels_recv,
                from_channels_send,
                to_sync_client_send,
                close_recv: close_receiver,
                bytes_from_server_send,
            })
            .await
        });

        self.last_gen_id += 1;
        let connection_id = self.last_gen_id;
        self.connections.insert(connection_id, connection);
        if self.default_connection_id.is_none() {
            self.default_connection_id = Some(connection_id);
        }

        Ok((connection_id, ordered_reliable_id))
    }

    /// Set the default connection
    pub fn set_default_connection(&mut self, connection_id: ConnectionId) {
        self.default_connection_id = Some(connection_id);
    }

    /// Get the default Connection Id
    pub fn get_default_connection(&self) -> Option<ConnectionId> {
        self.default_connection_id
    }

    /// Close a specific connection. Removes it from the client. This may fail if no [Connection] if found for connection_id, or if the [Connection] is already closed.
    pub fn close_connection(&mut self, connection_id: ConnectionId) -> Result<(), QuinnetError> {
        match self.connections.remove(&connection_id) {
            Some(mut connection) => {
                if Some(connection_id) == self.default_connection_id {
                    self.default_connection_id = None;
                }
                connection.disconnect()
            }
            None => Err(QuinnetError::UnknownConnection(connection_id)),
        }
    }

    /// Calls close_connection on all the open connections.
    pub fn close_all_connections(&mut self) -> Result<(), QuinnetError> {
        for connection_id in self
            .connections
            .keys()
            .cloned()
            .collect::<Vec<ConnectionId>>()
        {
            self.close_connection(connection_id)?;
        }
        Ok(())
    }
}

fn configure_client(
    cert_mode: CertificateVerificationMode,
    to_sync_client: mpsc::Sender<ClientAsyncMessage>,
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
            let (store, store_file) = load_known_hosts_store_from_config(config.known_hosts)?;
            let crypto = rustls::ClientConfig::builder()
                .with_safe_defaults()
                .with_custom_certificate_verifier(TofuServerVerification::new(
                    store,
                    config.verifier_behaviour,
                    to_sync_client,
                    store_file,
                ))
                .with_no_client_auth();
            Ok(ClientConfig::new(Arc::new(crypto)))
        }
    }
}

async fn connection_task(spawn_config: ConnectionSpawnConfig) {
    let config = spawn_config.connection_config;
    let server_adr_str = format!("{}:{}", config.server_host, config.server_port);
    let srv_host = config.server_host.clone();
    let local_bind_adr = format!("{}:{}", config.local_bind_host, config.local_bind_port);

    info!("Trying to connect to server on: {} ...", server_adr_str);

    let server_addr: SocketAddr = server_adr_str
        .parse()
        .expect("Failed to parse server address");

    let client_cfg = configure_client(
        spawn_config.cert_mode,
        spawn_config.to_sync_client_send.clone(),
    )
    .expect("Failed to configure client");

    let mut endpoint = Endpoint::client(local_bind_adr.parse().unwrap())
        .expect("Failed to create client endpoint");
    endpoint.set_default_client_config(client_cfg);

    let connection = endpoint
        .connect(server_addr, &srv_host) // TODO Clean: error handling
        .expect("Failed to connect: configuration error")
        .await;
    match connection {
        Err(e) => error!("Error while connecting: {}", e),
        Ok(connection) => {
            info!("Connected to {}", connection.remote_address());

            spawn_config
                .to_sync_client_send
                .send(ClientAsyncMessage::Connected(connection.clone()))
                .await
                .expect("Failed to signal connection to sync client");

            // Spawn a task to listen for the underlying connection being closed
            {
                let conn = connection.clone();
                let to_sync_client = spawn_config.to_sync_client_send.clone();
                tokio::spawn(async move {
                    let conn_err = conn.closed().await;
                    info!("Disconnected: {}", conn_err);
                    // If we requested the connection to close, channel may have been closed already.
                    if !to_sync_client.is_closed() {
                        to_sync_client
                            .send(ClientAsyncMessage::ConnectionClosed(conn_err))
                            .await
                            .expect("Failed to signal connection lost to sync client");
                    }
                })
            };

            // Spawn a task to listen for streams opened by the server
            {
                let close_recv = spawn_config.close_recv.resubscribe();
                let connection_handle = connection.clone();
                tokio::spawn(async move {
                    connection_receiving_task(
                        connection_handle,
                        spawn_config.bytes_from_server_send,
                        close_recv,
                    )
                    .await
                });
            }

            // Spawn a task to handle channels for this connection
            tokio::spawn(async move {
                channels_task(
                    connection,
                    spawn_config.close_recv,
                    spawn_config.to_channels_recv,
                    spawn_config.from_channels_send,
                )
                .await
            });
        }
    }
}

async fn connection_receiving_task(
    connection: quinn::Connection,
    bytes_from_server_send: mpsc::Sender<Bytes>,
    mut close_recv: broadcast::Receiver<()>,
) {
    let mut uni_receivers: JoinSet<()> = JoinSet::new();
    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Listener for new Unidirectional Receiving Streams  received a close signal")
        }
        _ = async {
            while let Ok(recv) = connection.accept_uni().await {
                let mut frame_recv = FramedRead::new(recv, LengthDelimitedCodec::new());
                let from_server_sender = bytes_from_server_send.clone();
                uni_receivers.spawn(async move {
                    while let Some(Ok(msg_bytes)) = frame_recv.next().await {
                         // TODO Clean: error handling
                        from_server_sender.send(msg_bytes.into()).await.unwrap();
                    }
                });
            }
        } => {
            trace!("Listener for new Unidirectional Receiving Streams ended")
        }
    };
    uni_receivers.shutdown().await;
    trace!("All unidirectional stream receivers cleaned");
}

// Receive messages from the async client tasks and update the sync client.
fn update_sync_client(
    mut connection_events: EventWriter<ConnectionEvent>,
    mut connection_lost_events: EventWriter<ConnectionLostEvent>,
    mut certificate_interaction_events: EventWriter<CertInteractionEvent>,
    mut cert_trust_update_events: EventWriter<CertTrustUpdateEvent>,
    mut cert_connection_abort_events: EventWriter<CertConnectionAbortEvent>,
    mut client: ResMut<Client>,
) {
    for (connection_id, mut connection) in &mut client.connections {
        while let Ok(message) = connection.from_async_client_recv.try_recv() {
            match message {
                ClientAsyncMessage::Connected(internal_connection) => {
                    connection.state = ConnectionState::Connected(internal_connection);
                    connection_events.send(ConnectionEvent { id: *connection_id });
                }
                ClientAsyncMessage::ConnectionClosed(_) => match connection.state {
                    ConnectionState::Disconnected => (),
                    _ => {
                        connection.try_disconnect();
                        connection_lost_events.send(ConnectionLostEvent { id: *connection_id });
                    }
                },
                ClientAsyncMessage::CertificateInteractionRequest {
                    status,
                    info,
                    action_sender,
                } => {
                    certificate_interaction_events.send(CertInteractionEvent {
                        connection_id: *connection_id,
                        status,
                        info,
                        action_sender: Mutex::new(Some(action_sender)),
                    });
                }
                ClientAsyncMessage::CertificateTrustUpdate(info) => {
                    cert_trust_update_events.send(CertTrustUpdateEvent {
                        connection_id: *connection_id,
                        cert_info: info,
                    });
                }
                ClientAsyncMessage::CertificateConnectionAbort { status, cert_info } => {
                    cert_connection_abort_events.send(CertConnectionAbortEvent {
                        connection_id: *connection_id,
                        status,
                        cert_info,
                    });
                }
            }
        }
        while let Ok(message) = connection.from_channels_recv.try_recv() {
            match message {
                ChannelAsyncMessage::LostConnection => match connection.state {
                    ConnectionState::Disconnected => (),
                    _ => {
                        connection.try_disconnect();
                        connection_lost_events.send(ConnectionLostEvent { id: *connection_id });
                    }
                },
            }
        }
    }
}

fn create_client(mut commands: Commands, runtime: Res<AsyncRuntime>) {
    commands.insert_resource(Client {
        connections: HashMap::new(),
        runtime: runtime.handle().clone(),
        last_gen_id: 0,
        default_connection_id: None,
    });
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
            .add_event::<CertInteractionEvent>()
            .add_event::<CertTrustUpdateEvent>()
            .add_event::<CertConnectionAbortEvent>()
            // StartupStage::PreStartup so that resources created in commands are available to default startup_systems
            .add_startup_system_to_stage(StartupStage::PreStartup, create_client)
            .add_system_to_stage(CoreStage::PreUpdate, update_sync_client);

        if app.world.get_resource_mut::<AsyncRuntime>().is_none() {
            app.insert_resource(AsyncRuntime(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .unwrap(),
            ));
        }
    }
}
