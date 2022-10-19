use std::{
    collections::{HashMap},
    error::Error,
    net::SocketAddr,
    sync::{Arc},
    time::Duration,
};

use bevy::prelude::*;
use bytes::Bytes;
use futures::{
    sink::SinkExt, 
};
use futures_util::StreamExt;
use quinn::{Endpoint, NewConnection, ServerConfig};
use serde::Deserialize;
use tokio::{
    runtime::Runtime,
    sync::{
        broadcast::{self},
        mpsc::{
            self,
            error::{TryRecvError},
        },
    },
    task::{ JoinSet},
};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

use crate::{ClientId, QuinnetError, DEFAULT_KEEP_ALIVE_INTERVAL_S, DEFAULT_MESSAGE_QUEUE_SIZE, DEFAULT_KILL_MESSAGE_QUEUE_SIZE};

pub const DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE: usize = 100;

#[derive(Debug)]
pub(crate) enum InternalAsyncMessage {
    ClientConnected(ClientConnection),
}

#[derive(Deserialize)]
pub struct ServerConfigurationData {
    host: String,
    port: u16,
    local_bind_host: String,
}

#[derive(Debug)]
pub struct ClientPayload {
    client_id: ClientId,
    msg: Bytes,
}

#[derive(Debug)]
pub(crate) struct ClientConnection {
    client_id: ClientId, 
    sender: mpsc::Sender<Bytes>,
    close_sender: broadcast::Sender<()>,
}

pub struct Server {
    clients: HashMap<ClientId, ClientConnection>,
    receiver: mpsc::Receiver<ClientPayload>,

    pub(crate) internal_receiver: mpsc::Receiver<InternalAsyncMessage>,
}

impl Server {
    pub fn disconnect_client(&mut self, client_id: ClientId) {
        match self.clients.remove(&client_id) {
            Some(client_connection) => {
                if let Err(_) =  client_connection.close_sender.send(()) {
                    error!("Failed to close client streams & resources while disconnecting client {}", client_id)
                }           
            },
            None => error!("Failed to disconnect client {}, client not found", client_id),
        }
    }

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

    pub fn send_message<T: serde::Serialize>(
        &mut self,
        client_id: ClientId,
        message: T,
    ) -> Result<(), QuinnetError> {
        match bincode::serialize(&message) {
            Ok(payload) => Ok(self.send_payload(client_id, payload)),
            Err(_) => Err(QuinnetError::Serialization),
        }
    }

    pub fn send_group_message<'a, I: Iterator<Item=&'a ClientId>, T: serde::Serialize>(
        &mut self,
        client_ids: I,
        message: T,
    ) -> Result<(), QuinnetError> {
        match bincode::serialize(&message) {
            Ok(payload) => {
                // TODO Fix: Error handling
                for id in client_ids {
                    self.send_payload(*id, payload.clone());
                }
                Ok(())
            },
            Err(_) => Err(QuinnetError::Serialization),
        }
    }

    pub fn broadcast_message<T: serde::Serialize>(
        &mut self,
        message: T,
    ) -> Result<(), QuinnetError> {
        match bincode::serialize(&message) {
            Ok(payload) => Ok(self.broadcast_payload(payload)),
            Err(_) => Err(QuinnetError::Serialization),
        }
    }

    pub fn broadcast_payload<T: Into<Bytes> + Clone>(&mut self, payload: T) {
        // TODO Fix: Error handling
        for (_, client_connection) in self.clients.iter() {
            client_connection.sender.try_send(payload.clone().into()).unwrap();
        }
    }

    pub fn send_payload<T: Into<Bytes>>(&mut self, client_id: ClientId, payload: T) {
        // TODO Fix: Error handling
        if let Some(client) = self.clients.get(&client_id) {
            client.sender.try_send(payload.into()).unwrap();
        }
    }

    //TODO Clean: Consider receiving payloads for a specified client
    pub fn receive_payload(&mut self) -> Result<Option<ClientPayload>, QuinnetError> {
        match self.receiver.try_recv() {
            Ok(msg) => Ok(Some(msg)),
            Err(err) => match err {
                TryRecvError::Empty => Ok(None),
                TryRecvError::Disconnected => Err(QuinnetError::ChannelClosed),
            },
        }
    }
}

/// Returns default server configuration along with its certificate.
#[allow(clippy::field_reassign_with_default)] // https://github.com/rust-lang/rust-clippy/issues/6527
fn configure_server(server_host: &String) -> Result<(ServerConfig, Vec<u8>), Box<dyn Error>> {
    let cert = rcgen::generate_simple_self_signed(vec![server_host.into()]).unwrap();
    let cert_der = cert.serialize_der().unwrap();
    let priv_key = rustls::PrivateKey(cert.serialize_private_key_der());
    let cert_chain = vec![rustls::Certificate(cert_der.clone())];

    let mut server_config = ServerConfig::with_single_cert(cert_chain, priv_key)?;
    Arc::get_mut(&mut server_config.transport)
        .unwrap()
        .keep_alive_interval(Some(Duration::from_secs(DEFAULT_KEEP_ALIVE_INTERVAL_S)));

    Ok((server_config, cert_der))
}

