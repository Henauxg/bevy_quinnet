use std::{
    collections::{HashMap, HashSet},
    net::{AddrParseError, IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use bevy::prelude::*;
use bytes::Bytes;
use quinn::{ConnectionError, Endpoint as QuinnEndpoint, ServerConfig};
use quinn_proto::ConnectionStats;
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

use crate::{
    server::certificate::retrieve_certificate,
    shared::{
        channel::{
            channels_task, get_channel_id_from_type, reliable_receiver_task,
            unreliable_receiver_task, Channel, ChannelAsyncMessage, ChannelId, ChannelSyncMessage,
            ChannelType, MultiChannelId,
        },
        AsyncRuntime, ClientId, InternalConnectionRef, QuinnetError, DEFAULT_KEEP_ALIVE_INTERVAL_S,
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

/// Configuration of the server, used when the server starts an Endpoint
#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfiguration {
    local_bind_addr: SocketAddr,
}

impl ServerConfiguration {
    /// Creates a new ServerConfiguration
    ///
    /// # Arguments
    ///
    /// * `local_bind_addr_str` - Local address and port to bind to separated by `:`. The address should usually be a wildcard like `0.0.0.0` (for an IPv4) or `[::]` (for an IPv6), which allow communication with any reachable IPv4 or IPv6 address. See [`std::net::SocketAddrV4`] and [`std::net::SocketAddrV6`] or [`quinn::endpoint::Endpoint`] for more precision.
    ///
    /// # Examples
    ///
    /// Listen on port 6000, on an IPv4 endpoint, for all incoming IPs.
    /// ```
    /// use bevy_quinnet::server::ServerConfiguration;
    /// let config = ServerConfiguration::from_string("0.0.0.0:6000");
    /// ```
    /// Listen on port 6000, on an IPv6 endpoint, for all incoming IPs.
    /// ```
    /// use bevy_quinnet::server::ServerConfiguration;
    /// let config = ServerConfiguration::from_string("[::]:6000");
    /// ```
    pub fn from_string(local_bind_addr_str: &str) -> Result<Self, AddrParseError> {
        let local_bind_addr = local_bind_addr_str.parse()?;
        Ok(Self { local_bind_addr })
    }

    /// Creates a new ServerConfiguration
    ///
    /// # Arguments
    ///
    /// * `local_bind_ip` - Local IP address to bind to. The address should usually be a wildcard like `0.0.0.0` (for an IPv4) or `0:0:0:0:0:0:0:0` (for an IPv6), which allow communication with any reachable IPv4 or IPv6 address. See [`std::net::Ipv4Addr`] and [`std::net::Ipv6Addr`] for more precision.
    /// * `local_bind_port` - Local port to bind to.
    ///
    /// # Examples
    ///
    /// Listen on port 6000, on an IPv4 endpoint, for all incoming IPs.
    /// ```
    /// use std::net::{IpAddr, Ipv4Addr};
    /// use bevy_quinnet::server::ServerConfiguration;
    /// let config = ServerConfiguration::from_ip(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 6000);
    /// ```
    pub fn from_ip(local_bind_ip: IpAddr, local_bind_port: u16) -> Self {
        Self {
            local_bind_addr: SocketAddr::new(local_bind_ip, local_bind_port),
        }
    }

    /// Creates a new ServerConfiguration
    ///
    /// # Arguments
    ///
    /// * `local_bind_addr` - Local address and port to bind to.
    /// See [`std::net::SocketAddrV4`] and [`std::net::SocketAddrV6`] for more precision.
    ///
    /// # Examples
    ///
    /// Listen on port 6000, on an IPv4 endpoint, for all incoming IPs.
    /// ```
    /// use bevy_quinnet::server::ServerConfiguration;
    /// use std::{net::{IpAddr, Ipv4Addr, SocketAddr}};
    /// let config = ServerConfiguration::from_addr(
    ///           SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 6000),
    ///       );
    /// ```
    pub fn from_addr(local_bind_addr: SocketAddr) -> Self {
        Self { local_bind_addr }
    }
}

#[derive(Debug)]
pub(crate) enum ServerAsyncMessage {
    ClientConnected(ClientConnection),
    ClientConnectionClosed(ClientId, ConnectionError),
}

#[derive(Debug, Clone)]
pub(crate) enum ServerSyncMessage {
    ClientConnectedAck(Option<ClientId>),
}

#[derive(Debug)]
pub struct ClientConnection {
    connection_handle: InternalConnectionRef,
    channels: HashMap<ChannelId, Channel>,
    bytes_from_client_recv: mpsc::Receiver<Bytes>,
    close_sender: broadcast::Sender<()>,

    pub(crate) to_connection_send: mpsc::Sender<ServerSyncMessage>,
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

    /// Signal the connection to closes all its background tasks. Before trully closing, the connection will wait for all buffered messages in all its opened channels to be properly sent according to their respective channel type.
    pub(crate) fn close(&mut self) -> Result<(), QuinnetError> {
        match self.close_sender.send(()) {
            Ok(_) => Ok(()),
            Err(_) => {
                // The only possible error for a send is that there is no active receivers, meaning that the tasks are already terminated.
                Err(QuinnetError::ConnectionAlreadyClosed)
            }
        }
    }

    pub(crate) fn try_close(&mut self) {
        match &self.close() {
            Ok(_) => (),
            Err(err) => error!("Failed to properly close clonnection: {}", err),
        }
    }
}

/// By default, when starting an [Endpoint], Quinnet creates 1 channel instance of each [ChannelType], each with their own [ChannelId].
/// Among those, there is a `default` channel which will be used when you don't specify the channel. At startup, this default channel is a [ChannelType::OrderedReliable] channel.
pub struct Endpoint {
    clients: HashMap<ClientId, ClientConnection>,
    last_gen_client_id: ClientId,

    channels: HashSet<ChannelId>,
    default_channel: Option<ChannelId>,
    last_gen_id: MultiChannelId,

    close_sender: broadcast::Sender<()>,

    pub(crate) from_async_server_recv: mpsc::Receiver<ServerAsyncMessage>,
}

impl Endpoint {
    /// Returns a vec of all connected client ids
    pub fn clients(&self) -> Vec<ClientId> {
        self.clients.keys().cloned().collect()
    }

    /// Attempt to deserialise a message into type `T`.
    ///
    /// Will return [`Err`] if the bytes accumulated from the client aren't deserializable to T.
    /// /// Will also return [`Err`] if this client is disconnected.
    pub fn receive_message_from<T: serde::de::DeserializeOwned>(
        &mut self,
        client_id: ClientId,
    ) -> Result<Option<T>, QuinnetError> {
        match self.receive_payload_from(client_id)? {
            Some(payload) => match bincode::deserialize(&payload) {
                Ok(msg) => Ok(Some(msg)),
                Err(_) => Err(QuinnetError::Deserialization),
            },
            None => Ok(None),
        }
    }

    /// [`Endpoint::receive_message_from`] that logs the error instead of returning a result.
    pub fn try_receive_message_from<T: serde::de::DeserializeOwned>(
        &mut self,
        client_id: ClientId,
    ) -> Option<T> {
        match self.receive_message_from(client_id) {
            Ok(message) => message,
            Err(err) => {
                error!("try_receive_message: {}", err);
                None
            }
        }
    }

    /// Attempt to receive a full payload sent by the specified client.
    pub fn receive_payload_from(
        &mut self,
        client_id: ClientId,
    ) -> Result<Option<Bytes>, QuinnetError> {
        match self.clients.get_mut(&client_id) {
            Some(client) => match client.bytes_from_client_recv.try_recv() {
                Ok(msg) => Ok(Some(msg)),
                Err(err) => match err {
                    TryRecvError::Empty => Ok(None),
                    TryRecvError::Disconnected => Err(QuinnetError::InternalChannelClosed),
                },
            },
            None => Err(QuinnetError::UnknownClient(client_id)),
        }
    }

    /// [`Endpoint::receive_payload_from`] that logs the error instead of returning a result.
    pub fn try_receive_payload_from(&mut self, client_id: ClientId) -> Option<Bytes> {
        match self.receive_payload_from(client_id) {
            Ok(payload) => payload,
            Err(err) => {
                error!("try_receive_payload: {}", err);
                None
            }
        }
    }

    /// Queue a message to be sent to the specified client on the default channel.
    pub fn send_message<T: serde::Serialize>(
        &self,
        client_id: ClientId,
        message: T,
    ) -> Result<(), QuinnetError> {
        match self.default_channel {
            Some(channel) => self.send_message_on(client_id, channel, message),
            None => Err(QuinnetError::NoDefaultChannel),
        }
    }

    /// Queue a message to be sent to the specified client on the specified channel.
    pub fn send_message_on<T: serde::Serialize>(
        &self,
        client_id: ClientId,
        channel_id: ChannelId,
        message: T,
    ) -> Result<(), QuinnetError> {
        match bincode::serialize(&message) {
            Ok(payload) => Ok(self.send_payload_on(client_id, channel_id, payload)?),
            Err(_) => Err(QuinnetError::Serialization),
        }
    }

    /// [`Endpoint::send_message`] that logs the error instead of returning a result.
    pub fn try_send_message<T: serde::Serialize>(&self, client_id: ClientId, message: T) {
        match self.send_message(client_id, message) {
            Ok(_) => {}
            Err(err) => error!("try_send_message: {}", err),
        }
    }

    /// [`Endpoint::send_message_on`] that logs the error instead of returning a result.
    pub fn try_send_message_on<T: serde::Serialize>(
        &self,
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
                let bytes = Bytes::from(payload);
                for id in client_ids {
                    self.send_payload_on(*id, channel_id, bytes.clone())?;
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

    pub fn try_broadcast_message_on<T: serde::Serialize>(&self, channel_id: ChannelId, message: T) {
        match self.broadcast_message_on(channel_id, message) {
            Ok(_) => {}
            Err(err) => error!("try_broadcast_message: {}", err),
        }
    }

    /// Sends the payload to all connected clients on the default channel.
    pub fn broadcast_payload<T: Into<Bytes> + Clone>(
        &self,
        payload: T,
    ) -> Result<(), QuinnetError> {
        match self.default_channel {
            Some(channel) => self.broadcast_payload_on(channel, payload),
            None => Err(QuinnetError::NoDefaultChannel),
        }
    }

    /// Sends the payload to all connected clients on the specified channel.
    pub fn broadcast_payload_on<T: Into<Bytes> + Clone>(
        &self,
        channel_id: ChannelId,
        payload: T,
    ) -> Result<(), QuinnetError> {
        let payload: Bytes = payload.into();
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
                Some(channel) => channel.send_payload(payload.into()),
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

    /// Disconnect a specific client. Removes it from the server.
    ///
    /// Disconnecting a client immediately prevents new messages from being sent on its connection and signal the underlying connection to closes all its background tasks. Before trully closing, the connection will wait for all buffered messages in all its opened channels to be properly sent according to their respective channel type.
    ///
    /// This may fail if no client if found for client_id, or if the client is already disconnected.
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

    /// Calls disconnect_client on all onnected clients
    pub fn disconnect_all_clients(&mut self) -> Result<(), QuinnetError> {
        for client_id in self.clients.keys().cloned().collect::<Vec<ClientId>>() {
            self.disconnect_client(client_id)?;
        }
        Ok(())
    }

    /// Returns statistics about a client if connected.
    pub fn stats(&self, client_id: ClientId) -> Option<ConnectionStats> {
        match &self.clients.get(&client_id) {
            Some(client) => Some(client.connection_handle.stats()),
            None => None,
        }
    }

    /// Opens a channel of the requested [ChannelType] and returns its [ChannelId].
    ///
    /// If no channels were previously opened, the opened channel will be the new default channel.
    ///
    /// Can fail if the Endpoint is closed. See [Endpoint] for the defautl channels.
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

    fn handle_connection(
        &mut self,
        mut connection: ClientConnection,
    ) -> Result<ClientId, QuinnetError> {
        for channel_id in self.channels.iter() {
            if let Err(err) = connection.create_channel(*channel_id) {
                connection.try_close();
                return Err(err);
            };
        }

        self.last_gen_client_id += 1;
        let client_id = self.last_gen_client_id;

        match connection
            .to_connection_send
            .try_send(ServerSyncMessage::ClientConnectedAck(Some(client_id)))
        {
            Ok(_) => {
                self.clients.insert(client_id, connection);
                Ok(client_id)
            }
            Err(_) => {
                connection.try_close();
                Err(QuinnetError::InternalChannelClosed)
            }
        }
    }
}

#[derive(Resource)]
pub struct Server {
    runtime: runtime::Handle,
    endpoint: Option<Endpoint>,
}

impl FromWorld for Server {
    fn from_world(world: &mut World) -> Self {
        if world.get_resource::<AsyncRuntime>().is_none() {
            let async_runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            world.insert_resource(AsyncRuntime(async_runtime));
        };

        let runtime = world.resource::<AsyncRuntime>();
        Server::new(runtime.handle().clone())
    }
}

impl Server {
    fn new(runtime: tokio::runtime::Handle) -> Self {
        Self {
            endpoint: None,
            runtime,
        }
    }

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

    /// Starts a new endpoint with the given [ServerConfiguration] and [CertificateRetrievalMode] and opens the default channels.
    ///
    /// Returns a tuple of the [ServerCertificate] generated or loaded, and the default [ChannelId]
    pub fn start_endpoint(
        &mut self,
        config: ServerConfiguration,
        cert_mode: CertificateRetrievalMode,
    ) -> Result<(ServerCertificate, ChannelId), QuinnetError> {
        // Endpoint configuration
        let server_cert = retrieve_certificate(cert_mode)?;
        let mut server_config = ServerConfig::with_single_cert(
            server_cert.cert_chain.clone(),
            server_cert.priv_key.clone(),
        )?;
        Arc::get_mut(&mut server_config.transport)
            .ok_or(QuinnetError::LockAcquisitionFailure)?
            .keep_alive_interval(Some(Duration::from_secs(DEFAULT_KEEP_ALIVE_INTERVAL_S)));

        let (to_sync_server_send, from_async_server_recv) =
            mpsc::channel::<ServerAsyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);
        let (endpoint_close_send, endpoint_close_recv) =
            broadcast::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);

        info!("Starting endpoint on: {} ...", config.local_bind_addr);

        self.runtime.spawn(async move {
            endpoint_task(
                server_config,
                config.local_bind_addr,
                to_sync_server_send.clone(),
                endpoint_close_recv,
            )
            .await;
        });

        let mut endpoint = Endpoint {
            clients: HashMap::new(),
            last_gen_client_id: 0,
            channels: HashSet::new(),
            default_channel: None,
            last_gen_id: 0,
            close_sender: endpoint_close_send,
            from_async_server_recv,
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
) {
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
                        let to_sync_server_send = to_sync_server_send.clone();
                        tokio::spawn(async move {
                            client_connection_task(
                                connection,
                                to_sync_server_send
                            )
                            .await
                        });
                    },
                }
            }
        } => {}
    }
}

async fn client_connection_task(
    connection: quinn::Connection,
    to_sync_server_send: mpsc::Sender<ServerAsyncMessage>,
) {
    let (client_close_send, client_close_recv) =
        broadcast::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);
    let (bytes_from_client_send, bytes_from_client_recv) =
        mpsc::channel::<Bytes>(DEFAULT_MESSAGE_QUEUE_SIZE);
    let (to_connection_send, mut from_sync_server_recv) =
        mpsc::channel::<ServerSyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);
    let (from_channels_send, from_channels_recv) =
        mpsc::channel::<ChannelAsyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);
    let (to_channels_send, to_channels_recv) =
        mpsc::channel::<ChannelSyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);

    // Signal the sync server of this new connection
    to_sync_server_send
        .send(ServerAsyncMessage::ClientConnected(ClientConnection {
            connection_handle: connection.clone(),
            channels: HashMap::new(),
            bytes_from_client_recv,
            close_sender: client_close_send.clone(),
            to_connection_send,
            from_channels_recv,
            to_channels_send,
        }))
        .await
        .expect("Failed to signal connection to sync client");

    // Wait for the sync server response before spawning connection tasks.
    match from_sync_server_recv.recv().await {
        Some(ServerSyncMessage::ClientConnectedAck(Some(client_id))) => {
            info!(
                "New connection from {}, client_id: {}",
                connection.remote_address(),
                client_id
            );

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
                let connection_handle = connection.clone();
                let client_close_recv = client_close_recv.resubscribe();
                let bytes_incoming_send = bytes_from_client_send.clone();
                tokio::spawn(async move {
                    reliable_receiver_task(
                        client_id,
                        connection_handle,
                        client_close_recv,
                        bytes_incoming_send,
                    )
                    .await
                });
            }

            // Spawn a task to listen for datagrams sent by this client
            {
                let connection_handle = connection.clone();
                let client_close_recv = client_close_recv.resubscribe();
                let bytes_incoming_send = bytes_from_client_send.clone();
                tokio::spawn(async move {
                    unreliable_receiver_task(
                        client_id,
                        connection_handle,
                        client_close_recv,
                        bytes_incoming_send,
                    )
                    .await
                });
            }

            // Spawn a task to handle send channels for this client
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
        _ => info!("Connection from {} refused", connection.remote_address()),
    }
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
                    match endpoint.handle_connection(connection) {
                        Ok(client_id) => connection_events.send(ConnectionEvent { id: client_id }),
                        Err(_) => error!("Failed to handle connection of a client"),
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

pub struct QuinnetServerPlugin {
    /// In order to have more control and only do the strict necessary, which is registering systems and events in the Bevy schedule, `initialize_later` can be set to `true`. This will prevent the plugin from initializing the `Server` Resource.
    /// Server systems are scheduled to only run if the `Server` resource exists.
    /// A Bevy command to create the resource `commands.init_resource::<Server>();` can be done later on, when needed.
    pub initialize_later: bool,
}

impl Default for QuinnetServerPlugin {
    fn default() -> Self {
        Self {
            initialize_later: false,
        }
    }
}

impl Plugin for QuinnetServerPlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<ConnectionEvent>()
            .add_event::<ConnectionLostEvent>();

        if !self.initialize_later {
            app.init_resource::<Server>();
        }

        app.add_system(
            update_sync_server
                .in_base_set(CoreSet::PreUpdate)
                .run_if(resource_exists::<Server>()),
        );
    }
}
