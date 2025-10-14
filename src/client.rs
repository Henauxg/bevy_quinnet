use std::{
    collections::{
        hash_map::{Iter, IterMut},
        HashMap,
    },
    sync::Mutex,
};

use bevy::prelude::*;

use tokio::{
    runtime::{self},
    sync::oneshot,
};

use crate::{
    client::connection::{create_client_connection_async_channels, ClientConnection},
    shared::{
        channels::{ChannelAsyncMessage, SendChannelsConfiguration},
        error::AsyncChannelError,
        AsyncRuntime, ClientId, InternalConnectionRef, QuinnetSyncPreUpdate,
    },
};

use self::{
    certificate::{
        CertConnectionAbortEvent, CertInteractionEvent, CertTrustUpdateEvent, CertVerificationInfo,
        CertVerificationStatus, CertVerifierAction, CertificateVerificationMode,
    },
    connection::{
        async_connection_task, ClientAddrConfiguration, ClientSideConnection, ConnectionEvent,
        ConnectionFailedEvent, ConnectionLocalId, ConnectionLostEvent, ConnectionState,
        InternalConnectionState,
    },
};

/// Module for the client's certificate features
pub mod certificate;
/// Module for a client's connection to a server
pub mod connection;

mod error;
pub use error::*;

/// Default path for the known hosts file
pub const DEFAULT_KNOWN_HOSTS_FILE: &str = "quinnet/known_hosts";

/// Configuration for a client's connection to a server
#[derive(Debug, Clone)]
pub struct ClientConnectionConfiguration {
    /// See [ClientAddrConfiguration]
    pub addr_config: ClientAddrConfiguration,
    /// How the client should verify the server's certificate
    pub cert_mode: CertificateVerificationMode,
    /// Configuration for a [ClientConnectionConfiguration] that can be defaulted
    pub defaultables: ClientConnectionConfigurationDefaultables,
}

/// Every configuration fields of a client's connection to a server that can be defaulted
#[derive(Debug, Default, Clone)]
pub struct ClientConnectionConfigurationDefaultables {
    /// Configuration of the send channels opened on the connection
    pub send_channels_cfg: SendChannelsConfiguration,
    /// Configuration for the receive channels on the connection
    #[cfg(feature = "recv_channels")]
    pub recv_channels_cfg: crate::shared::peer_connection::RecvChannelsConfiguration,
}

/// Possible errors occuring while a client is connecting to a server
#[derive(thiserror::Error, Debug, Clone)]
pub enum QuinnetConnectionError {
    /// A quic error occurred during the connection
    #[error("Quic connection error")]
    QuicConnectionError(#[from] quinn::ConnectionError),
    /// Client received an invalid client id
    #[error("Client received an invalid client id")]
    InvalidClientId,
    /// Client did not receive its client id
    #[error("Client did not receive its client id")]
    ClientIdNotReceived,
}

#[derive(Debug)]
pub(crate) enum ClientAsyncMessage {
    Connected(InternalConnectionRef, Option<ClientId>),
    ConnectionFailed(QuinnetConnectionError),
    ConnectionClosed, // TODO Might set a ConnectionError
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

/// Main quinnet client. Can open multiple [`ClientSideConnection`] with multiple quinnet servers
///
/// Created by the [`QuinnetClientPlugin`] or inserted manually via a call to [`bevy::prelude::World::insert_resource`]. When created, it will look for an existing [`AsyncRuntime`] resource and use it or create one itself.
#[derive(Resource)]
pub struct QuinnetClient {
    runtime: runtime::Handle,
    connections: HashMap<ConnectionLocalId, ClientSideConnection>,
    connection_local_id_gen: ConnectionLocalId,
    default_connection_id: Option<ConnectionLocalId>,
}

impl FromWorld for QuinnetClient {
    fn from_world(world: &mut World) -> Self {
        if world.get_resource::<AsyncRuntime>().is_none() {
            let async_runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            world.insert_resource(AsyncRuntime(async_runtime));
        };

        let runtime = world.resource::<AsyncRuntime>();
        QuinnetClient::new(runtime.handle().clone())
    }
}

impl QuinnetClient {
    fn new(runtime_handle: tokio::runtime::Handle) -> Self {
        Self {
            connections: HashMap::new(),
            runtime: runtime_handle,
            connection_local_id_gen: 0,
            default_connection_id: None,
        }
    }

