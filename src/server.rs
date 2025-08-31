use std::{
    collections::{BTreeSet, HashMap, HashSet},
    net::{AddrParseError, IpAddr, SocketAddr, UdpSocket},
    sync::Arc,
};

use bevy::prelude::*;
use bytes::Bytes;
use quinn::{default_runtime, Endpoint as QuinnEndpoint, EndpointConfig, ServerConfig};
use quinn_proto::ConnectionStats;
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
    server::certificate::{retrieve_certificate, CertificateRetrievalMode, ServerCertificate},
    shared::{
        channels::{
            spawn_recv_channels_tasks, spawn_send_channels_tasks_spawner, Channel,
            ChannelAsyncMessage, ChannelId, ChannelKind, ChannelSyncMessage, ChannelsConfiguration,
            CloseReason,
        },
        error::{AsyncChannelError, ChannelCloseError, ChannelCreationError},
        AsyncRuntime, ClientId, InternalConnectionRef, QuinnetSyncUpdate,
        DEFAULT_INTERNAL_MESSAGES_CHANNEL_SIZE, DEFAULT_KEEP_ALIVE_INTERVAL_S,
        DEFAULT_KILL_MESSAGE_QUEUE_SIZE, DEFAULT_MESSAGE_QUEUE_SIZE,
        DEFAULT_QCHANNEL_MESSAGES_CHANNEL_SIZE,
    },
};

#[cfg(feature = "shared-client-id")]
use crate::server::client_id::spawn_client_id_sender;

#[cfg(feature = "shared-client-id")]
mod client_id;

mod error;
pub use error::*;

/// Module for the server's certificate features
pub mod certificate;

/// Connection event raised when a client just connected to the server. Raised in the CoreStage::PreUpdate stage.
#[derive(Event, Debug, Copy, Clone)]
pub struct ConnectionEvent {
    /// Id of the client who connected
    pub id: ClientId,
}

/// ConnectionLost event raised when a client is considered disconnected from the server. Raised in the CoreStage::PreUpdate stage.
#[derive(Event, Debug, Copy, Clone)]
pub struct ConnectionLostEvent {
    /// Id of the client who lost connection
    pub id: ClientId,
}

/// Configuration of the server, used when the server starts an Endpoint
#[derive(Debug, Clone)]
pub struct ServerEndpointConfiguration {
    local_bind_addr: SocketAddr,
}

impl ServerEndpointConfiguration {
    /// Creates a new ServerEndpointConfiguration
    ///
    /// # Arguments
    ///
    /// * `local_bind_addr_str` - Local address and port to bind to separated by `:`. The address should usually be a wildcard like `0.0.0.0` (for an IPv4) or `[::]` (for an IPv6), which allow communication with any reachable IPv4 or IPv6 address. See [`std::net::SocketAddrV4`] and [`std::net::SocketAddrV6`] or [`quinn::Endpoint`] for more precision.
    ///
    /// # Examples
    ///
    /// Listen on port 6000, on an IPv4 endpoint, for all incoming IPs.
    /// ```
    /// use bevy_quinnet::server::ServerEndpointConfiguration;
    /// let config = ServerEndpointConfiguration::from_string("0.0.0.0:6000");
    /// ```
    /// Listen on port 6000, on an IPv6 endpoint, for all incoming IPs.
    /// ```
    /// use bevy_quinnet::server::ServerEndpointConfiguration;
    /// let config = ServerEndpointConfiguration::from_string("[::]:6000");
    /// ```
    pub fn from_string(local_bind_addr_str: &str) -> Result<Self, AddrParseError> {
        let local_bind_addr = local_bind_addr_str.parse()?;
        Ok(Self { local_bind_addr })
    }

    /// Creates a new ServerEndpointConfiguration
    ///
    /// # Arguments
    ///
    /// * `local_bind_ip` - Local IP address to bind to. The address should usually be a wildcard like `0.0.0.0` (for an IPv4) or `0:0:0:0:0:0:0:0` (for an IPv6), which allow communication with any reachable IPv4 or IPv6 address. See [`std::net::Ipv4Addr`] and [`std::net::Ipv6Addr`] for more precision.
    /// * `local_bind_port` - Local port to bind to.
    ///
    /// # Examples
    ///
    /// Listen on port 6000, on an IPv6 endpoint, for all incoming IPs.
    /// ```
    /// use std::net::Ipv6Addr;
    /// use bevy_quinnet::server::ServerEndpointConfiguration;
    /// let config = ServerEndpointConfiguration::from_ip(Ipv6Addr::UNSPECIFIED, 6000);
    /// ```
    pub fn from_ip(local_bind_ip: impl Into<IpAddr>, local_bind_port: u16) -> Self {
        Self {
            local_bind_addr: SocketAddr::new(local_bind_ip.into(), local_bind_port),
        }
    }

    /// Creates a new ServerEndpointConfiguration
    ///
    /// # Arguments
    ///
    /// * `local_bind_addr` - Local address and port to bind to.
    /// See [`std::net::SocketAddrV4`] and [`std::net::SocketAddrV6`] for more precision.
    ///
    /// # Examples
    ///
    /// Listen on port 6000, on an IPv6 endpoint, for all incoming IPs.
    /// ```
    /// use bevy_quinnet::server::ServerEndpointConfiguration;
    /// use std::{net::{IpAddr, Ipv4Addr, SocketAddr}};
    /// let config = ServerEndpointConfiguration::from_addr(
    ///           SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 6000),
    ///       );
    /// ```
    pub fn from_addr(local_bind_addr: SocketAddr) -> Self {
        Self { local_bind_addr }
    }
}

#[derive(Debug)]
pub(crate) enum ServerAsyncMessage {
    ClientConnected(ServerSideConnection),
    ClientConnectionClosed(ClientId), // TODO Might add a ConnectionError
}

#[derive(Debug, Clone)]
pub(crate) enum ServerSyncMessage {
    ClientConnectedAck(ClientId),
}

