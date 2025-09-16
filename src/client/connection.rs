use std::{
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
use tokio::{
    runtime,
    sync::{
        broadcast,
        mpsc::{self, error::TryRecvError},
    },
};

#[cfg(feature = "shared-client-id")]
mod client_id;

#[cfg(feature = "bincode-messages")]
mod messages;

use crate::shared::{
    channels::{
        tasks::{spawn_recv_channels_tasks, spawn_send_channels_tasks_spawner},
        ChannelAsyncMessage, ChannelConfig, ChannelId, ChannelSyncMessage, ChannelsConfiguration,
        CloseReason, CloseRecv, CloseSend,
    },
    connection::{
        ChannelAsyncMsgRecv, ChannelAsyncMsgSend, ChannelSyncMsgRecv, ChannelSyncMsgSend,
        ChannelsIdsPool, PayloadRecv, PayloadSend, PeerConnection,
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
    error::{ClientPayloadSendError, ClientSendError},
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
#[derive(Debug, Clone)]
pub struct ClientConfiguration {
    /// Address and port of the server to connect to.
    pub server_addr: SocketAddr,
    /// Server hostname for certificate verification, can be just the server IP.
    pub server_hostname: String,
    /// Local address and port to bind to.
    pub local_bind_addr: SocketAddr,
}

impl ClientConfiguration {
    /// Creates a new ClientConfiguration
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
    /// use bevy_quinnet::client::connection::ClientConfiguration;
    /// let config = ClientConfiguration::from_strings(
    ///                 "127.0.0.1:6000",
    ///                 "0.0.0.0:0"
    ///             );
    /// ```
    /// Connect to an IPv6 server hosted on localhost (::1), which is listening on port 6000. Use 0 as a local bind port to let the OS assign a port.
    /// ```
    /// use bevy_quinnet::client::connection::ClientConfiguration;
    /// let config = ClientConfiguration::from_strings(
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

    /// Same as [`ClientConfiguration::from_strings`], but with an additional `server_hostname` for certificate verification if it is not just the server IP.
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

    /// Creates a new ClientConfiguration
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
    /// use bevy_quinnet::client::connection::ClientConfiguration;
    /// let config = ClientConfiguration::from_ips(
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

    /// Same as [`ClientConfiguration::from_ips`], but with an additional `server_hostname` for certificate verification if it is not just the server IP.
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

    /// Creates a new ClientConfiguration
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
    /// use bevy_quinnet::client::connection::ClientConfiguration;
    /// use std::{net::{IpAddr, Ipv4Addr, SocketAddr}};
    /// let config = ClientConfiguration::from_addrs(
    ///        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 6000),
    ///        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0),
    ///    );
    /// ```
    pub fn from_addrs(server_addr: SocketAddr, local_bind_addr: SocketAddr) -> Self {
        Self::from_addrs_with_name(server_addr, server_addr.ip().to_string(), local_bind_addr)
    }

    /// Same as [`ClientConfiguration::from_addrs`], but with an additional `server_hostname` for certificate verification if it is not just the server IP.
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

pub(crate) type ClientAsyncMsgSend = mpsc::Sender<ClientAsyncMessage>;
pub(crate) type ClientAsyncMsgRecv = mpsc::Receiver<ClientAsyncMessage>;

pub(crate) fn create_client_connection_async_channels() -> (
    PayloadSend,
    PayloadRecv,
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

/// A connection to a server from the client's perspective.
pub type ClientSideConnection = PeerConnection<ClientConnection>;

/// Specific data for a client-side connection
pub struct ClientConnection {
    /// Non-networked identifier
    local_id: ConnectionLocalId,
    /// Handle to the async runtime
    runtime: runtime::Handle,
    // Configuration
    client_config: ClientConfiguration,
    cert_mode: CertificateVerificationMode,
    channels_config: ChannelsConfiguration,

    state: InternalConnectionState,
    send_channel_ids: ChannelsIdsPool,
    from_async_client_recv: mpsc::Receiver<ClientAsyncMessage>,
}
impl ClientConnection {
    pub(crate) fn new(
        local_id: ConnectionLocalId,
        runtime: runtime::Handle,
        client_config: ClientConfiguration,
        cert_mode: CertificateVerificationMode,
        channels_config: ChannelsConfiguration,
        from_async_client_recv: mpsc::Receiver<ClientAsyncMessage>,
    ) -> Self {
        Self {
            local_id,
            runtime,
            client_config,
            cert_mode,
            channels_config,
            state: InternalConnectionState::Connecting,
            send_channel_ids: ChannelsIdsPool::new(),
            from_async_client_recv,
        }
    }
}
impl ClientSideConnection {
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
        match &self.specific.state {
            InternalConnectionState::Disconnected => Err(ClientSendError::ConnectionClosed),
            _ => Ok(self.internal_send_payload(channel_id, payload.into())?),
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

    /// Same as [Self::send_payload_on] but on the default channel
    pub fn send_payload<T: Into<Bytes>>(
        &mut self,
        payload: T,
    ) -> Result<(), ClientPayloadSendError> {
        match self.specific.send_channel_ids.default_channel() {
            Some(channel) => Ok(self.send_payload_on(channel, payload.into())?),
            None => Err(ClientPayloadSendError::NoDefaultChannel),
        }
    }

    /// Attempts to receive a full payload sent by the server.
    ///
    /// - Returns an [`Ok`] result containg [`Some`] if there is a message from the server in the message buffer
    /// - Returns an [`Ok`] result containg [`None`] if there is no message from the server in the message buffer
    /// - Can return an [`Err`] if the connection is closed
    pub fn receive_payload<C: Into<ChannelId>>(
        &mut self,
        channel_id: C,
    ) -> Result<Option<Bytes>, ConnectionClosed> {
        match &self.internal_state() {
            InternalConnectionState::Disconnected => Err(ConnectionClosed),
            _ => Ok(self.internal_receive_payload(channel_id.into())),
        }
    }

    /// Same as [Self::receive_payload] but will log the error instead of returning it
    pub fn try_receive_payload<C: Into<ChannelId>>(&mut self, channel_id: C) -> Option<Bytes> {
        match self.receive_payload(channel_id) {
            Ok(payload) => payload,
            Err(err) => {
                error!("try_receive_payload: {}", err);
                None
            }
        }
    }

    /// Opens a channel of the requested [ChannelConfig] and returns its [ChannelId].
    ///
    /// If no channels were previously opened, the opened channel will be the new default channel.
    ///
    /// Can fail if the Connection is closed.
    pub fn open_channel(
        &mut self,
        channel_config: ChannelConfig,
    ) -> Result<ChannelId, ChannelCreationError> {
        let channel_id = self.specific.send_channel_ids.take_id()?;
        self.create_connection_channel(channel_id, channel_config)?;
        Ok(channel_id)
    }

    /// Closes the channel with the corresponding [ChannelId].
    ///
    /// No new messages will be able to be sent on this channel, however, the channel will properly try to send all the messages that were previously pushed to it, according to its [ChannelConfig], before fully closing.
    ///
    /// If the closed channel is the current default channel, the default channel gets set to `None`.
    ///
    /// Can fail if the [ChannelId] is unknown, or if the channel is already closed.
    pub fn close_channel(&mut self, channel_id: ChannelId) -> Result<(), ChannelCloseError> {
        self.internal_close_channel(channel_id)?;
        self.specific.send_channel_ids.release_id(channel_id);
        Ok(())
    }

    pub(crate) fn open_configured_channels(
        &mut self,
        channel_configs: ChannelsConfiguration,
    ) -> Result<(), AsyncChannelError> {
        for channel_config in channel_configs.configs() {
            // Unchecked because we know we have enough ids
            let channel_id = self.specific.send_channel_ids.take_id().unwrap();
            match self.create_unregistered_connection_channel(channel_id, channel_config.clone()) {
                Ok(channel) => self.register_connection_channel(channel),
                Err(e) => {
                    self.specific.send_channel_ids.release_id(channel_id);
                    return Err(e);
                }
            };
        }
        Ok(())
    }

    fn internal_disconnect(
        &mut self,
        reason: CloseReason,
    ) -> Result<(), ClientConnectionCloseError> {
        match &self.specific.state {
            &InternalConnectionState::Disconnected => Ok(()),
            _ => {
                self.specific.state = InternalConnectionState::Disconnected;
                Ok(self.close(reason)?)
            }
        }
    }

    /// Attempts to reconnect to the server.
    ///
    /// This uses the initial connection configuration. Notably, channels opened by calling [`Self::open_channel`] on the connection after it was initially opened won't be automatically re-opened.
    ///
    /// Does nothing if the connection state is not [`ConnectionState::Disconnected`]
    pub fn reconnect(&mut self) -> Result<(), AsyncChannelError> {
        match &self.internal_state() {
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
                ) = create_client_connection_async_channels();

                // Connection state reset
                self.set_state(InternalConnectionState::Connecting);
                self.specific.send_channel_ids = ChannelsIdsPool::new();
                self.specific.from_async_client_recv = to_sync_client_recv;
                self.reset(
                    close_send,
                    to_channels_send,
                    from_channels_recv,
                    bytes_from_server_recv,
                    self.specific.channels_config.configs().len(),
                );

                // Open default channels
                self.open_configured_channels(self.specific.channels_config.clone())?;

                // Async connection
                let local_id = self.specific.local_id;
                let endpoint_config = self.specific.client_config.clone();
                let cert_mode = self.specific.cert_mode.clone();
                self.specific.runtime.spawn(async move {
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

    /// Immediately prevents new messages from being sent on the connection and signal the connection to closes all its background tasks.
    ///
    /// Before trully closing, the connection will wait for all buffered messages in all its opened channels to be properly sent according to their respective channel type.
    #[inline(always)]
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

    #[inline(always)]
    pub(crate) fn try_recv_from_async(&mut self) -> Result<ClientAsyncMessage, TryRecvError> {
        self.specific.from_async_client_recv.try_recv()
    }

    #[inline(always)]
    pub(crate) fn set_state(&mut self, state: InternalConnectionState) {
        self.specific.state = state;
    }

    #[inline(always)]
    pub(crate) fn internal_state(&self) -> &InternalConnectionState {
        &self.specific.state
    }

    /// Returns the current [ConnectionState] of the connection
    #[inline(always)]
    pub fn state(&self) -> ConnectionState {
        (&self.specific.state).into()
    }

    /// Returns the client_id assigned to this client by the server.
    ///
    /// Will be [None] if the `shared-client-id` feature is disabled
    pub fn client_id(&self) -> Option<ClientId> {
        match &self.internal_state() {
            InternalConnectionState::Connected(_, client_id) => *client_id,
            _ => None,
        }
    }

    /// Returns statistics about the current connection if connected.
    pub fn connection_stats(&self) -> Option<ConnectionStats> {
        match &self.internal_state() {
            InternalConnectionState::Connected(connection, _) => Some(connection.stats()),
            _ => None,
        }
    }

    /// See [quinn::Connection::max_datagram_size]
    pub fn max_datagram_size(&self) -> Option<usize> {
        match &self.internal_state() {
            InternalConnectionState::Connected(connection, _) => connection.max_datagram_size(),
            _ => None,
        }
    }

    /// Set the default channel
    #[inline(always)]
    pub fn set_default_channel(&mut self, channel_id: ChannelId) {
        self.specific
            .send_channel_ids
            .set_default_channel(channel_id);
    }

    /// Get the default Channel Id
    #[inline(always)]
    pub fn default_channel(&self) -> Option<ChannelId> {
        self.specific.send_channel_ids.default_channel()
    }

    /// Returns the configuration used by this connection
    #[inline(always)]
    pub fn endpoint_configuration(&self) -> &ClientConfiguration {
        &self.specific.client_config
    }

    /// Returns the certificate verification configuration used by this connection
    #[inline(always)]
    pub fn certificate_verification_mode(&self) -> &CertificateVerificationMode {
        &self.specific.cert_mode
    }
}

pub(crate) async fn async_connection_task(
    local_id: ConnectionLocalId,
    endpoint_config: ClientConfiguration,
    cert_mode: CertificateVerificationMode,
    to_sync_client_send: ClientAsyncMsgSend,
    bytes_from_server_send: PayloadSend,
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
            signal_connected(
                connection_handle.clone(),
                local_id,
                None,
                to_sync_client_send,
            )
            .await;

            #[cfg(feature = "shared-client-id")]
            match client_id::receive_client_id(connection_handle.clone(), close_recv).await {
                client_id::ClientIdReception::Retrieved(client_id) => {
                    signal_connected(
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

async fn signal_connected(
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
