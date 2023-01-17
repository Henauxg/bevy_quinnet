use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};

use bevy::prelude::*;
use bytes::Bytes;
use futures_util::StreamExt;
use quinn::{ConnectionError, Endpoint as QuinnEndpoint, RecvStream, ServerConfig};
use serde::Deserialize;
use tokio::{
    runtime,
    sync::{
        broadcast::{self},
        mpsc::{
            self,
            error::{TryRecvError, TrySendError},
        },
    },
};
use tokio_util::codec::{FramedRead, LengthDelimitedCodec};

use crate::{
    server::certificate::retrieve_certificate,
    shared::{
        channel::{
            channels_task, get_channel_id_from_type, Channel, ChannelAsyncMessage, ChannelId,
            ChannelSyncMessage, ChannelType, MultiChannelId,
        },
        AsyncRuntime, ClientId, QuinnetError, DEFAULT_KEEP_ALIVE_INTERVAL_S,
        DEFAULT_KILL_MESSAGE_QUEUE_SIZE, DEFAULT_MESSAGE_QUEUE_SIZE,
    },
};

use self::certificate::{CertificateRetrievalMode, ServerCertificate};

pub mod certificate;

pub const DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE: usize = 100;

/// Connection event raised when a client just connected to the server. Raised in the CoreStage::PreUpdate stage.
pub struct ConnectionEvent {
    /// Id of the client who connected
    pub id: ClientId,
}

/// ConnectionLost event raised when a client is considered disconnected from the server. Raised in the CoreStage::PreUpdate stage.
pub struct ConnectionLostEvent {
    /// Id of the client who lost connection
    pub id: ClientId,
}

/// Configuration of the server, used when the server starts
///
/// # Examples
///
/// ```
/// use bevy_quinnet::server::ServerConfigurationData;
/// let config = ServerConfigurationData::new(
///             "127.0.0.1".to_string(),
///             6000,
///             "0.0.0.0".to_string());
/// ```
#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfigurationData {
    host: String,
    port: u16,
    local_bind_host: String,
}

impl ServerConfigurationData {
    /// Creates a new ServerConfigurationData
    ///
    /// # Arguments
    ///
    /// * `host` - Address of the server
    /// * `port` - Port that the server is listening on
    /// * `local_bind_host` - Local address to bind to, which should usually be a wildcard address like `0.0.0.0` or `[::]`, which allow communication with any reachable IPv4 or IPv6 address. See [`quinn::endpoint::Endpoint`] for more precision
    ///
    /// # Examples
    ///
    /// ```
    /// use bevy_quinnet::server::ServerConfigurationData;
    /// let config = ServerConfigurationData::new(
    ///         "127.0.0.1".to_string(),
    ///         6000,
    ///         "0.0.0.0".to_string(),
    ///     );
    /// ```
    pub fn new(host: String, port: u16, local_bind_host: String) -> Self {
        Self {
            host,
            port,
            local_bind_host,
        }
    }
}

/// Represents a client message in its binary form
#[derive(Debug)]
pub struct ClientPayload {
    /// Id of the client sending the message
    client_id: ClientId,
    /// Content of the message as bytes
    msg: Bytes,
}

#[derive(Debug)]
pub(crate) enum ServerAsyncMessage {
    ClientConnected(ClientConnection),
    ClientConnectionClosed(ClientId, ConnectionError),
}

#[derive(Debug, Clone)]
pub(crate) enum ServerSyncMessage {
    ClientConnectedAck(ClientId),
}

#[derive(Debug)]
pub(crate) struct ClientConnection {
    client_id: ClientId,
    channels: HashMap<ChannelId, Channel>,
    close_sender: broadcast::Sender<()>,

    pub(crate) to_channels_send: mpsc::Sender<ChannelSyncMessage>,
    pub(crate) from_channels_recv: mpsc::Receiver<ChannelAsyncMessage>,
}

impl ClientConnection {
    /// Immediately prevents new messages from being sent on the channel and signal the channel to closes all its background tasks.
    /// Before trully closing, the channel will wait for all buffered messages to be properly sent according to the channel type.
    /// Can fail if the [ChannelId] is unknown, or if the channel is already closed.
    pub(crate) fn close_channel(&mut self, channel_id: ChannelId) -> Result<(), QuinnetError> {
        match self.channels.remove(&channel_id) {
            Some(channel) => channel.close(),
            None => Err(QuinnetError::UnknownChannel(channel_id)),
        }
    }