/// Represents a connection from a quinnet client to a server's [`Endpoint`], viewed from the server.
#[derive(Debug)]
pub struct ServerSideConnection {
    connection_handle: InternalConnectionRef,

    channels: Vec<Option<Channel>>,
    bytes_from_client_recv: mpsc::Receiver<(ChannelId, Bytes)>,
    close_sender: broadcast::Sender<CloseReason>,

    pub(crate) to_connection_send: mpsc::Sender<ServerSyncMessage>,
    pub(crate) to_channels_send: mpsc::Sender<ChannelSyncMessage>,
    pub(crate) from_channels_recv: mpsc::Receiver<ChannelAsyncMessage>,

    received_bytes_count: usize,
    sent_bytes_count: usize,
}

impl ServerSideConnection {
    fn new(
        connection_handle: InternalConnectionRef,
        bytes_from_client_recv: mpsc::Receiver<(ChannelId, Bytes)>,
        close_sender: broadcast::Sender<CloseReason>,
        to_connection_send: mpsc::Sender<ServerSyncMessage>,
        from_channels_recv: mpsc::Receiver<ChannelAsyncMessage>,
        to_channels_send: mpsc::Sender<ChannelSyncMessage>,
    ) -> Self {
        Self {
            connection_handle,
            bytes_from_client_recv,
            close_sender,
            to_connection_send,
            to_channels_send,
            from_channels_recv,
            channels: Vec::new(),
            received_bytes_count: 0,
            sent_bytes_count: 0,
        }
    }

    /// Immediately prevents new messages from being sent on the channel and signal the channel to closes all its background tasks.
    /// Before trully closing, the channel will wait for all buffered messages to be properly sent according to the channel type.
    /// Can fail if the [ChannelId] is unknown, or if the channel is already closed.
    pub(crate) fn close_channel(&mut self, channel_id: ChannelId) -> Result<(), ChannelCloseError> {
        if (channel_id as usize) < self.channels.len() {
            match self.channels[channel_id as usize].take() {
                Some(channel) => channel.close(),
                None => Err(ChannelCloseError::ChannelAlreadyClosed),
            }
        } else {
            Err(ChannelCloseError::InvalidChannelId(channel_id))
        }
    }

    pub(crate) fn create_connection_channel(
        &mut self,
        id: ChannelId,
        kind: ChannelKind,
    ) -> Result<(), AsyncChannelError> {
        let channel = self.create_unregistered_connection_channel(id, kind)?;
        self.register_connection_channel(channel);
        Ok(())
    }

    pub(crate) fn create_unregistered_connection_channel(
        &mut self,
        id: ChannelId,
        kind: ChannelKind,
    ) -> Result<Channel, AsyncChannelError> {
        let (bytes_to_channel_send, bytes_to_channel_recv) =
            mpsc::channel::<Bytes>(DEFAULT_MESSAGE_QUEUE_SIZE);
        let (channel_close_send, channel_close_recv) =
            mpsc::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);

        match self
            .to_channels_send
            .try_send(ChannelSyncMessage::CreateChannel {
                id,
                kind,
                bytes_to_channel_recv,
                channel_close_recv,
            }) {
            Ok(_) => Ok(Channel::new(id, bytes_to_channel_send, channel_close_send)),
            Err(err) => match err {
                TrySendError::Full(_) => Err(AsyncChannelError::FullQueue),
                TrySendError::Closed(_) => Err(AsyncChannelError::InternalChannelClosed),
            },
        }
    }

    pub(crate) fn register_connection_channel(&mut self, channel: Channel) {
        let channel_index = channel.id() as usize;
        if channel_index < self.channels.len() {
            self.channels[channel_index] = Some(channel);
        } else {
            self.channels
                .extend((self.channels.len()..channel_index).map(|_| None));
            self.channels.push(Some(channel));
        }
    }

    /// Signal the connection to closes all its background tasks. Before trully closing, the connection will wait for all buffered messages in all its opened channels to be properly sent according to their respective channel type.
    pub(crate) fn close(&mut self) -> Result<(), EndpointConnectionAlreadyClosed> {
        match self.close_sender.send(CloseReason::LocalOrder) {
            Ok(_) => Ok(()),
            Err(_) => {
                // The only possible error for a send is that there is no active receivers, meaning that the tasks are already terminated.
                Err(EndpointConnectionAlreadyClosed)
            }
        }
    }

    pub(crate) fn try_close(&mut self) {
        match &self.close() {
            Ok(_) => (),
            Err(err) => error!("Failed to properly close clonnection: {}", err),
        }
    }

    /// See [quinn::Connection::max_datagram_size]
    pub fn max_datagram_size(&self) -> Option<usize> {
        self.connection_handle.max_datagram_size()
    }

    /// Returns statistics about a client connection
    pub fn connection_stats(&self) -> ConnectionStats {
        self.connection_handle.stats()
    }

    /// Returns how many bytes were received on this connection since the last time it was cleared and reset this value to 0
    pub fn clear_received_bytes_count(&mut self) -> usize {
        let bytes_count = self.received_bytes_count;
        self.received_bytes_count = 0;
        bytes_count
    }

    /// Returns how many bytes were received on this connection since the last time it was cleared
    pub fn received_bytes_count(&self) -> usize {
        self.received_bytes_count
    }

    /// Returns how many bytes were received on this connection since the last time it was cleared and reset this value to 0
    pub fn clear_sent_bytes_count(&mut self) -> usize {
        let bytes_count = self.sent_bytes_count;
        self.sent_bytes_count = 0;
        bytes_count
    }

    /// Returns how many bytes were received on this connection since the last time it was cleared
    pub fn sent_bytes_count(&self) -> usize {
        self.sent_bytes_count
    }
}

/// By default, when starting an [Endpoint], Quinnet creates 1 channel instance of each [ChannelKind], each with their own [ChannelId].
/// Among those, there is a `default` channel which will be used when you don't specify the channel. At startup, this default channel is a [ChannelKind::OrderedReliable] channel.
pub struct Endpoint {
    clients: HashMap<ClientId, ServerSideConnection>,
    client_id_gen: ClientId,

