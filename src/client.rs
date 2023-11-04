use std::{
    collections::{
        hash_map::{Iter, IterMut},
        HashMap,
    },
    sync::Mutex,
};

use bevy::prelude::*;
use bytes::Bytes;
use quinn::ConnectionError;
use tokio::{
    runtime::{self},
    sync::{
        broadcast,
        mpsc::{self},
        oneshot,
    },
};

use crate::shared::{
    channel::{ChannelAsyncMessage, ChannelId, ChannelSyncMessage, ChannelType},
    AsyncRuntime, InternalConnectionRef, QuinnetError, DEFAULT_KILL_MESSAGE_QUEUE_SIZE,
    DEFAULT_MESSAGE_QUEUE_SIZE,
};

use self::{
    certificate::{
        CertConnectionAbortEvent, CertInteractionEvent, CertTrustUpdateEvent, CertVerificationInfo,
        CertVerificationStatus, CertVerifierAction, CertificateVerificationMode,
    },
    connection::{
        connection_task, Connection, ConnectionConfiguration, ConnectionEvent, ConnectionId,
        ConnectionLostEvent, ConnectionState,
    },
};

pub mod certificate;
pub mod connection;

pub const DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE: usize = 100;
pub const DEFAULT_KNOWN_HOSTS_FILE: &str = "quinnet/known_hosts";

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

#[derive(Resource)]
pub struct Client {
    runtime: runtime::Handle,
    connections: HashMap<ConnectionId, Connection>,
    last_gen_id: ConnectionId,
    default_connection_id: Option<ConnectionId>,
}

impl FromWorld for Client {
    fn from_world(world: &mut World) -> Self {
        if world.get_resource::<AsyncRuntime>().is_none() {
            let async_runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            world.insert_resource(AsyncRuntime(async_runtime));
        };

        let runtime = world.resource::<AsyncRuntime>();
        Client::new(runtime.handle().clone())
    }
}

impl Client {
    fn new(runtime_handle: tokio::runtime::Handle) -> Self {
        Self {
            connections: HashMap::new(),
            runtime: runtime_handle,
            last_gen_id: 0,
            default_connection_id: None,
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
        let (close_send, close_recv) = broadcast::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);

        let mut connection = Connection::new(
            bytes_from_server_recv,
            close_send.clone(),
            from_async_client_recv,
            to_channels_send,
            from_channels_recv,
        );
        // Create default channels
        let ordered_reliable_id = connection.open_channel(ChannelType::OrderedReliable)?;
        connection.open_channel(ChannelType::UnorderedReliable)?;
        connection.open_channel(ChannelType::Unreliable)?;

        self.last_gen_id += 1;
        let connection_id = self.last_gen_id;
        self.connections.insert(connection_id, connection);
        if self.default_connection_id.is_none() {
            self.default_connection_id = Some(connection_id);
        }

        // Async connection
        self.runtime.spawn(async move {
            connection_task(
                connection_id,
                config,
                cert_mode,
                to_sync_client_send,
                to_channels_recv,
                from_channels_send,
                close_recv,
                bytes_from_server_send,
            )
            .await
        });

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

    /// Close a specific connection. Removes it from the client.
    ///
    /// Closign a connection immediately prevents new messages from being sent on the connection and signal it to closes all its background tasks. Before trully closing, the connection will wait for all buffered messages in all its opened channels to be properly sent according to their respective channel type.
    ///
    /// This may fail if no [Connection] if found for connection_id, or if the [Connection] is already closed.
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

// Receive messages from the async client tasks and update the sync client.
fn update_sync_client(
    mut connection_events: EventWriter<ConnectionEvent>,
    mut connection_lost_events: EventWriter<ConnectionLostEvent>,
    mut certificate_interaction_events: EventWriter<CertInteractionEvent>,
    mut cert_trust_update_events: EventWriter<CertTrustUpdateEvent>,
    mut cert_connection_abort_events: EventWriter<CertConnectionAbortEvent>,
    mut client: ResMut<Client>,
) {
    for (connection_id, connection) in &mut client.connections {
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
            .add_event::<ConnectionLostEvent>()
            .add_event::<CertInteractionEvent>()
            .add_event::<CertTrustUpdateEvent>()
            .add_event::<CertConnectionAbortEvent>();

        if !self.initialize_later {
            app.init_resource::<Client>();
        }

        app.add_systems(
            PreUpdate,
            update_sync_client.run_if(resource_exists::<Client>()),
        );
    }
}