    pub(crate) fn create_channel(
        &mut self,
        channel_id: ChannelId,
    ) -> Result<ChannelId, QuinnetError> {
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
                Ok(channel_id)
            }
            Err(err) => match err {
                TrySendError::Full(_) => Err(QuinnetError::FullQueue),
                TrySendError::Closed(_) => Err(QuinnetError::InternalChannelClosed),
            },
        }
    }
}

pub struct Endpoint {
    clients: HashMap<ClientId, ClientConnection>,
    channels: HashSet<ChannelId>,
    default_channel: Option<ChannelId>,
    last_gen_id: MultiChannelId,
    payloads_from_clients_recv: mpsc::Receiver<ClientPayload>,
    close_sender: broadcast::Sender<()>,

    pub(crate) from_async_server_recv: mpsc::Receiver<ServerAsyncMessage>,
    pub(crate) to_async_server_send: broadcast::Sender<ServerSyncMessage>,
}

impl Endpoint {
    pub fn receive_message<T: serde::de::DeserializeOwned>(
        &mut self,
    ) -> Result<Option<(T, ClientId)>, QuinnetError> {
        match self.receive_payload()? {
            Some(client_msg) => match bincode::deserialize(&client_msg.msg) {
                Ok(msg) => Ok(Some((msg, client_msg.client_id))),
                Err(_) => Err(QuinnetError::Deserialization),
            },
            None => Ok(None),
        }
    }

    pub fn try_receive_message<T: serde::de::DeserializeOwned>(&mut self) -> Option<(T, ClientId)> {
        match self.receive_message() {
            Ok(message) => message,
            Err(err) => {
                error!("try_receive_message: {}", err);
                None
            }
        }
    }

    pub fn receive_payload(&mut self) -> Result<Option<ClientPayload>, QuinnetError> {
        match self.payloads_from_clients_recv.try_recv() {
            Ok(msg) => Ok(Some(msg)),
            Err(err) => match err {
                TryRecvError::Empty => Ok(None),
                TryRecvError::Disconnected => Err(QuinnetError::InternalChannelClosed),
            },
        }
    }

    pub fn try_receive_payload(&mut self) -> Option<ClientPayload> {
        match self.receive_payload() {
            Ok(payload) => payload,
            Err(err) => {
                error!("try_receive_payload: {}", err);
                None
            }
        }
    }

    pub fn send_message<T: serde::Serialize>(
        &mut self,
        client_id: ClientId,
        message: T,
    ) -> Result<(), QuinnetError> {
        match self.default_channel {
            Some(channel) => self.send_message_on(client_id, channel, message),
            None => Err(QuinnetError::NoDefaultChannel),
        }
    }

    pub fn send_message_on<T: serde::Serialize>(
        &mut self,
        client_id: ClientId,
        channel_id: ChannelId,
        message: T,
    ) -> Result<(), QuinnetError> {
        match bincode::serialize(&message) {
            Ok(payload) => Ok(self.send_payload_on(client_id, channel_id, payload)?),
            Err(_) => Err(QuinnetError::Serialization),
        }
    }

    pub fn try_send_message<T: serde::Serialize>(&mut self, client_id: ClientId, message: T) {
        match self.send_message(client_id, message) {
            Ok(_) => {}
            Err(err) => error!("try_send_message: {}", err),
        }
    }

    pub fn try_send_message_on<T: serde::Serialize>(
        &mut self,
        client_id: ClientId,
        channel_id: ChannelId,
        message: T,
    ) {
        match self.send_message_on(client_id, channel_id, message) {
            Ok(_) => {}
            Err(err) => error!("try_send_message: {}", err),
        }
    }

    pub fn send_group_message<'a, I: Iterator<Item = &'a ClientId>, T: serde::Serialize>(
        &self,
        client_ids: I,
        message: T,
    ) -> Result<(), QuinnetError> {
        match self.default_channel {
            Some(channel) => self.send_group_message_on(client_ids, channel, message),
            None => Err(QuinnetError::NoDefaultChannel),
        }
    }

