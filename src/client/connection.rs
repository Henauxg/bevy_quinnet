use std::{
    collections::BTreeSet,
    error::Error,
    net::{AddrParseError, IpAddr, SocketAddr},
    sync::Arc,
};

use bevy::{
    log::{error, info, trace},
    prelude::Event,
};
use bytes::Bytes;
use quinn::{crypto::rustls::QuicClientConfig, ClientConfig, Endpoint};
use quinn_proto::ConnectionStats;

use rustls_platform_verifier::BuilderVerifierExt;
use serde::Deserialize;
use tokio::{
    runtime,
    sync::{
        broadcast,
        mpsc::{
            self,
            error::{TryRecvError, TrySendError},
        },
    },
};

#[cfg(feature = "shared-client-id")]
mod client_id;

#[cfg(feature = "shared-client-id")]
use client_id::receive_client_id;

use crate::shared::{
    channels::{
        spawn_recv_channels_tasks, spawn_send_channels_tasks_spawner, Channel, ChannelAsyncMessage,
        ChannelId, ChannelKind, ChannelSyncMessage, ChannelsConfiguration, CloseReason, CloseRecv,
        CloseSend,
    },
    error::{AsyncChannelError, ChannelCloseError, ChannelCreationError},
    ClientId, InternalConnectionRef, DEFAULT_INTERNAL_MESSAGES_CHANNEL_SIZE,
    DEFAULT_KILL_MESSAGE_QUEUE_SIZE, DEFAULT_MESSAGE_QUEUE_SIZE,
    DEFAULT_QCHANNEL_MESSAGES_CHANNEL_SIZE,
};

use super::{
    certificate::{
        load_known_hosts_store_from_config, CertificateVerificationMode, SkipServerVerification,
        TofuServerVerification,
    },
    error::{
        ClientMessageReceiveError, ClientMessageSendError, ClientPayloadSendError, ClientSendError,
    },
    ClientAsyncMessage, ClientConnectionCloseError, ConnectionClosed, QuinnetConnectionError,
};

/// Alias type for a local id of a connection
pub type ConnectionLocalId = u64;

/// Connection event raised when the client just connected to the server. Raised in the CoreStage::PreUpdate stage.
#[derive(Event, Debug, Copy, Clone)]
pub struct ConnectionEvent {
    /// Local id of the connection
    pub id: ConnectionLocalId,
    /// If present, id of the client on the server.
    ///
    /// Only available when the `shared-client-id` fetaure is enabled.
    pub client_id: Option<ClientId>,
}

/// Connection event raised when the client failed to connect to the server. Raised in the CoreStage::PreUpdate stage.
#[derive(Event, Debug, Clone)]
pub struct ConnectionFailedEvent {
    /// Local id of the connection which failed to connect
    pub id: ConnectionLocalId,
    /// Error raised during the connection
    pub err: QuinnetConnectionError,
}

/// ConnectionLost event raised when the client is considered disconnected from the server. Raised in the CoreStage::PreUpdate stage.
#[derive(Event, Debug, Copy, Clone)]
pub struct ConnectionLostEvent {
    /// Local id of the connection
    pub id: ConnectionLocalId,
}

/// Configuration of a client connection, used when connecting to a server
#[derive(Debug, Deserialize, Clone)]
pub struct ClientEndpointConfiguration {
    server_addr: SocketAddr,
    server_hostname: String,
    local_bind_addr: SocketAddr,
}

impl ClientEndpointConfiguration {
    /// Creates a new ClientEndpointConfiguration
    ///
    /// # Arguments
    ///
    /// * `server_addr_str` - IP address and port of the server
    /// * `local_bind_addr_str` - Local address and port to bind to separated by `:`. The address should usually be a wildcard like `0.0.0.0` (for an IPv4) or `[::]` (for an IPv6), which allow communication with any reachable IPv4 or IPv6 address. See [`std::net::SocketAddrV4`] and [`std::net::SocketAddrV6`] or [`quinn::Endpoint`] for more precision. For the local port to bind to, use 0 to get an OS-assigned port.
    ///
    /// # Examples
    ///
    /// Connect to an IPv4 server hosted on localhost (127.0.0.1), which is listening on port 6000. Use 0 as a local bind port to let the OS assign a port.
    /// ```
    /// use bevy_quinnet::client::connection::ClientEndpointConfiguration;
    /// let config = ClientEndpointConfiguration::from_strings(
    ///                 "127.0.0.1:6000",
    ///                 "0.0.0.0:0"
    ///             );
    /// ```
    /// Connect to an IPv6 server hosted on localhost (::1), which is listening on port 6000. Use 0 as a local bind port to let the OS assign a port.
    /// ```
    /// use bevy_quinnet::client::connection::ClientEndpointConfiguration;
    /// let config = ClientEndpointConfiguration::from_strings(
    ///                 "[::1]:6000",
    ///                 "[::]:0"
    ///             );
    /// ```
    pub fn from_strings(
        server_addr_str: &str,
        local_bind_addr_str: &str,
    ) -> Result<Self, AddrParseError> {
        let server_addr = server_addr_str.parse()?;
        let local_bind_addr = local_bind_addr_str.parse()?;
        Ok(Self::from_addrs(server_addr, local_bind_addr))
    }

