use std::{
    collections::HashSet,
    net::{AddrParseError, IpAddr, SocketAddr, UdpSocket},
    sync::Arc,
};

use bevy::prelude::*;
use bytes::Bytes;
use quinn::{default_runtime, Endpoint as QuinnEndpoint, EndpointConfig, ServerConfig};
use tokio::{
    runtime,
    sync::{
        broadcast::{self},
        mpsc::{self},
    },
};

use crate::{
    server::{
        certificate::{retrieve_certificate, CertificateRetrievalMode, ServerCertificate},
        connection::ServerConnection,
        endpoint::Endpoint,
    },
    shared::{
        channels::{
            tasks::{spawn_recv_channels_tasks, spawn_send_channels_tasks_spawner},
            ChannelAsyncMessage, ChannelId, ChannelSyncMessage, ChannelsConfiguration,
        },
        connection::{ConnectionConfig, PeerConnection, DEFAULT_CLEAR_STALE_RECEIVED_PAYLOADS},
        AsyncRuntime, ClientId, QuinnetSyncPostUpdate, QuinnetSyncPreUpdate,
        DEFAULT_INTERNAL_MESSAGES_CHANNEL_SIZE, DEFAULT_KEEP_ALIVE_INTERVAL_S,
        DEFAULT_KILL_MESSAGE_QUEUE_SIZE, DEFAULT_MESSAGE_QUEUE_SIZE,
        DEFAULT_QCHANNEL_MESSAGES_CHANNEL_SIZE,
    },
};

#[cfg(feature = "shared-client-id")]
mod client_id;

#[cfg(feature = "bincode-messages")]
mod messages;

/// Module for the server's endpoint connections
pub mod connection;
/// Module for the server's endpoint features
pub mod endpoint;
/// Module for the server's error types
pub mod error;

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
    /// Local address and port to bind to.
    pub local_bind_addr: SocketAddr,
    /// See [Endpoint::clear_stale_received_payloads]. Defaults to [DEFAULT_CLEAR_STALE_RECEIVED_PAYLOADS].
    pub clear_stale_received_payloads: bool,
    /// Configuration applied to each new connection accepted by this endpoint.
    pub connections_config: ConnectionConfig,
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
        Ok(Self::from_addr(local_bind_addr))
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
        Self::from_addr(SocketAddr::new(local_bind_ip.into(), local_bind_port))
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
        Self {
            local_bind_addr,
            clear_stale_received_payloads: DEFAULT_CLEAR_STALE_RECEIVED_PAYLOADS,
            connections_config: ConnectionConfig::default(),
        }
    }
}

pub(crate) enum ServerAsyncMessage {
    ClientConnected(PeerConnection<ServerConnection>),
    ClientConnectionClosed(ClientId), // TODO Might add a ConnectionError
}

#[derive(Debug, Clone)]
pub(crate) enum ServerSyncMessage {
    ClientConnectedAck(ClientId),
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
        let server_cert = retrieve_certificate(cert_mode)?;
        let mut quinn_endpoint_config = ServerConfig::with_single_cert(
            server_cert.cert_chain.clone(),
            server_cert.priv_key.clone_key(),
        )?;
        Arc::get_mut(&mut quinn_endpoint_config.transport)
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
                quinn_endpoint_config,
                to_sync_endpoint_send.clone(),
                endpoint_close_recv,
                config.connections_config,
            )
            .await;
        });

        let mut endpoint = Endpoint::new(
            endpoint_close_send,
            from_async_endpoint_recv,
            config.clear_stale_received_payloads,
        );
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
    connections_config: ConnectionConfig,
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
                        let connection_config = connections_config.clone();
                        tokio::spawn(async move {
                            client_connection_task(
                                connection,
                                to_sync_endpoint_send,
                                connection_config
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
    connections_config: ConnectionConfig,
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
        .send(ServerAsyncMessage::ClientConnected(PeerConnection::new(
            ServerConnection::new(connection_handle.clone(), to_connection_send),
            bytes_from_client_recv,
            client_close_send.clone(),
            from_channels_recv,
            to_channels_send,
            connections_config,
        )))
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
            client_id::spawn_client_id_sender(
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

/// - Receives events from the async server tasks
/// - Dispatches received payloads to the appropriate channels
/// - Updates the sync server state
///
/// This system generates the server's bevy events
pub fn handle_server_events_and_dispatch_payloads(
    mut server: ResMut<QuinnetServer>,
    mut connection_events: EventWriter<ConnectionEvent>,
    mut connection_lost_events: EventWriter<ConnectionLostEvent>,
    mut lost_clients: Local<HashSet<ClientId>>,
) {
    let Some(endpoint) = server.get_endpoint_mut() else {
        return;
    };

    while let Ok(endpoint_message) = endpoint.try_recv_from_async() {
        match endpoint_message {
            ServerAsyncMessage::ClientConnected(new_connection) => {
                match endpoint.handle_new_connection(new_connection) {
                    Ok(client_id) => {
                        connection_events.write(ConnectionEvent { id: client_id });
                    }
                    Err(_) => {
                        error!("Failed to handle connection of a client, already disconnected");
                    }
                };
            }
            ServerAsyncMessage::ClientConnectionClosed(client_id) => {
                if endpoint.clients.contains_key(&client_id) {
                    endpoint.try_disconnect_closed_client(client_id);
                    connection_lost_events.write(ConnectionLostEvent { id: client_id });
                }
            }
        }
    }

    for (client_id, connection) in endpoint.clients.iter_mut() {
        while let Ok(message) = connection.try_recv_from_channels() {
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

    for client_id in lost_clients.drain() {
        endpoint.try_disconnect_client(client_id);
    }

    endpoint.dispatch_received_payloads();
}

/// Clears stale payloads on all receive channels
pub fn clear_stale_client_payloads(mut server: ResMut<QuinnetServer>) {
    let Some(endpoint) = server.get_endpoint_mut() else {
        return;
    };

    if endpoint.clear_stale_received_payloads {
        endpoint.clear_stale_payloads_from_clients();
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
            handle_server_events_and_dispatch_payloads
                .in_set(QuinnetSyncPreUpdate)
                .run_if(resource_exists::<QuinnetServer>),
        );
        app.add_systems(
            Last,
            clear_stale_client_payloads
                .in_set(QuinnetSyncPostUpdate)
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