    pub fn send_group_message_on<'a, I: Iterator<Item = &'a ClientId>, T: serde::Serialize>(
        &self,
        client_ids: I,
        channel_id: ChannelId,
        message: T,
    ) -> Result<(), QuinnetError> {
        match bincode::serialize(&message) {
            Ok(payload) => {
                for id in client_ids {
                    self.send_payload_on(*id, channel_id, payload.clone())?;
                }
                Ok(())
            }
            Err(_) => Err(QuinnetError::Serialization),
        }
    }

    pub fn try_send_group_message<'a, I: Iterator<Item = &'a ClientId>, T: serde::Serialize>(
        &self,
        client_ids: I,
        message: T,
    ) {
        match self.send_group_message(client_ids, message) {
            Ok(_) => {}
            Err(err) => error!("try_send_group_message: {}", err),
        }
    }

    pub fn try_send_group_message_on<'a, I: Iterator<Item = &'a ClientId>, T: serde::Serialize>(
        &self,
        client_ids: I,
        channel_id: ChannelId,
        message: T,
    ) {
        match self.send_group_message_on(client_ids, channel_id, message) {
            Ok(_) => {}
            Err(err) => error!("try_send_group_message: {}", err),
        }
    }

    pub fn broadcast_message<T: serde::Serialize>(&self, message: T) -> Result<(), QuinnetError> {
        match self.default_channel {
            Some(channel) => self.broadcast_message_on(channel, message),
            None => Err(QuinnetError::NoDefaultChannel),
        }
    }

    pub fn broadcast_message_on<T: serde::Serialize>(
        &self,
        channel_id: ChannelId,
        message: T,
    ) -> Result<(), QuinnetError> {
        match bincode::serialize(&message) {
            Ok(payload) => Ok(self.broadcast_payload_on(channel_id, payload)?),
            Err(_) => Err(QuinnetError::Serialization),
        }
    }

    pub fn try_broadcast_message<T: serde::Serialize>(&self, message: T) {
        match self.broadcast_message(message) {
            Ok(_) => {}
            Err(err) => error!("try_broadcast_message: {}", err),
        }
    }

    pub fn try_broadcast_message_on<T: serde::Serialize>(&self, message: T, channel_id: ChannelId) {
        match self.broadcast_message_on(channel_id, message) {
            Ok(_) => {}
            Err(err) => error!("try_broadcast_message: {}", err),
        }
    }

    pub fn broadcast_payload<T: Into<Bytes> + Clone>(
        &self,
        payload: T,
    ) -> Result<(), QuinnetError> {
        match self.default_channel {
            Some(channel) => self.broadcast_payload_on(channel, payload),
            None => Err(QuinnetError::NoDefaultChannel),
        }
    }

    pub fn broadcast_payload_on<T: Into<Bytes> + Clone>(
        &self,
        channel_id: ChannelId,
        payload: T,
    ) -> Result<(), QuinnetError> {
        let payload = payload.into();
        for (_, client_connection) in self.clients.iter() {
            match client_connection.channels.get(&channel_id) {
                Some(channel) => channel.send_payload(payload.clone())?,
                None => return Err(QuinnetError::UnknownChannel(channel_id)),
            };
        }
        Ok(())
    }

    pub fn try_broadcast_payload<T: Into<Bytes> + Clone>(&self, payload: T) {
        match self.broadcast_payload(payload) {
            Ok(_) => {}
            Err(err) => error!("try_broadcast_payload: {}", err),
        }
    }

    pub fn try_broadcast_payload_on<T: Into<Bytes> + Clone>(
        &self,
        channel_id: ChannelId,
        payload: T,
    ) {
        match self.broadcast_payload_on(channel_id, payload) {
            Ok(_) => {}
            Err(err) => error!("try_broadcast_payload_on: {}", err),
        }
    }

    pub fn send_payload<T: Into<Bytes>>(
        &self,
        client_id: ClientId,
        payload: T,
    ) -> Result<(), QuinnetError> {
        match self.default_channel {
            Some(channel) => self.send_payload_on(client_id, channel, payload),
            None => Err(QuinnetError::NoDefaultChannel),
        }
    }