    /// Same as [`ClientEndpointConfiguration::from_strings`], but with an additional `server_hostname` for certificate verification if it is not just the server IP.
    pub fn from_strings_with_name(
        server_addr_str: &str,
        server_hostname: String,
        local_bind_addr_str: &str,
    ) -> Result<Self, AddrParseError> {
        Ok(Self::from_addrs_with_name(
            server_addr_str.parse()?,
            server_hostname,
            local_bind_addr_str.parse()?,
        ))
    }

    /// Creates a new ClientEndpointConfiguration
    ///
    /// # Arguments
    ///
    /// * `server_ip` - IP address of the server
    /// * `server_port` - Port of the server
    /// * `local_bind_ip` - Local IP address to bind to. The address should usually be a wildcard like `0.0.0.0` (for an IPv4) or `0:0:0:0:0:0:0:0` (for an IPv6), which allow communication with any reachable IPv4 or IPv6 address. See [`std::net::Ipv4Addr`] and [`std::net::Ipv6Addr`] for more precision.
    /// * `local_bind_port` - Local port to bind to. Use 0 to get an OS-assigned port.
    ///
    /// # Examples
    ///
    /// Connect to an IPv4 server hosted on localhost (127.0.0.1), which is listening on port 6000. Use 0 as a local bind port to let the OS assign a port.
    /// ```
    /// use std::net::Ipv6Addr;
    /// use bevy_quinnet::client::connection::ClientEndpointConfiguration;
    /// let config = ClientEndpointConfiguration::from_ips(
    ///                 Ipv6Addr::LOCALHOST,
    ///                 6000,
    ///                 Ipv6Addr::UNSPECIFIED,
    ///                 0
    ///             );
    /// ```
    pub fn from_ips(
        server_ip: impl Into<IpAddr>,
        server_port: u16,
        local_bind_ip: impl Into<IpAddr>,
        local_bind_port: u16,
    ) -> Self {
        Self::from_addrs(
            SocketAddr::new(server_ip.into(), server_port),
            SocketAddr::new(local_bind_ip.into(), local_bind_port),
        )
    }

    /// Same as [`ClientEndpointConfiguration::from_ips`], but with an additional `server_hostname` for certificate verification if it is not just the server IP.
    pub fn from_ips_with_name(
        server_ip: impl Into<IpAddr>,
        server_port: u16,
        server_hostname: String,
        local_bind_ip: impl Into<IpAddr>,
        local_bind_port: u16,
    ) -> Self {
        Self::from_addrs_with_name(
            SocketAddr::new(server_ip.into(), server_port),
            server_hostname,
            SocketAddr::new(local_bind_ip.into(), local_bind_port),
        )
    }

    /// Creates a new ClientEndpointConfiguration
    ///
    /// # Arguments
    ///
    /// * `server_addr` - IP address and port of the server
    /// * `local_bind_addr` - Local address and port to bind to. For the local port to bind to, use 0 to get an OS-assigned port.
    ///
    /// # Examples
    ///
    /// Connect to an IPv4 server hosted on localhost (127.0.0.1), which is listening on port 6000. Use 0 as a local bind port to let the OS assign a port.
    /// ```
    /// use bevy_quinnet::client::connection::ClientEndpointConfiguration;
    /// use std::{net::{IpAddr, Ipv4Addr, SocketAddr}};
    /// let config = ClientEndpointConfiguration::from_addrs(
    ///        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 6000),
    ///        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0),
    ///    );
    /// ```
    pub fn from_addrs(server_addr: SocketAddr, local_bind_addr: SocketAddr) -> Self {
        Self {
            server_addr,
            server_hostname: server_addr.ip().to_string(),
            local_bind_addr,
        }
    }

