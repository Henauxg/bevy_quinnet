use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

use bevy::prelude::*;
use bytes::Bytes;
use futures::sink::SinkExt;
use futures_util::StreamExt;
use quinn::{Endpoint as QuinnEndpoint, SendStream, ServerConfig};
use quinn_proto::VarInt;
use serde::Deserialize;
use tokio::{
    runtime,
    sync::{
        broadcast::{self},
        mpsc::{self, error::TryRecvError},
    },
    task::JoinSet,
};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

use crate::{
    server::certificate::retrieve_certificate,
    shared::{
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
pub(crate) enum InternalAsyncMessage {
    ClientConnected(ClientConnection),
    ClientLostConnection(ClientId),
}

#[derive(Debug, Clone)]
pub(crate) enum InternalSyncMessage {
    ClientConnectedAck(ClientId),
}

#[derive(Debug)]
pub(crate) struct ClientConnection {
    client_id: ClientId,
    sender: mpsc::Sender<Bytes>,
    close_sender: broadcast::Sender<()>,
}

pub struct Endpoint {
    clients: HashMap<ClientId, ClientConnection>,
    payloads_receiver: mpsc::Receiver<ClientPayload>,
    close_sender: broadcast::Sender<()>,

    pub(crate) internal_receiver: mpsc::Receiver<InternalAsyncMessage>,
    pub(crate) internal_sender: broadcast::Sender<InternalSyncMessage>,
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

    pub fn send_message<T: serde::Serialize>(
        &mut self,
        client_id: ClientId,
        message: T,
    ) -> Result<(), QuinnetError> {
        match bincode::serialize(&message) {
            Ok(payload) => Ok(self.send_payload(client_id, payload)?),
            Err(_) => Err(QuinnetError::Serialization),
        }
    }

    pub fn try_send_message<T: serde::Serialize>(&mut self, client_id: ClientId, message: T) {
        match self.send_message(client_id, message) {
            Ok(_) => {}
            Err(err) => error!("try_send_message: {}", err),
        }
    }

    pub fn send_group_message<'a, I: Iterator<Item = &'a ClientId>, T: serde::Serialize>(
        &self,
        client_ids: I,
        message: T,
    ) -> Result<(), QuinnetError> {
        match bincode::serialize(&message) {
            Ok(payload) => {
                for id in client_ids {
                    self.send_payload(*id, payload.clone())?;
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

    pub fn broadcast_message<T: serde::Serialize>(&self, message: T) -> Result<(), QuinnetError> {
        match bincode::serialize(&message) {
            Ok(payload) => Ok(self.broadcast_payload(payload)?),
            Err(_) => Err(QuinnetError::Serialization),
        }
    }

    pub fn try_broadcast_message<T: serde::Serialize>(&self, message: T) {
        match self.broadcast_message(message) {
            Ok(_) => {}
            Err(err) => error!("try_broadcast_message: {}", err),
        }
    }

    pub fn broadcast_payload<T: Into<Bytes> + Clone>(
        &self,
        payload: T,
    ) -> Result<(), QuinnetError> {
        for (_, client_connection) in self.clients.iter() {
            match client_connection.sender.try_send(payload.clone().into()) {
                Ok(_) => {}
                Err(err) => match err {
                    mpsc::error::TrySendError::Full(_) => return Err(QuinnetError::FullQueue),
                    mpsc::error::TrySendError::Closed(_) => {
                        return Err(QuinnetError::ChannelClosed)
                    }
                },
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

    pub fn send_payload<T: Into<Bytes>>(
        &self,
        client_id: ClientId,
        payload: T,
    ) -> Result<(), QuinnetError> {
        if let Some(client) = self.clients.get(&client_id) {
            match client.sender.try_send(payload.into()) {
                Ok(_) => Ok(()),
                Err(err) => match err {
                    mpsc::error::TrySendError::Full(_) => Err(QuinnetError::FullQueue),
                    mpsc::error::TrySendError::Closed(_) => Err(QuinnetError::ChannelClosed),
                },
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

    pub fn receive_payload(&mut self) -> Result<Option<ClientPayload>, QuinnetError> {
        match self.payloads_receiver.try_recv() {
            Ok(msg) => Ok(Some(msg)),
            Err(err) => match err {
                TryRecvError::Empty => Ok(None),
                TryRecvError::Disconnected => Err(QuinnetError::ChannelClosed),
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

    pub fn disconnect_client(&mut self, client_id: ClientId) -> Result<(), QuinnetError> {
        match self.clients.remove(&client_id) {
            Some(client_connection) => match client_connection.close_sender.send(()) {
                Ok(_) => Ok(()),
                Err(_) => Err(QuinnetError::ChannelClosed),
            },
            None => Err(QuinnetError::UnknownClient(client_id)),
        }
    }

    pub fn disconnect_all_clients(&mut self) -> Result<(), QuinnetError> {
        for client_id in self.clients.keys().cloned().collect::<Vec<ClientId>>() {
            self.disconnect_client(client_id)?;
        }
        Ok(())
    }

    pub(crate) fn close_incoming_connections_handler(&mut self) -> Result<(), QuinnetError> {
        match self.close_sender.send(()) {
            Ok(_) => Ok(()),
            Err(_) => Err(QuinnetError::ChannelClosed),
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

    /// Run the server with the given [ServerConfigurationData] and [CertificateRetrievalMode]
    pub fn start_endpoint(
        &mut self,
        config: ServerConfigurationData,
        cert_mode: CertificateRetrievalMode,
    ) -> Result<ServerCertificate, QuinnetError> {
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

        let (from_clients_sender, from_clients_receiver) =
            mpsc::channel::<ClientPayload>(DEFAULT_MESSAGE_QUEUE_SIZE);
        let (to_sync_server, from_async_server) =
            mpsc::channel::<InternalAsyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);
        let (to_async_server, from_sync_server) =
            broadcast::channel::<InternalSyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);
        // Create a close channel for this endpoint
        let (endpoint_close_sender, endpoint_close_receiver) =
            broadcast::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);

        info!("Starting endpoint on: {} ...", server_adr_str);

        self.runtime.spawn(async move {
            endpoint_task(
                server_config,
                server_addr,
                to_sync_server.clone(),
                endpoint_close_receiver,
                from_sync_server,
                from_clients_sender.clone(),
            )
            .await;
        });

        self.endpoint = Some(Endpoint {
            clients: HashMap::new(),
            payloads_receiver: from_clients_receiver,
            close_sender: endpoint_close_sender,
            internal_receiver: from_async_server,
            internal_sender: to_async_server.clone(),
        });

        Ok(server_cert)
    }

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
    to_sync_server: mpsc::Sender<InternalAsyncMessage>,
    mut close_receiver: broadcast::Receiver<()>,
    mut from_sync_server: broadcast::Receiver<InternalSyncMessage>,
    from_clients_sender: mpsc::Sender<ClientPayload>,
) {
    let mut client_gen_id: ClientId = 0;
    let mut client_id_mappings = HashMap::new();

    let endpoint = QuinnEndpoint::server(endpoint_config, endpoint_adr)
        .expect("Failed to create the endpoint");
    // Handle incoming connections/clients.
    tokio::select! {
        _ = close_receiver.recv() => {
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

                        handle_client_connection(
                            connection,
                            client_id,
                            &to_sync_server,
                            &mut from_sync_server,
                            from_clients_sender.clone(),
                        )
                        .await;
                    },
                }

            }
        } => {}
    }
}

async fn handle_client_connection(
    connection: quinn::Connection,
    client_id: ClientId,
    to_sync_server: &mpsc::Sender<InternalAsyncMessage>,
    from_sync_server: &mut broadcast::Receiver<InternalSyncMessage>,
    from_clients_sender: mpsc::Sender<ClientPayload>,
) {
    info!(
        "New connection from {}, client_id: {}, stable_id : {}",
        connection.remote_address(),
        client_id,
        connection.stable_id()
    );

    // Create a close channel for this client
    let (client_close_sender, client_close_receiver) =
        broadcast::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);

    // Create an ordered reliable send channel for this client
    let (to_client_sender, to_client_receiver) = mpsc::channel::<Bytes>(DEFAULT_MESSAGE_QUEUE_SIZE);

    let _client_sender = {
        let to_sync_server_clone = to_sync_server.clone();
        let close_sender_clone = client_close_sender.clone();
        let connection_clone = connection.clone();
        tokio::spawn(async move {
            client_sender_task(
                client_id,
                connection_clone,
                to_client_receiver,
                client_close_receiver,
                close_sender_clone,
                to_sync_server_clone,
            )
            .await
        })
    };

    // Signal the sync server of this new connection
    to_sync_server
        .send(InternalAsyncMessage::ClientConnected(ClientConnection {
            client_id: client_id,
            sender: to_client_sender,
            close_sender: client_close_sender.clone(),
        }))
        .await
        .expect("Failed to signal connection to sync client");

    // Wait for the sync server to acknowledge the connection before spawning reception tasks.
    while let Ok(InternalSyncMessage::ClientConnectedAck(id)) = from_sync_server.recv().await {
        if id == client_id {
            break;
        }
    }

    let _client_close_wait = {
        let conn = connection.clone();
        let close_sender = client_close_sender.clone();
        let to_sync_server = to_sync_server.clone();
        tokio::spawn(async move {
            let conn_err = conn.closed().await;
            info!("Client {} disconnected: {}", client_id, conn_err);
            close_sender.send(()).ok();
            to_sync_server
                .send(InternalAsyncMessage::ClientLostConnection(client_id))
                .await
                .expect("Failed to signal connection lost to sync server");
        });
    };

    // Spawn a task to listen for streams opened by this client
    let _client_receiver = tokio::spawn(async move {
        client_receiver_task(
            client_id,
            connection,
            client_close_sender.subscribe(),
            from_clients_sender,
        )
        .await
    });
}

async fn client_sender_task(
    client_id: ClientId,
    connection: quinn::Connection,
    mut to_client_receiver: tokio::sync::mpsc::Receiver<Bytes>,
    mut close_receiver: tokio::sync::broadcast::Receiver<()>,
    close_sender: tokio::sync::broadcast::Sender<()>,
    to_sync_server: mpsc::Sender<InternalAsyncMessage>,
) {
    let send_stream = connection.open_uni().await.expect(
        format!(
            "Failed to open unidirectional send stream for client: {}",
            client_id
        )
        .as_str(),
    );

    let mut framed_send_stream = FramedWrite::new(send_stream, LengthDelimitedCodec::new());

    tokio::select! {
        _ = close_receiver.recv() => {
            trace!("Unidirectional send stream forced to disconnected for client: {}", client_id)
        }
        _ = async {
            while let Some(msg_bytes) = to_client_receiver.recv().await {
                send_msg(
                    client_id,
                    &close_sender,
                    &to_sync_server,
                    &mut framed_send_stream,
                    msg_bytes,
                )
                .await
            }
        } => {}
    }
    while let Ok(msg_bytes) = to_client_receiver.try_recv() {
        if let Err(err) = framed_send_stream.send(msg_bytes.clone()).await {
            error!("Error while sending to client {}: {}", client_id, err);
        };
    }
    if let Err(err) = framed_send_stream.flush().await {
        error!(
            "Error while flushing stream to client {}: {}",
            client_id, err
        );
    }
    if let Err(err) = framed_send_stream.into_inner().finish().await {
        error!(
            "Failed to shutdown stream gracefully for client {}: {}",
            client_id, err
        );
    }
    connection.close(VarInt::from_u32(0), "closed".as_bytes());
}

async fn send_msg(
    client_id: ClientId,
    close_sender: &tokio::sync::broadcast::Sender<()>,
    to_sync_server: &mpsc::Sender<InternalAsyncMessage>,
    framed_send_stream: &mut FramedWrite<SendStream, LengthDelimitedCodec>,
    msg_bytes: Bytes,
) {
    // TODO Perf: Batch frames for a send_all
    if let Err(err) = framed_send_stream.send(msg_bytes.clone()).await {
        error!("Error while sending to client {}: {}", client_id, err);
        error!("Client {} seems disconnected, closing resources", client_id);
        // Emit ClientLostConnection to properly update the server about this client state.
        // Raise ClientLostConnection event before emitting a close signal because we have no guarantee to continue this async execution after the close signal has been processed.
        to_sync_server
            .send(InternalAsyncMessage::ClientLostConnection(client_id))
            .await
            .expect("Failed to signal connection lost to sync server");
        if let Err(_) = close_sender.send(()) {
            error!(
                "Failed to close all client streams & resources for client {}",
                client_id
            )
        }
    };
}

async fn client_receiver_task(
    client_id: ClientId,
    connection: quinn::Connection,
    mut close_receiver: tokio::sync::broadcast::Receiver<()>,
    from_clients_sender: mpsc::Sender<ClientPayload>,
) {
    let mut uni_receivers: JoinSet<()> = JoinSet::new();
    tokio::select! {
        _ = close_receiver.recv() => {
            trace!("New Stream listener forced to disconnected for client: {}", client_id)
        }
        _ = async {
            // For each new stream opened by the client
            while let Ok(recv) = connection.accept_uni().await {
                let mut frame_recv = FramedRead::new(recv, LengthDelimitedCodec::new());

                // Spawn a task to receive data on this stream.
                let from_client_sender = from_clients_sender.clone();
                uni_receivers.spawn(async move {
                    while let Some(Ok(msg_bytes)) = frame_recv.next().await {
                        from_client_sender
                            .send(ClientPayload {
                                client_id: client_id,
                                msg: msg_bytes.into(),
                            })
                            .await
                            .unwrap();// TODO Fix: error event
                    }
                    trace!("Unidirectional stream receiver ended for client: {}", client_id)
                });
            }
        } => {
            trace!("New Stream listener ended for client: {}", client_id)
        }
    }
    uni_receivers.shutdown().await;
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
        while let Ok(message) = endpoint.internal_receiver.try_recv() {
            match message {
                InternalAsyncMessage::ClientConnected(connection) => {
                    let id = connection.client_id;
                    endpoint.clients.insert(id, connection);
                    endpoint
                        .internal_sender
                        .send(InternalSyncMessage::ClientConnectedAck(id))
                        .unwrap();
                    connection_events.send(ConnectionEvent { id: id });
                }
                InternalAsyncMessage::ClientLostConnection(client_id) => {
                    match endpoint.clients.remove(&client_id) {
                        Some(_) => {
                            connection_lost_events.send(ConnectionLostEvent { id: client_id })
                        }
                        None => (),
                    }
                }
            }
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