    pub fn send_payload_on<T: Into<Bytes>>(
        &self,
        client_id: ClientId,
        channel_id: ChannelId,
        payload: T,
    ) -> Result<(), QuinnetError> {
        if let Some(client_connection) = self.clients.get(&client_id) {
            match client_connection.channels.get(&channel_id) {
                Some(channel) => channel.send_payload(payload),
                None => return Err(QuinnetError::UnknownChannel(channel_id)),
            }
        } else {
            Err(QuinnetError::UnknownClient(client_id))
        }
    }

    pub fn try_send_payload<T: Into<Bytes>>(&self, client_id: ClientId, payload: T) {
        match self.send_payload(client_id, payload) {
            Ok(_) => {}
            Err(err) => error!("try_send_payload: {}", err),
        }
    }

    pub fn try_send_payload_on<T: Into<Bytes>>(
        &self,
        client_id: ClientId,
        channel_id: ChannelId,
        payload: T,
    ) {
        match self.send_payload_on(client_id, channel_id, payload) {
            Ok(_) => {}
            Err(err) => error!("try_send_payload_on: {}", err),
        }
    }

    pub fn disconnect_client(&mut self, client_id: ClientId) -> Result<(), QuinnetError> {
        match self.clients.remove(&client_id) {
            Some(client_connection) => match client_connection.close_sender.send(()) {
                Ok(_) => Ok(()),
                Err(_) => Err(QuinnetError::ClientAlreadyDisconnected(client_id)),
            },
            None => Err(QuinnetError::UnknownClient(client_id)),
        }
    }

    /// Same as disconnect_client but errors are logged instead of returned
    pub fn try_disconnect_client(&mut self, client_id: ClientId) {
        match self.disconnect_client(client_id) {
            Ok(_) => (),
            Err(err) => error!(
                "Failed to properly disconnect client {}: {}",
                client_id, err
            ),
        }
    }

    pub fn disconnect_all_clients(&mut self) -> Result<(), QuinnetError> {
        for client_id in self.clients.keys().cloned().collect::<Vec<ClientId>>() {
            self.disconnect_client(client_id)?;
        }
        Ok(())
    }

    /// Opens a channel of the requested [ChannelType] and returns its [ChannelId].
    ///
    /// By default, when starting an [Endpoint], Quinnet creates 1 channel instance of each [ChannelType], each with their own [ChannelId]. Among those, there is a `default` channel which will be used when you don't specify the channel. At startup, this default channel is a [ChannelType::OrderedReliable] channel.
    ///
    /// If no channels were previously opened, the opened channel will be the new default channel.
    ///
    /// Can fail if the Endpoint is closed.
    pub fn open_channel(&mut self, channel_type: ChannelType) -> Result<ChannelId, QuinnetError> {
        let channel_id = get_channel_id_from_type(channel_type, || {
            self.last_gen_id += 1;
            self.last_gen_id
        });
        match self.channels.contains(&channel_id) {
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
            true => {
                if Some(channel_id) == self.default_channel {
                    self.default_channel = None;
                }
                for (_, connection) in self.clients.iter_mut() {
                    connection.close_channel(channel_id)?;
                }
                Ok(())
            }
            false => Err(QuinnetError::UnknownChannel(channel_id)),
        }
    }

    /// Set the default channel via its [ChannelId]
    pub fn set_default_channel(&mut self, channel_id: ChannelId) {
        self.default_channel = Some(channel_id);
    }

    /// Get the default [ChannelId]
    pub fn get_default_channel(&self) -> Option<ChannelId> {
        self.default_channel
    }

    fn open_default_channels(&mut self) -> Result<ChannelId, QuinnetError> {
        let ordered_reliable_id = self.open_channel(ChannelType::OrderedReliable)?;
        self.open_channel(ChannelType::UnorderedReliable)?;
        self.open_channel(ChannelType::Unreliable)?;
        Ok(ordered_reliable_id)
    }

    fn create_channel(&mut self, channel_id: ChannelId) -> Result<ChannelId, QuinnetError> {
        for (_, client_connection) in self.clients.iter_mut() {
            client_connection.create_channel(channel_id)?;
        }
        self.channels.insert(channel_id);
        if self.default_channel.is_none() {
            self.default_channel = Some(channel_id);
        }
        Ok(channel_id)
    }