    /// Same as [`ClientEndpointConfiguration::from_addrs`], but with an additional `server_hostname` for certificate verification if it is not just the server IP.
    pub fn from_addrs_with_name(
        server_addr: SocketAddr,
        server_hostname: String,
        local_bind_addr: SocketAddr,
    ) -> Self {
        Self {
            server_addr,
            server_hostname,
            local_bind_addr,
        }
    }
}

/// Current state of a client connection
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum ConnectionState {
    /// The connection is currently attempting to connect to the specified server
    Connecting,
    /// The connection is currently connected to the specified server
    Connected,
    /// The connection is currently disconnected from the specified server.
    ///
    /// It may have never been connected if the connection failed.
    Disconnected,
}
impl From<&InternalConnectionState> for ConnectionState {
    fn from(internal_conn: &InternalConnectionState) -> Self {
        match internal_conn {
            InternalConnectionState::Connecting => ConnectionState::Connecting,
            InternalConnectionState::Connected(_, _) => ConnectionState::Connected,
            InternalConnectionState::Disconnected => ConnectionState::Disconnected,
        }
    }
}

/// Current state of a client connection
#[derive(Debug)]
pub(crate) enum InternalConnectionState {
    Connecting,
    Connected(InternalConnectionRef, Option<ClientId>),
    Disconnected,
}

pub(crate) type MessageSend = mpsc::Sender<(ChannelId, Bytes)>;
pub(crate) type MessageRecv = mpsc::Receiver<(ChannelId, Bytes)>;
pub(crate) type ClientAsyncMsgSend = mpsc::Sender<ClientAsyncMessage>;
pub(crate) type ClientAsyncMsgRecv = mpsc::Receiver<ClientAsyncMessage>;
pub(crate) type ChannelAsyncMsgSend = mpsc::Sender<ChannelAsyncMessage>;
pub(crate) type ChannelAsyncMsgRecv = mpsc::Receiver<ChannelAsyncMessage>;
pub(crate) type ChannelSyncMsgSend = mpsc::Sender<ChannelSyncMessage>;
pub(crate) type ChannelSyncMsgRecv = mpsc::Receiver<ChannelSyncMessage>;

pub(crate) fn create_async_channels() -> (
    MessageSend,
    MessageRecv,
    ClientAsyncMsgSend,
    ClientAsyncMsgRecv,
    ChannelAsyncMsgSend,
    ChannelAsyncMsgRecv,
    ChannelSyncMsgSend,
    ChannelSyncMsgRecv,
    CloseSend,
    CloseRecv,
) {
    let (bytes_from_server_send, bytes_from_server_recv) =
        mpsc::channel::<(ChannelId, Bytes)>(DEFAULT_MESSAGE_QUEUE_SIZE);
    let (to_sync_client_send, to_sync_client_recv) =
        mpsc::channel::<ClientAsyncMessage>(DEFAULT_INTERNAL_MESSAGES_CHANNEL_SIZE);
    let (from_channels_send, from_channels_recv) =
        mpsc::channel::<ChannelAsyncMessage>(DEFAULT_INTERNAL_MESSAGES_CHANNEL_SIZE);
    let (to_channels_send, to_channels_recv) =
        mpsc::channel::<ChannelSyncMessage>(DEFAULT_QCHANNEL_MESSAGES_CHANNEL_SIZE);
    let (close_send, close_recv) = broadcast::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);
    (
        bytes_from_server_send,
        bytes_from_server_recv,
        to_sync_client_send,
        to_sync_client_recv,
        from_channels_send,
        from_channels_recv,
        to_channels_send,
        to_channels_recv,
        close_send,
        close_recv,
    )
}

/// A connection from a [`crate::client::QuinnetClient`] to a [`crate::server::QuinnetServer`]
#[derive(Debug)]
pub struct ClientSideConnection {
    /// Non networked identifier
    local_id: ConnectionLocalId,
    /// handle to the async runtime
    runtime: runtime::Handle,

    // Configuration
    endpoint_config: ClientEndpointConfiguration,
    cert_mode: CertificateVerificationMode,
    channels_config: ChannelsConfiguration,

    // State
    pub(crate) state: InternalConnectionState,

    channels: Vec<Option<Channel>>,
    available_channel_ids: BTreeSet<ChannelId>,
    default_channel: Option<ChannelId>,