    opened_channels: HashMap<ChannelId, ChannelKind>,
    available_channel_ids: BTreeSet<ChannelId>,
    default_channel: Option<ChannelId>,

    close_sender: broadcast::Sender<()>,

    from_async_endpoint_recv: mpsc::Receiver<ServerAsyncMessage>,

    stats: EndpointStats,
}

/// Basic quinnet stats about this server endpoint
#[derive(Default)]
pub struct EndpointStats {
    received_messages_count: u64,
    connect_count: u32,
    disconnect_count: u32,
}
impl EndpointStats {
    /// Returns how many messages were received (read) on this endpoint
    pub fn received_messages_count(&self) -> u64 {
        self.received_messages_count
    }
    /// Returns how many connection events occurred on this endpoint
    pub fn connect_count(&self) -> u32 {
        self.connect_count
    }
    /// Returns how many disconnections events occurred on this endpoint
    pub fn disconnect_count(&self) -> u32 {
        self.disconnect_count
    }
}

impl Endpoint {
    fn new(
        endpoint_close_send: broadcast::Sender<()>,
        from_async_endpoint_recv: mpsc::Receiver<ServerAsyncMessage>,
    ) -> Self {
        Self {
            clients: HashMap::new(),
            client_id_gen: 0,
            opened_channels: HashMap::new(),
            default_channel: None,
            available_channel_ids: (0..255).collect(),
            close_sender: endpoint_close_send,
            from_async_endpoint_recv,
            stats: default(),
        }
    }

    /// Returns a vec of all connected client ids
    pub fn clients(&self) -> Vec<ClientId> {
        self.clients.keys().cloned().collect()
    }

    /// Attempts to receive a full payload sent by the specified client.
    ///
    /// - Returns an [`Ok`] result containg [`Some`] if there is a message from the client in the message buffer
    /// - Returns an [`Ok`] result containg [`None`] if there is no message from the client in the message buffer
    /// - Can return an [`Err`] if:
    ///   - the connection is closed
    ///   - the client id is not valid
    pub fn receive_payload_from(
        &mut self,
        client_id: ClientId,
    ) -> Result<Option<(ChannelId, Bytes)>, ServerReceiveError> {
        match self.clients.get_mut(&client_id) {
            Some(client) => match client.bytes_from_client_recv.try_recv() {
                Ok(msg) => {
                    self.stats.received_messages_count += 1;
                    client.received_bytes_count += msg.1.len();
                    Ok(Some(msg))
                }
                Err(err) => match err {
                    TryRecvError::Empty => Ok(None),
                    TryRecvError::Disconnected => Err(ServerReceiveError::ConnectionClosed),
                },
            },
            None => Err(ServerReceiveError::UnknownClient(client_id)),
        }
    }

    /// [`Endpoint::receive_payload_from`] that logs the error instead of returning a result.
    pub fn try_receive_payload_from(&mut self, client_id: ClientId) -> Option<(ChannelId, Bytes)> {
        match self.receive_payload_from(client_id) {
            Ok(payload) => payload,
            Err(err) => {
                error!("try_receive_payload: {}", err);
                None
            }
        }
    }

    /// Same as [Endpoint::send_group_payload_on] but on the default channel
    pub fn send_group_payload<'a, I: Iterator<Item = &'a ClientId>, T: Into<Bytes>>(
        &mut self,
        client_ids: I,
        payload: T,
    ) -> Result<(), ServerGroupPayloadSendError> {
        match self.default_channel {
            Some(channel) => self.send_group_payload_on(client_ids, channel, payload),
            None => Err(ServerGroupPayloadSendError::NoDefaultChannel),
        }
    }

    /// [`Endpoint::send_group_payload`] that logs the error instead of returning a result.
    pub fn try_send_group_payload<'a, I: Iterator<Item = &'a ClientId>, T: Into<Bytes>>(
        &mut self,
        client_ids: I,
        payload: T,
    ) {
        if let Err(err) = self.send_group_payload(client_ids, payload) {
            error!("try_send_group_payload: {}", err);
        }
    }

    /// Sends the payload to the specified clients on the specified channel.
    ///
    /// Tries to send to each client before returning. Returns an [`Err`] if sending failed for at least 1 client. Information about the failed sendings will be available in the [`ServerGroupMessageSendError`].
    pub fn send_group_payload_on<
        'a,
        I: Iterator<Item = &'a ClientId>,
        T: Into<Bytes>,
        C: Into<ChannelId>,
    >(
        &mut self,
        client_ids: I,
        channel_id: C,
        payload: T,
    ) -> Result<(), ServerGroupPayloadSendError> {
        let channel_id = channel_id.into();
        let bytes = payload.into();
        let mut errs = vec![];

        for &client_id in client_ids {
            if let Err(e) = self.send_payload_on(client_id, channel_id, bytes.clone()) {
                errs.push((client_id, e.into()));
            }
        }

        match errs.is_empty() {
            true => Ok(()),
            false => Err(ServerGroupSendError(errs).into()),
        }
    }

    /// [`Endpoint::send_group_payload_on`] that logs the error instead of returning a result.
    pub fn try_send_group_payload_on<
        'a,
        I: Iterator<Item = &'a ClientId>,
        T: Into<Bytes>,
        C: Into<ChannelId>,
    >(
        &mut self,
        client_ids: I,
        channel_id: C,
        payload: T,
    ) {
        if let Err(err) = self.send_group_payload_on(client_ids, channel_id, payload) {
            error!("try_send_group_payload_on: {}", err);
        }
    }

    /// Same as [Endpoint::broadcast_payload_on] but on the default channel
    pub fn broadcast_payload<T: Into<Bytes>>(
        &mut self,
        payload: T,
    ) -> Result<(), ServerGroupPayloadSendError> {
        match self.default_channel {
            Some(channel) => Ok(self.broadcast_payload_on(channel, payload)?),
            None => Err(ServerGroupPayloadSendError::NoDefaultChannel),
        }
    }