    fn close_incoming_connections_handler(&mut self) -> Result<(), QuinnetError> {
        match self.close_sender.send(()) {
            Ok(_) => Ok(()),
            Err(_) => Err(QuinnetError::InternalChannelClosed),
        }
    }

    fn handle_connection(&mut self, mut connection: ClientConnection) -> Result<(), QuinnetError> {
        match self.clients.contains_key(&connection.client_id) {
            true => todo!(),
            false => {
                for channel_id in self.channels.iter() {
                    connection.create_channel(*channel_id)?;
                }

                match self
                    .to_async_server_send
                    .send(ServerSyncMessage::ClientConnectedAck(connection.client_id))
                {
                    Ok(_) => {
                        self.clients.insert(connection.client_id, connection);
                        Ok(())
                    }
                    Err(_) => Err(QuinnetError::InternalChannelClosed),
                }
            }
        }
    }
}

#[derive(Resource)]
pub struct Server {
    runtime: runtime::Handle,
    endpoint: Option<Endpoint>,
}

impl Server {
    pub fn endpoint(&self) -> &Endpoint {
        self.endpoint.as_ref().unwrap()
    }

    pub fn endpoint_mut(&mut self) -> &mut Endpoint {
        self.endpoint.as_mut().unwrap()
    }

    pub fn get_endpoint(&self) -> Option<&Endpoint> {
        self.endpoint.as_ref()
    }

    pub fn get_endpoint_mut(&mut self) -> Option<&mut Endpoint> {
        self.endpoint.as_mut()
    }

    /// Starts a new endpoint with the given [ServerConfigurationData] and [CertificateRetrievalMode] and opens the default channels.
    ///
    /// Returns a tuple of the [ServerCertificate] generated or loaded, and the default [ChannelId]
    pub fn start_endpoint(
        &mut self,
        config: ServerConfigurationData,
        cert_mode: CertificateRetrievalMode,
    ) -> Result<(ServerCertificate, ChannelId), QuinnetError> {
        let server_adr_str = format!("{}:{}", config.local_bind_host, config.port);
        let server_addr = server_adr_str.parse::<SocketAddr>()?;

        // Endpoint configuration
        let server_cert = retrieve_certificate(&config.host, cert_mode)?;
        let mut server_config = ServerConfig::with_single_cert(
            server_cert.cert_chain.clone(),
            server_cert.priv_key.clone(),
        )?;
        Arc::get_mut(&mut server_config.transport)
            .ok_or(QuinnetError::LockAcquisitionFailure)?
            .keep_alive_interval(Some(Duration::from_secs(DEFAULT_KEEP_ALIVE_INTERVAL_S)));

        let (payloads_from_clients_send, payloads_from_clients_recv) =
            mpsc::channel::<ClientPayload>(DEFAULT_MESSAGE_QUEUE_SIZE);
        let (to_sync_server_send, from_async_server_recv) =
            mpsc::channel::<ServerAsyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);
        let (to_async_server_send, from_sync_server_recv) =
            broadcast::channel::<ServerSyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);
        let (endpoint_close_send, endpoint_close_recv) =
            broadcast::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);

        info!("Starting endpoint on: {} ...", server_adr_str);

        self.runtime.spawn(async move {
            endpoint_task(
                server_config,
                server_addr,
                to_sync_server_send.clone(),
                endpoint_close_recv,
                from_sync_server_recv,
                payloads_from_clients_send.clone(),
            )
            .await;
        });

        let mut endpoint = Endpoint {
            clients: HashMap::new(),
            channels: HashSet::new(),
            default_channel: None,
            last_gen_id: 0,
            payloads_from_clients_recv,
            close_sender: endpoint_close_send,
            from_async_server_recv,
            to_async_server_send: to_async_server_send.clone(),
        };
        let ordered_reliable_id = endpoint.open_default_channels()?;
        self.endpoint = Some(endpoint);

        Ok((server_cert, ordered_reliable_id))
    }

    /// Closes the endpoint and all the connections associated with it
    ///
    /// Returns `QuinnetError::EndpointAlreadyClosed` if the endpoint is already closed
    pub fn stop_endpoint(&mut self) -> Result<(), QuinnetError> {
        match self.endpoint.take() {
            Some(mut endpoint) => {
                endpoint.close_incoming_connections_handler()?;
                endpoint.disconnect_all_clients()
            }
            None => Err(QuinnetError::EndpointAlreadyClosed),
        }
    }

    /// Returns true if the server is currently listening for messages and connections.
    pub fn is_listening(&self) -> bool {
        match &self.endpoint {
            Some(_) => true,
            None => false,
        }
    }
}