    bytes_from_server_recv: mpsc::Receiver<(ChannelId, Bytes)>,
    close_sender: broadcast::Sender<CloseReason>,

    pub(crate) from_async_client_recv: mpsc::Receiver<ClientAsyncMessage>,
    pub(crate) to_channels_send: mpsc::Sender<ChannelSyncMessage>,
    pub(crate) from_channels_recv: mpsc::Receiver<ChannelAsyncMessage>,

    /// Quinnet stats
    received_messages_count: u64,
    received_bytes_count: usize,
    sent_bytes_count: usize,
}

impl ClientSideConnection {
    pub(crate) fn new(
        local_id: ConnectionLocalId,
        runtime: runtime::Handle,
        config: ClientEndpointConfiguration,
        cert_mode: CertificateVerificationMode,
        channels_config: ChannelsConfiguration,
        bytes_from_server_recv: MessageRecv,
        close_sender: CloseSend,
        from_async_client_recv: ClientAsyncMsgRecv,
        to_channels_send: ChannelSyncMsgSend,
        from_channels_recv: ChannelAsyncMsgRecv,
    ) -> Self {
        Self {
            local_id,
            runtime,
            state: InternalConnectionState::Connecting,
            channels: Vec::new(),
            default_channel: None,
            available_channel_ids: (0..255).collect(),
            bytes_from_server_recv,
            close_sender,
            from_async_client_recv,
            to_channels_send,
            from_channels_recv,
            endpoint_config: config,
            cert_mode,
            channels_config,
            received_messages_count: 0,
            received_bytes_count: 0,
            sent_bytes_count: 0,
        }
    }

    /// Attempt to deserialise a message into type `T`.
    ///
    /// Will return an [`Err`] if:
    /// - the bytes accumulated from the server aren't deserializable to T
    /// - or if the client is disconnected
    /// - (or if the message queue is full)
    pub fn receive_message<T: serde::de::DeserializeOwned>(
        &mut self,
    ) -> Result<Option<(ChannelId, T)>, ClientMessageReceiveError> {
        match self.receive_payload()? {
            Some((channel_id, payload)) => {
                match bincode::serde::decode_from_slice(&payload, bincode::config::standard()) {
                    Ok((msg, _size)) => Ok(Some((channel_id, msg))),
                    Err(_) => Err(ClientMessageReceiveError::Deserialization),
                }
            }
            None => Ok(None),
        }
    }

    /// Same as [Self::receive_message] but will log the error instead of returning it
    pub fn try_receive_message<T: serde::de::DeserializeOwned>(
        &mut self,
    ) -> Option<(ChannelId, T)> {
        match self.receive_message() {
            Ok(message) => message,
            Err(err) => {
                error!("try_receive_message: {}", err);
                None
            }
        }
    }

    /// Queues a message to be sent to the server on the specified channel
    ///
    /// Will return an [`Err`] if:
    /// - the specified channel does not exist/is closed
    /// - or if the client is disconnected
    /// - or if a serialization error occurs
    /// - (or if the message queue is full)
    pub fn send_message_on<T: serde::Serialize, C: Into<ChannelId>>(
        &mut self,
        channel_id: C,
        message: T,
    ) -> Result<(), ClientMessageSendError> {
        match bincode::serde::encode_to_vec(&message, bincode::config::standard()) {
            Ok(payload) => Ok(self.send_payload_on(channel_id, payload)?),
            Err(_) => Err(ClientMessageSendError::Serialization),
        }
    }

    /// Same as [Self::send_message_on] but on the default channel
    pub fn send_message<T: serde::Serialize>(
        &mut self,
        message: T,
    ) -> Result<(), ClientMessageSendError> {
        match self.default_channel {
            Some(channel) => self.send_message_on(channel, message),
            None => Err(ClientMessageSendError::NoDefaultChannel),
        }
    }

    /// Same as [Self::send_message] but will log the error instead of returning it
    pub fn try_send_message<T: serde::Serialize>(&mut self, message: T) {
        if let Err(err) = self.send_message(message) {
            error!("try_send_message: {}", err);
        }
    }

    /// Same as [Self::send_message_on] but will log the error instead of returning it
    pub fn try_send_message_on<T: serde::Serialize, C: Into<ChannelId>>(
        &mut self,
        channel_id: C,
        message: T,
    ) {
        if let Err(err) = self.send_message_on(channel_id, message) {
            error!("try_send_message_on: {}", err);
        }
    }

