use std::{
    collections::{
        hash_map::{Iter, IterMut},
        HashMap,
    },
    sync::Mutex,
};

use bevy::prelude::*;
use quinn::ConnectionError;
use tokio::{
    runtime::{self},
    sync::oneshot,
};

use crate::shared::{
    channels::{ChannelAsyncMessage, ChannelsConfiguration},
    error::QuinnetError,
    AsyncRuntime, ClientId, InternalConnectionRef, QuinnetSyncUpdate,
};

use self::{
    certificate::{
        CertConnectionAbortEvent, CertInteractionEvent, CertTrustUpdateEvent, CertVerificationInfo,
        CertVerificationStatus, CertVerifierAction, CertificateVerificationMode,
    },
    connection::{
        async_connection_task, create_async_channels, ClientEndpointConfiguration, Connection,
        ConnectionEvent, ConnectionFailedEvent, ConnectionLocalId, ConnectionLostEvent,
        ConnectionState, InternalConnectionState,
    },
};

/// Module for the client's certificate features
pub mod certificate;
/// Module for a client's connection to a server
pub mod connection;

/// Default path for the known hosts file
pub const DEFAULT_KNOWN_HOSTS_FILE: &str = "quinnet/known_hosts";

/// Possible errors occuring while a client is connecting to a server
#[derive(thiserror::Error, Debug)]
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

/// Main quinnet client. Can open multiple [`Connection`] with multiple quinnet servers
///
/// Created by the [`QuinnetClientPlugin`] or inserted manually via a call to [`bevy::prelude::World::insert_resource`]. When created, it will look for an existing [`AsyncRuntime`] resource and use it or create one itself.
#[derive(Resource)]
pub struct QuinnetClient {
    runtime: runtime::Handle,
    connections: HashMap<ConnectionLocalId, Connection>,
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
    pub fn get_connection_by_id(&self, id: ConnectionLocalId) -> Option<&Connection> {
        self.connections.get(&id)
    }

    /// Returns the requested connection as mut.
    pub fn get_connection_mut_by_id(&mut self, id: ConnectionLocalId) -> Option<&mut Connection> {
        self.connections.get_mut(&id)
    }

    /// Returns an iterator over all connections
    pub fn connections(&self) -> Iter<ConnectionLocalId, Connection> {
        self.connections.iter()
    }

    /// Returns an iterator over all connections as muts
    pub fn connections_mut(&mut self) -> IterMut<ConnectionLocalId, Connection> {
        self.connections.iter_mut()
    }

    /// Open a connection to a server with the given [ClientEndpointConfiguration], [CertificateVerificationMode] and [ChannelsConfiguration]. The connection will raise an event when fully connected, see [ConnectionEvent]
    ///
    /// Returns the [ConnectionLocalId]
    pub fn open_connection(
        &mut self,
        endpoint_config: ClientEndpointConfiguration,
        cert_mode: CertificateVerificationMode,
        channels_config: ChannelsConfiguration,
    ) -> Result<ConnectionLocalId, QuinnetError> {
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
        ) = create_async_channels();

        let mut connection = Connection::new(
            local_id,
            self.runtime.clone(),
            endpoint_config.clone(),
            cert_mode.clone(),
            channels_config.clone(),
            bytes_from_server_recv,
            close_send,
            to_sync_client_recv,
            to_channels_send,
            from_channels_recv,
        );
        connection.open_configured_channels(channels_config)?;

        self.connections.insert(local_id, connection);
        if self.default_connection_id.is_none() {
            self.default_connection_id = Some(local_id);
        }

        // Async connection
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

    /// Close a specific connection. Removes it from the client.
    ///
    /// Closign a connection immediately prevents new messages from being sent on the connection and signal it to closes all its background tasks. Before trully closing, the connection will wait for all buffered messages in all its opened channels to be properly sent according to their respective channel type.
    ///
    /// This may fail if no [Connection] if found for connection_id, or if the [Connection] is already closed.
    pub fn close_connection(
        &mut self,
        connection_id: ConnectionLocalId,
    ) -> Result<(), QuinnetError> {
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
            .collect::<Vec<ConnectionLocalId>>()
        {
            self.close_connection(connection_id)?;
        }
        Ok(())
    }
}

/// Receive messages from the async client tasks and update the sync client.
///
/// This system generates the client's bevy events
pub fn update_sync_client(
    mut connection_events: EventWriter<ConnectionEvent>,
    mut connection_failed_events: EventWriter<ConnectionFailedEvent>,
    mut connection_lost_events: EventWriter<ConnectionLostEvent>,
    mut certificate_interaction_events: EventWriter<CertInteractionEvent>,
    mut cert_trust_update_events: EventWriter<CertTrustUpdateEvent>,
    mut cert_connection_abort_events: EventWriter<CertConnectionAbortEvent>,
    mut client: ResMut<QuinnetClient>,
) {
    for (connection_id, connection) in &mut client.connections {
        while let Ok(message) = connection.from_async_client_recv.try_recv() {
            match message {
                ClientAsyncMessage::Connected(internal_connection, client_id) => {
                    connection.state =
                        InternalConnectionState::Connected(internal_connection, client_id);
                    connection_events.send(ConnectionEvent {
                        id: *connection_id,
                        client_id,
                    });
                }
                ClientAsyncMessage::ConnectionFailed(err) => {
                    connection.state = InternalConnectionState::Disconnected;
                    connection_failed_events.send(ConnectionFailedEvent {
                        id: *connection_id,
                        err,
                    });
                }
                ClientAsyncMessage::ConnectionClosed(_) => match connection.state {
                    InternalConnectionState::Disconnected => (),
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
                    InternalConnectionState::Disconnected => (),
                    _ => {
                        connection.try_disconnect();
                        connection_lost_events.send(ConnectionLostEvent { id: *connection_id });
                    }
                },
            }
        }
    }
}

/// Quinnet Server's plugin
///
/// It is possbile to add both this plugin and the [`crate::server::QuinnetServerPlugin`]
pub struct QuinnetClientPlugin {
    /// In order to have more control and only do the strict necessary, which is registering systems and events in the Bevy schedule, `initialize_later` can be set to `true`. This will prevent the plugin from initializing the `Client` Resource.
    /// Client systems are scheduled to only run if the `Client` resource exists.
    /// A Bevy command to create the resource `commands.init_resource::<Client>();` can be done later on, when needed.
    pub initialize_later: bool,
}

impl Default for QuinnetClientPlugin {
    fn default() -> Self {
        Self {
            initialize_later: false,
        }
    }
}

impl Plugin for QuinnetClientPlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<ConnectionEvent>()
            .add_event::<ConnectionFailedEvent>()
            .add_event::<ConnectionLostEvent>()
            .add_event::<CertInteractionEvent>()
            .add_event::<CertTrustUpdateEvent>()
            .add_event::<CertConnectionAbortEvent>();

        if !self.initialize_later {
            app.init_resource::<QuinnetClient>();
        }

        app.add_systems(
            PreUpdate,
            update_sync_client
                .in_set(QuinnetSyncUpdate)
                .run_if(resource_exists::<QuinnetClient>),
        );
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