async fn endpoint_task(
    endpoint_config: ServerConfig,
    endpoint_adr: SocketAddr,
    to_sync_server_send: mpsc::Sender<ServerAsyncMessage>,
    mut endpoint_close_recv: broadcast::Receiver<()>,
    from_sync_server_recv: broadcast::Receiver<ServerSyncMessage>,
    payloads_from_clients_send: mpsc::Sender<ClientPayload>,
) {
    let mut client_gen_id: ClientId = 0;
    let mut client_id_mappings = HashMap::new();

    let endpoint = QuinnEndpoint::server(endpoint_config, endpoint_adr)
        .expect("Failed to create the endpoint");
    // Handle incoming connections/clients.
    tokio::select! {
        _ = endpoint_close_recv.recv() => {
            trace!("Endpoint incoming connection handler received a request to close")
        }
        _ = async {
            while let Some(connecting) = endpoint.accept().await {
                match connecting.await {
                    Err(err) => error!("An incoming connection failed: {}", err),
                    Ok(connection) => {
                        client_gen_id += 1; // TODO Fix: Better id generation/check
                        let client_id = client_gen_id;
                        client_id_mappings.insert(connection.stable_id(), client_id);

                        {
                            let to_sync_server_send = to_sync_server_send.clone();
                            let from_sync_server_recv = from_sync_server_recv.resubscribe();
                            let payloads_from_clients_send = payloads_from_clients_send.clone();
                            tokio::spawn(async move {
                                client_connection_task(
                                    connection,
                                    client_id,
                                    to_sync_server_send,
                                    from_sync_server_recv,
                                    payloads_from_clients_send,
                                )
                                .await
                            });
                        }
                    },
                }
            }
        } => {}
    }
}

async fn client_connection_task(
    connection: quinn::Connection,
    client_id: ClientId,
    to_sync_server_send: mpsc::Sender<ServerAsyncMessage>,
    mut from_sync_server_recv: broadcast::Receiver<ServerSyncMessage>,
    payloads_from_clients_send: mpsc::Sender<ClientPayload>,
) {
    info!(
        "New connection from {}, client_id: {}",
        connection.remote_address(),
        client_id
    );

    let (client_close_send, client_close_recv) =
        broadcast::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);
    let (from_channels_send, from_channels_recv) =
        mpsc::channel::<ChannelAsyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);
    let (to_channels_send, to_channels_recv) =
        mpsc::channel::<ChannelSyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);

    // Signal the sync server of this new connection
    to_sync_server_send
        .send(ServerAsyncMessage::ClientConnected(ClientConnection {
            client_id: client_id,
            channels: HashMap::new(),
            close_sender: client_close_send.clone(),
            from_channels_recv,
            to_channels_send,
        }))
        .await
        .expect("Failed to signal connection to sync client");

    // Wait for the sync server to acknowledge the connection before spawning reception tasks.
    while let Ok(ServerSyncMessage::ClientConnectedAck(id)) = from_sync_server_recv.recv().await {
        if id == client_id {
            break;
        }
    }

    // Spawn a task to listen for the underlying connection being closed
    {
        let conn = connection.clone();
        let to_sync_server = to_sync_server_send.clone();
        tokio::spawn(async move {
            let conn_err = conn.closed().await;
            info!("Client {} connection closed: {}", client_id, conn_err);
            // If we requested the connection to close, channel may have been closed already.
            if !to_sync_server.is_closed() {
                to_sync_server
                    .send(ServerAsyncMessage::ClientConnectionClosed(
                        client_id, conn_err,
                    ))
                    .await
                    .expect("Failed to signal connection lost to sync server");
            }
        });
    };

    // Spawn a task to listen for streams opened by this client
    {
        let conn = connection.clone();
        let client_close_recv = client_close_recv.resubscribe();
        tokio::spawn(async move {
            client_receiver_task(
                client_id,
                conn,
                client_close_recv,
                payloads_from_clients_send,
            )
            .await
        });
    }

    // Spawn a task to handle channels for this client
    tokio::spawn(async move {
        channels_task(
            connection,
            client_close_recv,
            to_channels_recv,
            from_channels_send,
        )
        .await
    });
}