    /// Sends the payload to all connected clients on the specified channel.
    ///
    /// Tries to send to each client before returning. Returns an [`Err`] if sending failed for at least 1 client. Information about the failed sendings will be available in the [`ServerGroupSendError`].
    pub fn broadcast_payload_on<T: Into<Bytes>, C: Into<ChannelId>>(
        &mut self,
        channel_id: C,
        payload: T,
    ) -> Result<(), ServerGroupSendError> {
        let payload: Bytes = payload.into();
        let channel_id = channel_id.into();

        let mut errs = vec![];
        for (&client_id, server_side_connection) in self.clients.iter_mut() {
            if let Err(e) =
                Self::internal_send_payload(server_side_connection, channel_id, payload.clone())
            {
                errs.push((client_id, e.into()));
            }
        }
        match errs.is_empty() {
            true => Ok(()),
            false => Err(ServerGroupSendError(errs)),
        }
    }

    /// Same as [Endpoint::broadcast_payload] but will log the error instead of returning it
    pub fn try_broadcast_payload<T: Into<Bytes>>(&mut self, payload: T) {
        if let Err(err) = self.broadcast_payload(payload) {
            error!("try_broadcast_payload: {}", err);
        }
    }

    /// Same as [Endpoint::broadcast_payload_on] but will log the error instead of returning it
    pub fn try_broadcast_payload_on<T: Into<Bytes>, C: Into<ChannelId>>(
        &mut self,
        channel_id: C,
        payload: T,
    ) {
        if let Err(err) = self.broadcast_payload_on(channel_id, payload) {
            error!("try_broadcast_payload_on: {}", err);
        }
    }

    /// Same as [Endpoint::send_payload] but on the default channel
    pub fn send_payload<T: Into<Bytes>>(
        &mut self,
        client_id: ClientId,
        payload: T,
    ) -> Result<(), ServerPayloadSendError> {
        match self.default_channel {
            Some(channel) => Ok(self.send_payload_on(client_id, channel, payload)?),
            None => Err(ServerPayloadSendError::NoDefaultChannel.into()),
        }
    }

    /// Sends the payload to the specified client on the specified channel
    ///
    /// Will return an [`Err`] if:
    /// - the channel does not exist/is closed
    /// - or if the client is disconnected
    /// - (or if the message queue is full)
    pub fn send_payload_on<T: Into<Bytes>, C: Into<ChannelId>>(
        &mut self,
        client_id: ClientId,
        channel_id: C,
        payload: T,
    ) -> Result<(), ServerSendError> {
        if let Some(client_connection) = self.clients.get_mut(&client_id) {
            let channel_id = channel_id.into();
            Self::internal_send_payload(client_connection, channel_id, payload.into())
        } else {
            Err(ServerSendError::UnknownClient(client_id))
        }
    }

    fn internal_send_payload(
        client_connection: &mut ServerSideConnection,
        channel_id: ChannelId,
        payload: Bytes,
    ) -> Result<(), ServerSendError> {
        match client_connection.channels.get(channel_id as usize) {
            Some(Some(channel)) => {
                client_connection.sent_bytes_count += payload.len();
                Ok(channel.send_payload(payload)?)
            }
            Some(None) => return Err(ServerSendError::ChannelClosed),
            None => return Err(ServerSendError::InvalidChannelId(channel_id)),
        }
    }

    /// Same as [Endpoint::send_payload] but will log the error instead of returning it
    pub fn try_send_payload<T: Into<Bytes>>(&mut self, client_id: ClientId, payload: T) {
        match self.send_payload(client_id, payload) {
            Ok(_) => {}
            Err(err) => error!("try_send_payload: {}", err),
        }
    }

    /// Same as [Endpoint::send_payload_on] but will log the error instead of returning it
    pub fn try_send_payload_on<T: Into<Bytes>, C: Into<ChannelId>>(
        &mut self,
        client_id: ClientId,
        channel_id: C,
        payload: T,
    ) {
        match self.send_payload_on(client_id, channel_id, payload) {
            Ok(_) => {}
            Err(err) => error!("try_send_payload_on: {}", err),
        }
    }

    fn internal_disconnect_client(
        &mut self,
        client_id: ClientId,
        reason: CloseReason,
    ) -> Result<(), ServerDisconnectError> {
        match self.clients.remove(&client_id) {
            Some(client_connection) => match client_connection.close_sender.send(reason) {
                Ok(_) => Ok(()),
                Err(_) => Err(ServerDisconnectError::ClientAlreadyDisconnected(client_id)),
            },
            None => Err(ServerDisconnectError::UnknownClient(client_id)),
        }
    }

    /// Logical "Disconnect", the client already closed/lost the connection.
    fn try_disconnect_closed_client(&mut self, client_id: ClientId) {
        if let Err(err) = self.internal_disconnect_client(client_id, CloseReason::PeerClosed) {
            error!(
                "Failed to properly disconnect client {}: {}",
                client_id, err
            );
        }
    }

    /// Disconnect a specific client. Removes it from the server.
    ///
    /// Disconnecting a client immediately prevents new messages from being sent on its connection and signal the underlying connection to closes all its background tasks. Before trully closing, the connection will wait for all buffered messages in all its opened channels to be properly sent according to their respective channel type.
    ///
    /// This may fail if no client if found for client_id, or if the client is already disconnected.
    pub fn disconnect_client(&mut self, client_id: ClientId) -> Result<(), ServerDisconnectError> {
        self.internal_disconnect_client(client_id, CloseReason::LocalOrder)
    }

    /// Same as [Endpoint::disconnect_client] but errors are logged instead of returned
    pub fn try_disconnect_client(&mut self, client_id: ClientId) {
        if let Err(err) = self.disconnect_client(client_id) {
            error!(
                "Failed to properly disconnect client {}: {}",
                client_id, err
            );
        }
    }