fn start_server(
    mut commands: Commands,
    runtime: Res<Runtime>,
    config: Res<ServerConfigurationData>,
) {
    // TODO Fix: handle unwraps
    let server_adr_str = format!("{}:{}", config.local_bind_host, config.port);
    info!("Starting server on: {} ...", server_adr_str);

    let server_addr: SocketAddr = server_adr_str
        .parse()
        .expect("Failed to parse server address");

    // TODO Security: Server certificate
    let (server_config, server_cert) =
        configure_server(&config.host).expect("Failed to configure server");

    // TODO Clean: Configure size
    let (from_clients_sender, from_clients_receiver) =
        mpsc::channel::<ClientPayload>(DEFAULT_MESSAGE_QUEUE_SIZE);

    let (to_sync_server, from_async_server) =
        mpsc::channel::<InternalAsyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);

    commands.insert_resource(Server {
        clients: HashMap::new(),
        receiver: from_clients_receiver,
        internal_receiver: from_async_server
    });

    // Create server task
    runtime.spawn(async move {
        let mut client_gen_id: ClientId = 0;
        let mut client_id_mappings = HashMap::new();

        let (_endpoint, mut incoming) =
            Endpoint::server(server_config, server_addr).expect("Failed to create server endpoint");

        // Start iterating over incoming connections/clients.
        while let Some(conn) = incoming.next().await {
            let mut new_connection: NewConnection =
                conn.await.expect("Failed to handle incoming connection");
            let connection = new_connection.connection;

            // Attribute an id to this client
            client_gen_id += 1; // TODO Fix: Better id generation/check
            let client_id = client_gen_id;
            client_id_mappings.insert(connection.stable_id(), client_id);
            // TODO Clean: Raise a connection event to the sync side.

            info!(
                "New connection from {}, client_id: {}, stable_id : {}",
                connection.remote_address(),
                client_id,
                connection.stable_id()
            );

            // Create a close channel for this client
            let (close_sender, mut close_receiver): (
                tokio::sync::broadcast::Sender<()>,
                tokio::sync::broadcast::Receiver<()>,
            ) = broadcast::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);

            // Create an ordered reliable send channel for this client
            let (to_client_sender, mut to_client_receiver) =
            mpsc::channel::<Bytes>(DEFAULT_MESSAGE_QUEUE_SIZE);

            // Create a reliable ordered send stream
            let send_stream = connection
                .open_uni()
                .await
                .expect( format!("Failed to open unidirectional send stream for client: {}", client_id).as_str());
            let mut framed_send_stream = FramedWrite::new(send_stream, LengthDelimitedCodec::new());            
            let _network_broadcaster = tokio::spawn(async move {
                tokio::select! {
                    _ = close_receiver.recv() => {
                        trace!("Unidirectional send stream forced to disconnected for client: {}", client_id)
                    }
                    _ = async {
                        while let Some(msg_bytes) = to_client_receiver.recv().await {
                                // TODO Perf: Batch frames for a send_all
                                // TODO Clean: Error handling
                            if let Err(err) = framed_send_stream.send(msg_bytes.clone()).await {
                                error!("Error while sending to client {}: {}", client_id, err);
                            };
                        }
                    } => {}
                }
            });

            // Spawn a task to listen for stream opened from this client
            let from_client_sender_clone = from_clients_sender.clone();
            let mut uni_receivers:JoinSet<()> = JoinSet::new();
            let mut close_receiver = close_sender.subscribe();
            let _client_receiver = tokio::spawn(async move {
                tokio::select! {
                    _ = close_receiver.recv() => {
                        trace!("New Stream listener forced to disconnected for client: {}", client_id)
                    }
                    _ = async {
                        // For each new stream opened by the client
                        while let Some(Ok(recv)) = new_connection.uni_streams.next().await {
                            let mut frame_recv = FramedRead::new(recv, LengthDelimitedCodec::new());

                            // Spawn a task to receive data on this stream.
                            let from_client_sender = from_client_sender_clone.clone();
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
                trace!("All unidirectional stream receivers cleaned for client: {}", client_id)
            });

            to_sync_server
            .send(InternalAsyncMessage::ClientConnected(ClientConnection { client_id: client_id, sender: to_client_sender, close_sender: close_sender }))
            .await
            .expect("Failed to signal connection to sync client");
        }
    });
}

// Receive messages from the async server tasks and update the sync server.
fn update_sync_server(mut server: ResMut<Server>) {
    while let Ok(message) = server.internal_receiver.try_recv() {
        match message {
            // TODO Clean: Raise a connected event
            InternalAsyncMessage::ClientConnected(connection) => {
                server.clients.insert(connection.client_id, connection);
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
        app.insert_resource(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap(),
        )
        .add_startup_system_to_stage(StartupStage::PreStartup, start_server)
        .add_system(update_sync_server);
    }
}