async fn uni_receiver_task(
    client_id: ClientId,
    mut close_recv: broadcast::Receiver<()>,
    recv: RecvStream,
    payloads_from_clients_send: mpsc::Sender<ClientPayload>,
) {
    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Listener of a Unidirectional Receiving Streams received a close signal")
        }
        _ = async {
            let mut frame_recv = FramedRead::new(recv, LengthDelimitedCodec::new());
            while let Some(Ok(msg_bytes)) = frame_recv.next().await {
                // TODO Clean: error handling
                payloads_from_clients_send
                .send(ClientPayload {
                    client_id: client_id,
                    msg: msg_bytes.into(),
                })
                .await
                .unwrap();
            }
        } => {}
    };
}

async fn client_receiver_task(
    client_id: ClientId,
    connection: quinn::Connection,
    mut close_recv: tokio::sync::broadcast::Receiver<()>,
    payloads_from_clients_send: mpsc::Sender<ClientPayload>,
) {
    let close_recv_clone = close_recv.resubscribe();
    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Listener for new Unidirectional Receiving Streams received a close signal for client: {}", client_id)
        }
        _ = async {
            while let Ok(recv) = connection.accept_uni().await {
                let payloads_from_clients_send = payloads_from_clients_send.clone();
                let  close_recv_clone = close_recv_clone.resubscribe();
                tokio::spawn(async move {
                    uni_receiver_task(
                        client_id,
                        close_recv_clone,
                        recv,
                        payloads_from_clients_send
                    ).await;
                });
            }
        } => {
            trace!("New Stream listener ended for client: {}", client_id)
        }
    }
    trace!(
        "All unidirectional stream receivers cleaned for client: {}",
        client_id
    )
}

fn create_server(mut commands: Commands, runtime: Res<AsyncRuntime>) {
    commands.insert_resource(Server {
        endpoint: None,
        runtime: runtime.handle().clone(),
    });
}

// Receive messages from the async server tasks and update the sync server.
fn update_sync_server(
    mut server: ResMut<Server>,
    mut connection_events: EventWriter<ConnectionEvent>,
    mut connection_lost_events: EventWriter<ConnectionLostEvent>,
) {
    if let Some(endpoint) = server.get_endpoint_mut() {
        while let Ok(message) = endpoint.from_async_server_recv.try_recv() {
            match message {
                ServerAsyncMessage::ClientConnected(connection) => {
                    let id = connection.client_id;
                    match endpoint.handle_connection(connection) {
                        Ok(_) => connection_events.send(ConnectionEvent { id }),
                        Err(_) => error!("Failed to handle connection of client {}", id),
                    };
                }
                ServerAsyncMessage::ClientConnectionClosed(client_id, _) => {
                    match endpoint.clients.contains_key(&client_id) {
                        true => {
                            endpoint.try_disconnect_client(client_id);
                            connection_lost_events.send(ConnectionLostEvent { id: client_id })
                        }
                        false => (),
                    }
                }
            }
        }

        let mut lost_clients = HashSet::new();
        for (client_id, connection) in endpoint.clients.iter_mut() {
            while let Ok(message) = connection.from_channels_recv.try_recv() {
                match message {
                    ChannelAsyncMessage::LostConnection => {
                        if !lost_clients.contains(client_id) {
                            lost_clients.insert(*client_id);
                            connection_lost_events.send(ConnectionLostEvent { id: *client_id })
                        }
                    }
                }
            }
        }
        for client_id in lost_clients {
            endpoint.try_disconnect_client(client_id);
        }
    }
}

pub struct QuinnetServerPlugin {}

impl Default for QuinnetServerPlugin {
    fn default() -> Self {
        Self {}
    }
}

impl Plugin for QuinnetServerPlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<ConnectionEvent>()
            .add_event::<ConnectionLostEvent>()
            .add_startup_system_to_stage(StartupStage::PreStartup, create_server)
            .add_system_to_stage(CoreStage::PreUpdate, update_sync_server);

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