    /// Disconnects all connect clients
    pub fn disconnect_all_clients(&mut self) {
        for (_, client_connection) in self.clients.drain() {
            let _ = client_connection.close_sender.send(CloseReason::LocalOrder);
        }
    }

    /// Returns statistics about a client if connected.
    pub fn get_connection_stats(&self, client_id: ClientId) -> Option<ConnectionStats> {
        match &self.clients.get(&client_id) {
            Some(client) => Some(client.connection_stats()),
            None => None,
        }
    }

    /// Returns a mutable reference to a client connection if it exists
    pub fn get_connection_mut(&mut self, client_id: ClientId) -> Option<&mut ServerSideConnection> {
        match self.clients.get_mut(&client_id) {
            Some(client_connection) => Some(client_connection),
            None => None,
        }
    }

    /// Returns a reference to a client connection if it exists
    pub fn get_connection(&self, client_id: ClientId) -> Option<&ServerSideConnection> {
        match self.clients.get(&client_id) {
            Some(client_connection) => Some(client_connection),
            None => None,
        }
    }

    /// Returns statistics about the server's endpoint
    pub fn endpoint_stats(&self) -> &EndpointStats {
        &self.stats
    }

    /// Opens a channel of the requested [ChannelKind] and returns its [ChannelId].
    ///
    /// If no channels were previously opened, the opened channel will be the new default channel.
    ///
    /// Can fail if the Endpoint is closed or if too many channels are already opened.
    pub fn open_channel(
        &mut self,
        channel_type: ChannelKind,
    ) -> Result<ChannelId, ChannelCreationError> {
        let channel_id = match self.available_channel_ids.pop_first() {
            Some(channel_id) => channel_id,
            None => return Err(ChannelCreationError::MaxChannelsCountReached),
        };
        match self.create_endpoint_channel(channel_id, channel_type) {
            Ok(channel_id) => Ok(channel_id),
            Err(err) => {
                self.available_channel_ids.insert(channel_id);
                Err(err.into())
            }
        }
    }

    /// Assumes presence of available ids in `available_channel_ids`
    fn unchecked_open_channel(
        &mut self,
        channel_type: ChannelKind,
    ) -> Result<ChannelId, AsyncChannelError> {
        let channel_id = self.available_channel_ids.pop_first().unwrap();
        match self.create_endpoint_channel(channel_id, channel_type) {
            Ok(channel_id) => Ok(channel_id),
            Err(err) => {
                self.available_channel_ids.insert(channel_id);
                Err(err)
            }
        }
    }

    /// `channel_id` must be an available [ChannelId]
    fn create_endpoint_channel(
        &mut self,
        channel_id: ChannelId,
        channel_type: ChannelKind,
    ) -> Result<ChannelId, AsyncChannelError> {
        let unregistered_channels =
            self.create_unregistered_endpoint_channels(channel_id, channel_type)?;
        // Only commit the changes once all channels have been confirmed to be created.
        for (client_id, channel) in unregistered_channels {
            self.clients
                .get_mut(&client_id)
                .unwrap()
                .register_connection_channel(channel);
        }
        self.opened_channels.insert(channel_id, channel_type);
        if self.default_channel.is_none() {
            self.default_channel = Some(channel_id);
        }
        Ok(channel_id)
    }

    fn create_unregistered_endpoint_channels(
        &mut self,
        channel_id: ChannelId,
        channel_type: ChannelKind,
    ) -> Result<HashMap<ClientId, Channel>, AsyncChannelError> {
        let mut unregistered_channels = HashMap::new();
        for (&client_id, client_connection) in self.clients.iter_mut() {
            // Unregistered channels are dropped here on error, created async tasks are closing too.
            let channel = client_connection
                .create_unregistered_connection_channel(channel_id, channel_type)?;
            unregistered_channels.insert(client_id, channel);
        }
        Ok(unregistered_channels)
    }