    /// Same as [Self::send_payload_on] but on the default channel
    pub fn send_payload<T: Into<Bytes>>(
        &mut self,
        payload: T,
    ) -> Result<(), ClientPayloadSendError> {
        match self.default_channel {
            Some(channel) => Ok(self.send_payload_on(channel, payload)?),
            None => Err(ClientPayloadSendError::NoDefaultChannel),
        }
    }

    /// Sends the payload to the server on the specified channel
    ///
    /// Will return an [`Err`] if:
    /// - the channel does not exist/is closed
    /// - or if the client is disconnected
    /// - (or if the message queue is full)
    pub fn send_payload_on<T: Into<Bytes>, C: Into<ChannelId>>(
        &mut self,
        channel_id: C,
        payload: T,
    ) -> Result<(), ClientSendError> {
        let channel_id = channel_id.into();
        match &self.state {
            InternalConnectionState::Disconnected => Err(ClientSendError::ConnectionClosed),
            _ => match self.channels.get(channel_id as usize) {
                Some(Some(channel)) => {
                    let bytes = payload.into();
                    self.sent_bytes_count += bytes.len();
                    Ok(channel.send_payload(bytes)?)
                }
                Some(None) => Err(ClientSendError::ChannelClosed),
                None => Err(ClientSendError::InvalidChannelId(channel_id)),
            },
        }
    }

    /// Same as [Self::send_payload] but will log the error instead of returning it
    pub fn try_send_payload<T: Into<Bytes>>(&mut self, payload: T) {
        if let Err(err) = self.send_payload(payload) {
            error!("try_send_payload: {}", err);
        }
    }

    /// Same as [Self::send_payload_on] but will log the error instead of returning it
    pub fn try_send_payload_on<T: Into<Bytes>, C: Into<ChannelId>>(
        &mut self,
        channel_id: C,
        payload: T,
    ) {
        if let Err(err) = self.send_payload_on(channel_id, payload) {
            error!("try_send_payload_on: {}", err);
        }
    }

    /// Attempts to receive a full payload sent by the server.
    ///
    /// - Returns an [`Ok`] result containg [`Some`] if there is a message from the server in the message buffer
    /// - Returns an [`Ok`] result containg [`None`] if there is no message from the server in the message buffer
    /// - Can return an [`Err`] if the connection is closed
    pub fn receive_payload(&mut self) -> Result<Option<(ChannelId, Bytes)>, ConnectionClosed> {
        match &self.state {
            InternalConnectionState::Disconnected => Err(ConnectionClosed),
            _ => match self.bytes_from_server_recv.try_recv() {
                Ok(msg_payload) => {
                    self.received_bytes_count += msg_payload.1.len();
                    self.received_messages_count += 1;
                    Ok(Some(msg_payload))
                }
                Err(err) => match err {
                    TryRecvError::Empty => Ok(None),
                    TryRecvError::Disconnected => Err(ConnectionClosed),
                },
            },
        }
    }

    /// Same as [Self::receive_payload] but will log the error instead of returning it
    pub fn try_receive_payload(&mut self) -> Option<(ChannelId, Bytes)> {
        match self.receive_payload() {
            Ok(payload) => payload,
            Err(err) => {
                error!("try_receive_payload: {}", err);
                None
            }
        }
    }

    fn internal_disconnect(
        &mut self,
        reason: CloseReason,
    ) -> Result<(), ClientConnectionCloseError> {
        match &self.state {
            &InternalConnectionState::Disconnected => Ok(()),
            _ => {
                self.state = InternalConnectionState::Disconnected;
                match self.close_sender.send(reason) {
                    Ok(_) => Ok(()),
                    Err(_) => {
                        // The only possible error for a send is that there is no active receivers, meaning that the tasks are already terminated.
                        Err(ClientConnectionCloseError::ConnectionAlreadyClosed)
                    }
                }
            }
        }
    }

    /// Immediately prevents new messages from being sent on the connection and signal the connection to closes all its background tasks.
    ///
    /// Before trully closing, the connection will wait for all buffered messages in all its opened channels to be properly sent according to their respective channel type.
    pub fn disconnect(&mut self) -> Result<(), ClientConnectionCloseError> {
        self.internal_disconnect(CloseReason::LocalOrder)
    }

    /// Same as [Self::disconnect] but will log the error instead of returning it
    pub fn try_disconnect(&mut self) {
        if let Err(err) = &self.disconnect() {
            error!("Failed to properly close clonnection: {}", err);
        }
    }