    /// Returns true if the default connection exists and is connecting.
    pub fn is_connecting(&self) -> bool {
        match self.get_connection() {
            Some(connection) => connection.state() == ConnectionState::Connecting,
            None => false,
        }
    }

    /// Returns true if the default connection exists and is connected.
    pub fn is_connected(&self) -> bool {
        match self.get_connection() {
            Some(connection) => connection.state() == ConnectionState::Connected,
            None => false,
        }
    }

    /// Returns true if the default connection does not exists or is disconnected.
    pub fn is_disconnected(&self) -> bool {
        match self.get_connection() {
            Some(connection) => connection.state() == ConnectionState::Disconnected,
            None => true,
        }
    }

    /// Returns the default connection or None.
    pub fn get_connection(&self) -> Option<&ClientSideConnection> {
        match self.default_connection_id {
            Some(id) => self.connections.get(&id),
            None => None,
        }
    }

    /// Returns the default connection as mut or None.
    pub fn get_connection_mut(&mut self) -> Option<&mut ClientSideConnection> {
        match self.default_connection_id {
            Some(id) => self.connections.get_mut(&id),
            None => None,
        }
    }

    /// Returns the default connection. **Warning**, this function panics if there is no default connection.
    pub fn connection(&self) -> &ClientSideConnection {
        self.connections
            .get(&self.default_connection_id.unwrap())
            .unwrap()
    }

    /// Returns the default connection as mut. **Warning**, this function panics if there is no default connection.
    pub fn connection_mut(&mut self) -> &mut ClientSideConnection {
        self.connections
            .get_mut(&self.default_connection_id.unwrap())
            .unwrap()
    }

    /// Returns the requested connection.
    pub fn get_connection_by_id(&self, id: ConnectionLocalId) -> Option<&ClientSideConnection> {
        self.connections.get(&id)
    }

    /// Returns the requested connection as mut.
    pub fn get_connection_mut_by_id(
        &mut self,
        id: ConnectionLocalId,
    ) -> Option<&mut ClientSideConnection> {
        self.connections.get_mut(&id)
    }