    /// Closes the channel with the corresponding [ChannelId].
    ///
    /// No new messages will be able to be sent on this channel, however, the channel will properly try to send all the messages that were previously pushed to it, according to its [ChannelKind], before fully closing.
    ///
    /// If the closed channel is the current default channel, the default channel gets set to `None`.
    ///
    /// Can fail if the [ChannelId] is unknown, or if the channel is already closed.
    pub fn close_channel(&mut self, channel_id: ChannelId) -> Result<(), ChannelCloseError> {
        match self.opened_channels.remove(&channel_id) {
            Some(_) => {
                if Some(channel_id) == self.default_channel {
                    self.default_channel = None;
                }
                for (_, connection) in self.clients.iter_mut() {
                    connection.close_channel(channel_id)?;
                }
                self.available_channel_ids.insert(channel_id);
                Ok(())
            }
            None => Err(ChannelCloseError::InvalidChannelId(channel_id)),
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

    fn close_incoming_connections_handler(&mut self) -> Result<(), AsyncChannelError> {
        match self.close_sender.send(()) {
            Ok(_) => Ok(()),
            // Connections handler is already closed
            Err(_) => Err(AsyncChannelError::InternalChannelClosed),
        }
    }

    fn handle_connection(
        &mut self,
        mut connection: ServerSideConnection,
    ) -> Result<ClientId, AsyncChannelError> {
        for (channel_id, channel_type) in self.opened_channels.iter() {
            if let Err(err) = connection.create_connection_channel(*channel_id, *channel_type) {
                connection.try_close();
                return Err(err);
            };
        }

        self.client_id_gen += 1;
        let client_id = self.client_id_gen;

        match connection
            .to_connection_send
            .try_send(ServerSyncMessage::ClientConnectedAck(client_id))
        {
            Ok(_) => {
                self.clients.insert(client_id, connection);
                Ok(client_id)
            }
            Err(_) => {
                connection.try_close();
                Err(AsyncChannelError::InternalChannelClosed)
            }
        }
    }
}

#[cfg(feature = "bincode-messages")]
impl Endpoint {
    /// Attempt to deserialise a message into type `T`.
    ///
    /// Will return [`Err`] if:
    /// - the bytes accumulated from the client aren't deserializable to T.
    /// - or if this client is disconnected.
    pub fn receive_message_from<T: serde::de::DeserializeOwned>(
        &mut self,
        client_id: ClientId,
    ) -> Result<Option<(ChannelId, T)>, ServerMessageReceiveError> {
        match self.receive_payload_from(client_id)? {
            Some((channel_id, payload)) => {
                match bincode::serde::decode_from_slice(&payload, bincode::config::standard()) {
                    Ok((msg, _size)) => Ok(Some((channel_id, msg))),
                    Err(_) => Err(ServerMessageReceiveError::Deserialization),
                }
            }
            None => Ok(None),
        }
    }

    /// [`Endpoint::receive_message_from`] that logs the error instead of returning a result.
    pub fn try_receive_message_from<T: serde::de::DeserializeOwned>(
        &mut self,
        client_id: ClientId,
    ) -> Option<(ChannelId, T)> {
        match self.receive_message_from(client_id) {
            Ok(message) => message,
            Err(err) => {
                error!("try_receive_message: {}", err);
                None
            }
        }
    }

    /// Same as [Endpoint::send_message_on] but on the default channel
    pub fn send_message<T: serde::Serialize>(
        &mut self,
        client_id: ClientId,
        message: T,
    ) -> Result<(), ServerMessageSendError> {
        match self.default_channel {
            Some(channel) => self.send_message_on(client_id, channel, message),
            None => Err(ServerMessageSendError::NoDefaultChannel),
        }
    }

    /// Sends a message to the specified client on the specified channel
    ///
    /// Will return an [`Err`] if:
    /// - the specified channel does not exist/is closed
    /// - or if the client is disconnected
    /// - or if a serialization error occurs
    /// - (or if the message queue is full)
    pub fn send_message_on<T: serde::Serialize, C: Into<ChannelId>>(
        &mut self,
        client_id: ClientId,
        channel_id: C,
        message: T,
    ) -> Result<(), ServerMessageSendError> {
        match bincode::serde::encode_to_vec(&message, bincode::config::standard()) {
            Ok(payload) => Ok(self.send_payload_on(client_id, channel_id, payload)?),
            Err(_) => Err(ServerMessageSendError::Serialization),
        }
    }

    /// [`Endpoint::send_message`] that logs the error instead of returning a result.
    pub fn try_send_message<T: serde::Serialize>(&mut self, client_id: ClientId, message: T) {
        match self.send_message(client_id, message) {
            Ok(_) => {}
            Err(err) => error!("try_send_message: {}", err),
        }
    }

    /// [`Endpoint::send_message_on`] that logs the error instead of returning a result.
    pub fn try_send_message_on<T: serde::Serialize, C: Into<ChannelId>>(
        &mut self,
        client_id: ClientId,
        channel_id: C,
        message: T,
    ) {
        match self.send_message_on(client_id, channel_id, message) {
            Ok(_) => {}
            Err(err) => error!("try_send_message: {}", err),
        }
    }

    /// Same as [Endpoint::send_group_message_on] but on the default channel
    pub fn send_group_message<'a, I: Iterator<Item = &'a ClientId>, T: serde::Serialize>(
        &mut self,
        client_ids: I,
        message: T,
    ) -> Result<(), ServerGroupMessageSendError> {
        match self.default_channel {
            Some(channel) => self.send_group_message_on(client_ids, channel, message),
            None => Err(ServerGroupMessageSendError::NoDefaultChannel),
        }
    }

    /// Sends the message to the specified clients on the specified channel.
    ///
    /// Tries to send to each client before returning. Returns an [`Err`] if sending failed for at least 1 client. Information about the failed sendings will be available in the [`ServerGroupMessageSendError`].
    pub fn send_group_message_on<
        'a,
        I: Iterator<Item = &'a ClientId>,
        T: serde::Serialize,
        C: Into<ChannelId>,
    >(
        &mut self,
        client_ids: I,
        channel_id: C,
        message: T,
    ) -> Result<(), ServerGroupMessageSendError> {
        let channel_id = channel_id.into();
        let Ok(payload) = bincode::serde::encode_to_vec(&message, bincode::config::standard())
        else {
            return Err(ServerGroupMessageSendError::Serialization);
        };
        let bytes = Bytes::from(payload);
        let mut errs = vec![];
        for &client_id in client_ids {
            if let Err(e) = self.send_payload_on(client_id, channel_id, bytes.clone()) {
                errs.push((client_id, e.into()));
            }
        }
        match errs.is_empty() {
            true => Ok(()),
            false => Err(ServerGroupSendError(errs).into()),
        }
    }

    /// Same as [Endpoint::send_group_message] but will log the error instead of returning it
    pub fn try_send_group_message<'a, I: Iterator<Item = &'a ClientId>, T: serde::Serialize>(
        &mut self,
        client_ids: I,
        message: T,
    ) {
        if let Err(err) = self.send_group_message(client_ids, message) {
            error!("try_send_group_message: {}", err);
        }
    }

    /// Same as [Endpoint::send_group_message_on] but will log the error instead of returning it
    pub fn try_send_group_message_on<
        'a,
        I: Iterator<Item = &'a ClientId>,
        T: serde::Serialize,
        C: Into<ChannelId>,
    >(
        &mut self,
        client_ids: I,
        channel_id: C,
        message: T,
    ) {
        if let Err(err) = self.send_group_message_on(client_ids, channel_id, message) {
            error!("try_send_group_message: {}", err);
        }
    }

    /// Same as [Endpoint::broadcast_message_on] but on the default channel
    pub fn broadcast_message<T: serde::Serialize>(
        &mut self,
        message: T,
    ) -> Result<(), ServerGroupMessageSendError> {
        match self.default_channel {
            Some(channel) => self.broadcast_message_on(channel, message),
            None => Err(ServerGroupMessageSendError::NoDefaultChannel),
        }
    }

    /// Same as [Endpoint::broadcast_payload_on] but will serialize the message to a payload before
    pub fn broadcast_message_on<T: serde::Serialize, C: Into<ChannelId>>(
        &mut self,
        channel_id: C,
        message: T,
    ) -> Result<(), ServerGroupMessageSendError> {
        match bincode::serde::encode_to_vec(&message, bincode::config::standard()) {
            Ok(payload) => Ok(self.broadcast_payload_on(channel_id, payload)?),
            Err(_) => Err(ServerGroupMessageSendError::Serialization),
        }
    }

    /// Same as [Endpoint::broadcast_message] but will log the error instead of returning it
    pub fn try_broadcast_message<T: serde::Serialize>(&mut self, message: T) {
        if let Err(err) = self.broadcast_message(message) {
            error!("try_broadcast_message: {}", err);
        }
    }

    /// Same as [Endpoint::broadcast_message_on] but will log the error instead of returning it
    pub fn try_broadcast_message_on<T: serde::Serialize, C: Into<ChannelId>>(
        &mut self,
        channel_id: C,
        message: T,
    ) {
        if let Err(err) = self.broadcast_message_on(channel_id, message) {
            error!("try_broadcast_message: {}", err);
        }
    }
}

/// Main quinnet server. Can listen to multiple [`ServerSideConnection`] from multiple quinnet clients
///
/// Created by the [`QuinnetServerPlugin`] or inserted manually via a call to [`bevy::prelude::World::insert_resource`]. When created, it will look for an existing [`AsyncRuntime`] resource and use it or create one itself.
#[derive(Resource)]
pub struct QuinnetServer {
    runtime: runtime::Handle,
    endpoint: Option<Endpoint>,
}

impl FromWorld for QuinnetServer {
    fn from_world(world: &mut World) -> Self {
        if world.get_resource::<AsyncRuntime>().is_none() {
            let async_runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            world.insert_resource(AsyncRuntime(async_runtime));
        };

        let runtime = world.resource::<AsyncRuntime>();
        QuinnetServer::new(runtime.handle().clone())
    }
}

impl QuinnetServer {
    fn new(runtime: tokio::runtime::Handle) -> Self {
        Self {
            endpoint: None,
            runtime,
        }
    }

    /// Returns a reference to the server's endpoint.
    ///
    /// **Panics** if the endpoint is not opened
    pub fn endpoint(&self) -> &Endpoint {
        self.endpoint.as_ref().unwrap()
    }

    /// Returns a mutable reference to the server's endpoint
    ///
    /// **Panics** if the endpoint is not opened
    pub fn endpoint_mut(&mut self) -> &mut Endpoint {
        self.endpoint.as_mut().unwrap()
    }

    /// Returns an optional reference to the server's endpoint
    pub fn get_endpoint(&self) -> Option<&Endpoint> {
        self.endpoint.as_ref()
    }

    /// Returns an optional mutable reference to the server's endpoint
    pub fn get_endpoint_mut(&mut self) -> Option<&mut Endpoint> {
        self.endpoint.as_mut()
    }

    /// Starts a new endpoint with the given [ServerEndpointConfiguration], [CertificateRetrievalMode] and [ChannelsConfiguration]
    ///
    /// Returns the [ServerCertificate] generated or loaded
    pub fn start_endpoint(
        &mut self,
        config: ServerEndpointConfiguration,
        cert_mode: CertificateRetrievalMode,
        channels_config: ChannelsConfiguration,
    ) -> Result<ServerCertificate, EndpointStartError> {
        // Endpoint configuration
        let server_cert = retrieve_certificate(cert_mode)?;
        let mut endpoint_config = ServerConfig::with_single_cert(
            server_cert.cert_chain.clone(),
            server_cert.priv_key.clone_key(),
        )?;
        Arc::get_mut(&mut endpoint_config.transport)
            .ok_or(EndpointStartError::LockAcquisitionFailure)?
            .keep_alive_interval(Some(DEFAULT_KEEP_ALIVE_INTERVAL_S));

        let (to_sync_endpoint_send, from_async_endpoint_recv) =
            mpsc::channel::<ServerAsyncMessage>(DEFAULT_INTERNAL_MESSAGES_CHANNEL_SIZE);
        let (endpoint_close_send, endpoint_close_recv) =
            broadcast::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);

        let socket = std::net::UdpSocket::bind(config.local_bind_addr)?;

        info!("Starting endpoint on: {} ...", config.local_bind_addr);
        self.runtime.spawn(async move {
            endpoint_task(
                socket,
                endpoint_config,
                to_sync_endpoint_send.clone(),
                endpoint_close_recv,
            )
            .await;
        });

        let mut endpoint = Endpoint::new(endpoint_close_send, from_async_endpoint_recv);
        for channel_type in channels_config.configs() {
            endpoint.unchecked_open_channel(*channel_type)?;
        }

        self.endpoint = Some(endpoint);

        Ok(server_cert)
    }

    /// Closes the endpoint and all the connections associated with it
    ///
    /// Returns [`EndpointAlreadyClosed`] if the endpoint is already closed
    pub fn stop_endpoint(&mut self) -> Result<(), EndpointAlreadyClosed> {
        match self.endpoint.take() {
            Some(mut endpoint) => {
                endpoint.disconnect_all_clients();
                match endpoint.close_incoming_connections_handler() {
                    Ok(_) => Ok(()),
                    Err(_) => Err(EndpointAlreadyClosed),
                }
            }
            None => Err(EndpointAlreadyClosed),
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
    socket: UdpSocket,
    endpoint_config: ServerConfig,
    to_sync_endpoint_send: mpsc::Sender<ServerAsyncMessage>,
    mut endpoint_close_recv: broadcast::Receiver<()>,
) {
    let endpoint = QuinnEndpoint::new(
        EndpointConfig::default(),
        Some(endpoint_config),
        socket,
        default_runtime().expect("async runtime should be valid"),
    )
    .expect("should create quinn endpoint");

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
                        let to_sync_endpoint_send = to_sync_endpoint_send.clone();
                        tokio::spawn(async move {
                            client_connection_task(
                                connection,
                                to_sync_endpoint_send
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
    connection_handle: quinn::Connection,
    to_sync_endpoint_send: mpsc::Sender<ServerAsyncMessage>,
) {
    let (client_close_send, client_close_recv) =
        broadcast::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);
    let (bytes_from_client_send, bytes_from_client_recv) =
        mpsc::channel::<(ChannelId, Bytes)>(DEFAULT_MESSAGE_QUEUE_SIZE);
    let (to_connection_send, mut from_sync_server_recv) =
        mpsc::channel::<ServerSyncMessage>(DEFAULT_INTERNAL_MESSAGES_CHANNEL_SIZE);
    let (from_channels_send, from_channels_recv) =
        mpsc::channel::<ChannelAsyncMessage>(DEFAULT_INTERNAL_MESSAGES_CHANNEL_SIZE);
    let (to_channels_send, to_channels_recv) =
        mpsc::channel::<ChannelSyncMessage>(DEFAULT_QCHANNEL_MESSAGES_CHANNEL_SIZE);

    // Signal the sync server of this new connection
    to_sync_endpoint_send
        .send(ServerAsyncMessage::ClientConnected(
            ServerSideConnection::new(
                connection_handle.clone(),
                bytes_from_client_recv,
                client_close_send.clone(),
                to_connection_send,
                from_channels_recv,
                to_channels_send,
            ),
        ))
        .await
        .expect("Failed to signal connection to sync client");

    // Wait for the sync server response before spawning connection tasks.
    match from_sync_server_recv.recv().await {
        Some(ServerSyncMessage::ClientConnectedAck(client_id)) => {
            info!(
                "New connection from {}, client_id: {}",
                connection_handle.remote_address(),
                client_id
            );

            #[cfg(feature = "shared-client-id")]
            spawn_client_id_sender(
                connection_handle.clone(),
                client_id,
                from_channels_send.clone(),
            );

            // Spawn a task to listen for the underlying connection being closed
            {
                let conn = connection_handle.clone();
                let to_sync_server = to_sync_endpoint_send.clone();
                tokio::spawn(async move {
                    let _conn_err = conn.closed().await;
                    info!("Connection {} closed: {}", client_id, _conn_err);
                    // If we requested the connection to close, channel may have been closed already.
                    if !to_sync_server.is_closed() {
                        to_sync_server
                            .send(ServerAsyncMessage::ClientConnectionClosed(client_id))
                            .await
                            .expect("Failed to signal connection lost in async connection");
                    }
                });
            };

            spawn_recv_channels_tasks(
                connection_handle.clone(),
                client_id,
                client_close_recv.resubscribe(),
                bytes_from_client_send,
            );

            spawn_send_channels_tasks_spawner(
                connection_handle,
                client_close_recv,
                to_channels_recv,
                from_channels_send,
            );
        }
        _ => info!(
            "Connection from {} refused",
            connection_handle.remote_address()
        ),
    }
}

/// Receive messages from the async server tasks and update the sync server.
///
/// This system generates the server's bevy events
pub fn update_sync_server(
    mut server: ResMut<QuinnetServer>,
    mut connection_events: EventWriter<ConnectionEvent>,
    mut connection_lost_events: EventWriter<ConnectionLostEvent>,
) {
    if let Some(endpoint) = server.get_endpoint_mut() {
        while let Ok(message) = endpoint.from_async_endpoint_recv.try_recv() {
            match message {
                ServerAsyncMessage::ClientConnected(connection) => {
                    match endpoint.handle_connection(connection) {
                        Ok(client_id) => {
                            endpoint.stats.connect_count += 1;
                            connection_events.write(ConnectionEvent { id: client_id });
                        }
                        Err(_) => {
                            error!("Failed to handle connection of a client, already disconnected");
                        }
                    };
                }
                ServerAsyncMessage::ClientConnectionClosed(client_id) => {
                    match endpoint.clients.contains_key(&client_id) {
                        true => {
                            endpoint.stats.disconnect_count += 1;
                            endpoint.try_disconnect_closed_client(client_id);
                            connection_lost_events.write(ConnectionLostEvent { id: client_id });
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
                            connection_lost_events.write(ConnectionLostEvent { id: *client_id });
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

/// Quinnet Server's plugin
///
/// It is possbile to add both this plugin and the [`crate::client::QuinnetClientPlugin`]
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
            app.init_resource::<QuinnetServer>();
        }

        app.add_systems(
            PreUpdate,
            update_sync_server
                .in_set(QuinnetSyncUpdate)
                .run_if(resource_exists::<QuinnetServer>),
        );
    }
}

/// Returns true if the following conditions are all true:
/// - the server Resource exists
/// - its endpoint is opened.
pub fn server_listening(server: Option<Res<QuinnetServer>>) -> bool {
    match server {
        Some(server) => server.is_listening(),
        None => false,
    }
}

/// Returns true if the following conditions are all true:
/// - the server Resource exists and its endpoint is opened
/// - the previous condition was false during the previous update
pub fn server_just_opened(
    mut was_listening: Local<bool>,
    server: Option<Res<QuinnetServer>>,
) -> bool {
    let listening = server.map(|server| server.is_listening()).unwrap_or(false);

    let just_opened = !*was_listening && listening;
    *was_listening = listening;
    just_opened
}

/// Returns true if the following conditions are all true:
/// - the server Resource does not exists or its endpoint is closed
/// - the previous condition was false during the previous update
pub fn server_just_closed(
    mut was_listening: Local<bool>,
    server: Option<Res<QuinnetServer>>,
) -> bool {
    let closed = server.map(|server| !server.is_listening()).unwrap_or(true);

    let just_closed = *was_listening && closed;
    *was_listening = !closed;
    just_closed
}