    /// Logical "Disconnect", the underlying connection si already closed/lost.
    pub(crate) fn try_disconnect_closed_connection(&mut self) {
        if let Err(err) = self.internal_disconnect(CloseReason::PeerClosed) {
            error!("Failed to properly close clonnection: {}", err);
        }
    }

    /// Returns the current [ConnectionState] of the connection
    pub fn state(&self) -> ConnectionState {
        (&self.state).into()
    }

    /// See [quinn::Connection::max_datagram_size]
    pub fn max_datagram_size(&self) -> Option<usize> {
        match &self.state {
            InternalConnectionState::Connected(connection, _) => connection.max_datagram_size(),
            _ => None,
        }
    }

    /// Returns statistics about the current connection if connected.
    pub fn connection_stats(&self) -> Option<ConnectionStats> {
        match &self.state {
            InternalConnectionState::Connected(connection, _) => Some(connection.stats()),
            _ => None,
        }
    }

    /// Returns how many messages were read from this connection currently
    pub fn received_messages_count(&self) -> u64 {
        self.received_messages_count
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

    /// Returns the client_id assigned to this client by the server.
    ///
    /// Will be [None] if the `shared-client-id` feature is disabled
    pub fn client_id(&self) -> Option<ClientId> {
        match &self.state {
            InternalConnectionState::Connected(_, client_id) => *client_id,
            _ => None,
        }
    }

    /// Attempts to reconnect to the server.
    ///
    /// This uses the initial connection configuration. Notably, channels opened by calling [`Self::open_channel`] on the connection after it was initially opened won't be automatically re-opened.
    ///
    /// Does nothing if the connection state is not [`ConnectionState::Disconnected`]
    pub fn reconnect(&mut self) -> Result<(), AsyncChannelError> {
        match &self.state {
            InternalConnectionState::Disconnected => {
                let (
                    bytes_from_server_send,
                    bytes_from_server_recv,
                    to_sync_client_send,
                    to_sync_client_recv,
                    from_channels_send,
                    from_channels_recv,
                    to_channels_send,
                    to_channels_recv,
                    close_send,
                    close_recv,
                ) = create_async_channels();

                // Connection state reset
                self.state = InternalConnectionState::Connecting;
                self.channels = Vec::with_capacity(self.channels_config.configs().len());
                self.default_channel = None;
                self.available_channel_ids = (0..255).collect();
                self.bytes_from_server_recv = bytes_from_server_recv;
                self.close_sender = close_send;
                self.from_async_client_recv = to_sync_client_recv;
                self.to_channels_send = to_channels_send;
                self.from_channels_recv = from_channels_recv;
                // Connection stats reset
                self.received_messages_count = 0;
                self.received_bytes_count = 0;
                self.sent_bytes_count = 0;

                // Open default channels
                self.open_configured_channels(self.channels_config.clone())?;

                // Async connection
                let local_id = self.local_id;
                let endpoint_config = self.endpoint_config.clone();
                let cert_mode = self.cert_mode.clone();
                self.runtime.spawn(async move {
                    async_connection_task(
                        local_id,
                        endpoint_config,
                        cert_mode,
                        to_sync_client_send,
                        bytes_from_server_send,
                        to_channels_recv,
                        from_channels_send,
                        close_recv,
                    )
                    .await
                });
            }
            _ => (),
        }
        Ok(())
    }

    pub(crate) fn open_configured_channels(
        &mut self,
        channels_config: ChannelsConfiguration,
    ) -> Result<(), AsyncChannelError> {
        for channel_type in channels_config.configs() {
            self.unchecked_open_channel(*channel_type)?;
        }
        Ok(())
    }

    /// Returns the configuration used by this connection
    pub fn endpoint_configuration(&self) -> &ClientEndpointConfiguration {
        &self.endpoint_config
    }

    /// Returns the certificate verification configuration used by this connection
    pub fn certificate_verification_mode(&self) -> &CertificateVerificationMode {
        &self.cert_mode
    }

    /// Opens a channel of the requested [ChannelKind] and returns its [ChannelId].
    ///
    /// If no channels were previously opened, the opened channel will be the new default channel.
    ///
    /// Can fail if the Connection is closed.
    pub fn open_channel(
        &mut self,
        channel_type: ChannelKind,
    ) -> Result<ChannelId, ChannelCreationError> {
        let channel_id = match self.available_channel_ids.pop_first() {
            Some(channel_id) => channel_id,
            None => return Err(ChannelCreationError::MaxChannelsCountReached),
        };
        Ok(self.internal_open_channel(channel_id, channel_type)?)
    }

    fn unchecked_open_channel(
        &mut self,
        channel_type: ChannelKind,
    ) -> Result<ChannelId, AsyncChannelError> {
        let channel_id = self.available_channel_ids.pop_first().unwrap();
        Ok(self.internal_open_channel(channel_id, channel_type)?)
    }

    fn internal_open_channel(
        &mut self,
        channel_id: ChannelId,
        channel_type: ChannelKind,
    ) -> Result<ChannelId, AsyncChannelError> {
        match self.create_channel(channel_id, channel_type) {
            Ok(channel_id) => {
                if self.default_channel.is_none() {
                    self.default_channel = Some(channel_id);
                }
                Ok(channel_id)
            }
            Err(err) => {
                // Reinsert the popped channel id
                self.available_channel_ids.insert(channel_id);
                Err(err)
            }
        }
    }

    /// Closes the channel with the corresponding [ChannelId].
    ///
    /// No new messages will be able to be sent on this channel, however, the channel will properly try to send all the messages that were previously pushed to it, according to its [ChannelKind], before fully closing.
    ///
    /// If the closed channel is the current default channel, the default channel gets set to `None`.
    ///
    /// Can fail if the [ChannelId] is unknown, or if the channel is already closed.
    pub fn close_channel(&mut self, channel_id: ChannelId) -> Result<(), ChannelCloseError> {
        if (channel_id as usize) < self.channels.len() {
            match self.channels[channel_id as usize].take() {
                Some(channel) => {
                    if Some(channel_id) == self.default_channel {
                        self.default_channel = None;
                    }
                    self.available_channel_ids.insert(channel_id);
                    channel.close()
                }
                None => Err(ChannelCloseError::ChannelAlreadyClosed),
            }
        } else {
            Err(ChannelCloseError::InvalidChannelId(channel_id))
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

    fn create_channel(
        &mut self,
        channel_id: ChannelId,
        channel_type: ChannelKind,
    ) -> Result<ChannelId, AsyncChannelError> {
        let (bytes_to_channel_send, bytes_to_channel_recv) =
            mpsc::channel::<Bytes>(DEFAULT_MESSAGE_QUEUE_SIZE);
        let (channel_close_send, channel_close_recv) =
            mpsc::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);

        match self
            .to_channels_send
            .try_send(ChannelSyncMessage::CreateChannel {
                id: channel_id,
                kind: channel_type,
                bytes_to_channel_recv,
                channel_close_recv,
            }) {
            Ok(_) => {
                let channel = Some(Channel::new(
                    channel_id,
                    bytes_to_channel_send,
                    channel_close_send,
                ));
                if (channel_id as usize) < self.channels.len() {
                    self.channels[channel_id as usize] = channel;
                } else {
                    for _ in self.channels.len()..channel_id as usize {
                        self.channels.push(None);
                    }
                    self.channels.push(channel);
                }

                Ok(channel_id)
            }
            Err(err) => match err {
                TrySendError::Full(_) => Err(AsyncChannelError::FullQueue),
                TrySendError::Closed(_) => Err(AsyncChannelError::InternalChannelClosed),
            },
        }
    }
}

pub(crate) async fn async_connection_task(
    local_id: ConnectionLocalId,
    endpoint_config: ClientEndpointConfiguration,
    cert_mode: CertificateVerificationMode,
    to_sync_client_send: ClientAsyncMsgSend,
    bytes_from_server_send: MessageSend,
    to_channels_recv: ChannelSyncMsgRecv,
    from_channels_send: ChannelAsyncMsgSend,
    close_recv: CloseRecv,
) {
    info!(
        "Connection {} trying to connect to server on: {} ...",
        local_id, endpoint_config.server_addr
    );

    let client_cfg = configure_client(cert_mode, to_sync_client_send.clone())
        .expect("Failed to configure client");

    let mut endpoint = Endpoint::client(endpoint_config.local_bind_addr)
        .expect("Failed to create client endpoint");
    endpoint.set_default_client_config(client_cfg);

    let connection = endpoint
        .connect(
            endpoint_config.server_addr,
            &endpoint_config.server_hostname,
        )
        .expect("Failed to connect: configuration error")
        .await;

    match connection {
        Err(e) => {
            error!("Connection {}, error while connecting: {}", local_id, e);
            // Signal connection failure
            to_sync_client_send
                .send(ClientAsyncMessage::ConnectionFailed(
                    QuinnetConnectionError::from(e),
                ))
                .await
                .expect("Failed to signal connection failure to sync client");
        }
        Ok(connection_handle) => {
            // Spawn a task to listen for the underlying connection being closed
            {
                let conn = connection_handle.clone();
                let to_sync_client = to_sync_client_send.clone();
                tokio::spawn(async move {
                    let _conn_err = conn.closed().await;
                    info!("Connection {} closed: {}", local_id, _conn_err);
                    // If we requested the connection to close, channel may have been closed already.
                    if !to_sync_client.is_closed() {
                        to_sync_client
                            .send(ClientAsyncMessage::ConnectionClosed)
                            .await
                            .expect("Failed to signal connection closed in async connection");
                    }
                })
            };

            spawn_recv_channels_tasks(
                connection_handle.clone(),
                local_id,
                close_recv.resubscribe(),
                bytes_from_server_send,
            );

            spawn_send_channels_tasks_spawner(
                connection_handle.clone(),
                close_recv.resubscribe(),
                to_channels_recv,
                from_channels_send,
            );

            #[cfg(not(feature = "shared-client-id"))]
            signal_connection(
                connection_handle.clone(),
                local_id,
                None,
                to_sync_client_send,
            )
            .await;

            #[cfg(feature = "shared-client-id")]
            match receive_client_id(connection_handle.clone(), close_recv).await {
                client_id::ClientIdReception::Retrieved(client_id) => {
                    signal_connection(
                        connection_handle.clone(),
                        local_id,
                        Some(client_id),
                        to_sync_client_send,
                    )
                    .await
                }
                client_id::ClientIdReception::Failed(e) => {
                    error!(
                        "Connection {}, error while retrieving client_id: {}",
                        local_id, e
                    );
                    // Signal connection failure
                    to_sync_client_send
                        .send(ClientAsyncMessage::ConnectionFailed(e))
                        .await
                        .expect("Failed to signal connection failure to sync client");
                }
                client_id::ClientIdReception::Interrupted => trace!(
                    "Connection {}, reception of client_id was interrupted",
                    local_id
                ),
            }
        }
    }
}

async fn signal_connection(
    connection_handle: quinn::Connection,
    connection_id: ConnectionLocalId,
    client_id: Option<ClientId>,
    to_sync_client_send: mpsc::Sender<ClientAsyncMessage>,
) {
    // Signal connection
    to_sync_client_send
        .send(ClientAsyncMessage::Connected(
            connection_handle.clone(),
            client_id,
        ))
        .await
        .expect("Failed to signal connection to sync client");

    info!(
        "Connection {} connected to {} with client_id {:?}",
        connection_id,
        connection_handle.remote_address(),
        client_id
    );
}

fn configure_client(
    cert_mode: CertificateVerificationMode,
    to_sync_client: mpsc::Sender<ClientAsyncMessage>,
) -> Result<ClientConfig, Box<dyn Error>> {
    let mut crypto = match cert_mode {
        CertificateVerificationMode::SkipVerification => rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(SkipServerVerification::new())
            .with_no_client_auth(),
        CertificateVerificationMode::SignedByCertificateAuthority => {
            // Using Quinn's helper `ClientConfig::with_platform_verifier` does not let us specify the CryptoProvider used,
            // and relies on the per-process default one (https://docs.rs/rustls/latest/rustls/crypto/struct.CryptoProvider.html#using-the-per-process-default-cryptoprovider) which may not be set.
            // As a library, we do not want to set it ourselves using `CryptoProvider::install_default`
            rustls::ClientConfig::builder_with_provider(
                rustls::crypto::ring::default_provider().into(),
            )
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            // We use `rustls-platform-verifier::with_platform_verifier` directly instead (used internally by Quinn).
            .with_platform_verifier()
            .with_no_client_auth()
        }
        CertificateVerificationMode::TrustOnFirstUse(config) => {
            let (store, store_file) = load_known_hosts_store_from_config(config.known_hosts)?;
            rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(TofuServerVerification::new(
                    store,
                    config.verifier_behaviour,
                    to_sync_client,
                    store_file,
                    Arc::new(rustls::crypto::ring::default_provider()),
                ))
                .with_no_client_auth()
        }
    };

    // Quinn defaults to true
    crypto.enable_early_data = true;

    Ok(ClientConfig::new(Arc::new(QuicClientConfig::try_from(
        crypto,
    )?)))
}