    /// Returns an iterator over all connections
    pub fn connections(&'_ self) -> Iter<'_, ConnectionLocalId, ClientSideConnection> {
        self.connections.iter()
    }

    /// Returns an iterator over all connections as muts
    pub fn connections_mut(&'_ mut self) -> IterMut<'_, ConnectionLocalId, ClientSideConnection> {
        self.connections.iter_mut()
    }

    /// Opens a connection to a server.
    ///
    /// The connection will raise an event when fully connected, see [ConnectionEvent]
    ///
    /// Returns the [ConnectionLocalId]
    pub fn open_connection(
        &mut self,
        config: ClientConnectionConfiguration,
    ) -> Result<ConnectionLocalId, AsyncChannelError> {
        // Generate a local connection id
        let local_id = self.connection_local_id_gen;
        self.connection_local_id_gen += 1;

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

        let mut connection = ClientSideConnection::new(
            ClientConnection::new(
                local_id,
                self.runtime.clone(),
                config.addr_config.clone(),
                config.cert_mode.clone(),
                config.defaultables.send_channels_cfg.clone(),
                to_sync_client_recv,
            ),
            bytes_from_server_recv,
            close_send,
            from_channels_recv,
            to_channels_send,
            #[cfg(feature = "recv_channels")]
            config.defaultables.recv_channels_cfg,
        );

        connection.open_configured_channels(config.defaultables.send_channels_cfg)?;

        self.connections.insert(local_id, connection);
        if self.default_connection_id.is_none() {
            self.default_connection_id = Some(local_id);
        }

        // Async connection
        self.runtime.spawn(async move {
            async_connection_task(
                local_id,
                config.addr_config,
                config.cert_mode,
                to_sync_client_send,
                bytes_from_server_send,
                to_channels_recv,
                from_channels_send,
                close_recv,
            )
            .await
        });

        Ok(local_id)
    }

    /// Set the default connection
    pub fn set_default_connection(&mut self, connection_id: ConnectionLocalId) {
        self.default_connection_id = Some(connection_id);
    }

    /// Get the default Connection Id
    pub fn get_default_connection(&self) -> Option<ConnectionLocalId> {
        self.default_connection_id
    }

    /// Closes a specific connection. Removes it from the client.
    ///
    /// Closing a connection immediately prevents new messages from being sent on the connection and signal it to closes all its background tasks. Before trully closing, the connection will wait for all buffered messages in all its opened channels to be properly sent according to their respective channel type.
    ///
    /// This may fail if no [ClientSideConnection] if found for `connection_id`, or if the connection is already closed.
    pub fn close_connection(
        &mut self,
        connection_id: ConnectionLocalId,
    ) -> Result<(), ClientConnectionCloseError> {
        match self.connections.remove(&connection_id) {
            Some(mut connection) => {
                if Some(connection_id) == self.default_connection_id {
                    self.default_connection_id = None;
                }
                connection.disconnect()
            }
            None => Err(ClientConnectionCloseError::InvalidConnectionId(
                connection_id,
            )),
        }
    }

    /// Calls [Self::close_connection] on all the open connections.
    pub fn close_all_connections(&mut self) {
        for connection_id in self
            .connections
            .keys()
            .cloned()
            .collect::<Vec<ConnectionLocalId>>()
        {
            let _ = self.close_connection(connection_id);
        }
    }
}

/// Receive messages from the async client tasks and update the sync client.
///
/// This system generates client's bevy events
pub fn handle_client_events(
    mut connection_events: MessageWriter<ConnectionEvent>,
    mut connection_failed_events: MessageWriter<ConnectionFailedEvent>,
    mut connection_lost_events: MessageWriter<ConnectionLostEvent>,
    mut certificate_interaction_events: MessageWriter<CertInteractionEvent>,
    mut cert_trust_update_events: MessageWriter<CertTrustUpdateEvent>,
    mut cert_connection_abort_events: MessageWriter<CertConnectionAbortEvent>,
    mut client: ResMut<QuinnetClient>,
) {
    for (connection_id, connection) in &mut client.connections {
        while let Ok(message) = connection.try_recv_from_async() {
            match message {
                ClientAsyncMessage::Connected(internal_connection, client_id) => {
                    connection.set_state(InternalConnectionState::Connected(
                        internal_connection,
                        client_id,
                    ));
                    connection_events.write(ConnectionEvent {
                        id: *connection_id,
                        client_id,
                    });
                }
                ClientAsyncMessage::ConnectionFailed(err) => {
                    connection.set_state(InternalConnectionState::Disconnected);
                    connection_failed_events.write(ConnectionFailedEvent {
                        id: *connection_id,
                        err,
                    });
                }
                ClientAsyncMessage::ConnectionClosed => match connection.internal_state() {
                    InternalConnectionState::Disconnected => (),
                    _ => {
                        connection.try_disconnect_closed_connection();
                        connection_lost_events.write(ConnectionLostEvent { id: *connection_id });
                    }
                },
                ClientAsyncMessage::CertificateInteractionRequest {
                    status,
                    info,
                    action_sender,
                } => {
                    certificate_interaction_events.write(CertInteractionEvent {
                        connection_id: *connection_id,
                        status,
                        info,
                        action_sender: Mutex::new(Some(action_sender)),
                    });
                }
                ClientAsyncMessage::CertificateTrustUpdate(info) => {
                    cert_trust_update_events.write(CertTrustUpdateEvent {
                        connection_id: *connection_id,
                        cert_info: info,
                    });
                }
                ClientAsyncMessage::CertificateConnectionAbort { status, cert_info } => {
                    cert_connection_abort_events.write(CertConnectionAbortEvent {
                        connection_id: *connection_id,
                        status,
                        cert_info,
                    });
                }
            }
        }
        while let Ok(message) = connection.try_recv_from_channels() {
            match message {
                ChannelAsyncMessage::LostConnection => match connection.internal_state() {
                    InternalConnectionState::Disconnected => (),
                    _ => {
                        connection.try_disconnect_closed_connection();
                        connection_lost_events.write(ConnectionLostEvent { id: *connection_id });
                    }
                },
            }
        }
    }
}

#[cfg(feature = "recv_channels")]
/// Type alias for the recv channel error event for the client
pub type ClientRecvChannelError = crate::shared::error::RecvChannelErrorEvent<ConnectionLocalId>;

#[cfg(feature = "recv_channels")]
/// Dispatches received payloads to their respective channel buffers
///
/// This system generates client's bevy events
pub fn dispatch_received_payloads(
    mut recv_error_events: MessageWriter<ClientRecvChannelError>,
    mut client: ResMut<QuinnetClient>,
) {
    for (connection_id, connection) in &mut client.connections {
        match connection.internal_state() {
            InternalConnectionState::Disconnected => (),
            _ => {
                if let Err(recv_errors) = connection.dispatch_received_payloads_to_channel_buffers()
                {
                    for error in recv_errors {
                        error!(
                            "Error while dispatching received payloads to channel buffers: {}",
                            error
                        );
                        recv_error_events.write(ClientRecvChannelError {
                            id: *connection_id,
                            error,
                        });
                    }
                }
            }
        }
    }
}

#[cfg(feature = "recv_channels")]
/// Clears stale payloads on all receive channels
pub fn clear_stale_received_payloads(mut client: ResMut<QuinnetClient>) {
    for connection in client.connections.values_mut() {
        connection.clear_stale_received_payloads();
    }
}

/// Quinnet Client's plugin
///
/// It is possbile to add both this plugin and the [`crate::server::QuinnetServerPlugin`]
#[derive(Default)]
pub struct QuinnetClientPlugin {
    /// In order to have more control and only do the strict necessary, which is registering systems and events in the Bevy schedule, `initialize_later` can be set to `true`. This will prevent the plugin from initializing the `Client` Resource.
    /// Client systems are scheduled to only run if the `Client` resource exists.
    /// A Bevy command to create the resource `commands.init_resource::<Client>();` can be done later on, when needed.
    pub initialize_later: bool,
}

impl Plugin for QuinnetClientPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<ConnectionEvent>()
            .add_message::<ConnectionFailedEvent>()
            .add_message::<ConnectionLostEvent>()
            .add_message::<CertInteractionEvent>()
            .add_message::<CertTrustUpdateEvent>()
            .add_message::<CertConnectionAbortEvent>();

        if !self.initialize_later {
            app.init_resource::<QuinnetClient>();
        }

        app.add_systems(
            PreUpdate,
            handle_client_events
                .in_set(QuinnetSyncPreUpdate)
                .run_if(resource_exists::<QuinnetClient>),
        );
        #[cfg(feature = "recv_channels")]
        {
            app.add_message::<ClientRecvChannelError>();
            app.add_systems(
                PreUpdate,
                dispatch_received_payloads
                    .in_set(QuinnetSyncPreUpdate)
                    .run_if(resource_exists::<QuinnetClient>),
            );
            app.add_systems(
                Last,
                clear_stale_received_payloads
                    .in_set(crate::shared::QuinnetSyncLast)
                    .run_if(resource_exists::<QuinnetClient>),
            );
        }
    }
}

/// Returns true if the following conditions are all true:
/// - the client Resource exists
/// - its default connection is connecting.
pub fn client_connecting(client: Option<Res<QuinnetClient>>) -> bool {
    match client {
        Some(client) => client.is_connecting(),
        None => false,
    }
}

/// Returns true if the following conditions are all true:
/// - the client Resource exists
/// - its default connection is connected.
pub fn client_connected(client: Option<Res<QuinnetClient>>) -> bool {
    match client {
        Some(client) => client.is_connected(),
        None => false,
    }
}

/// Returns true if the following conditions are all true:
/// - the client Resource exists and its default connection is connected
/// - the previous condition was false during the previous update
pub fn client_just_connected(
    mut last_connected: Local<bool>,
    client: Option<Res<QuinnetClient>>,
) -> bool {
    let connected = client.map(|client| client.is_connected()).unwrap_or(false);

    let just_connected = !*last_connected && connected;
    *last_connected = connected;
    just_connected
}

/// Returns true if the following conditions are all true:
/// - the client Resource does not exists or its default connection is disconnected
/// - the previous condition was false during the previous update
pub fn client_just_disconnected(
    mut last_connected: Local<bool>,
    client: Option<Res<QuinnetClient>>,
) -> bool {
    let disconnected = client
        .map(|client| client.is_disconnected())
        .unwrap_or(true);

    let just_disconnected = *last_connected && disconnected;
    *last_connected = !disconnected;
    just_disconnected
}
