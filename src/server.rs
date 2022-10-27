use std::{
    collections::HashMap,
    error::Error,
    fs::{self, File},
    io::BufReader,
    net::SocketAddr,
    path::Path,
    sync::Arc,
    time::Duration,
};

use bevy::prelude::*;
use bytes::Bytes;
use futures::sink::SinkExt;
use futures_util::StreamExt;
use quinn::{Endpoint, NewConnection, ServerConfig};
use serde::Deserialize;
use tokio::{
    runtime::Runtime,
    sync::{
        broadcast::{self},
        mpsc::{self, error::TryRecvError},
    },
    task::JoinSet,
};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

use crate::{
    ClientId, QuinnetError, DEFAULT_KEEP_ALIVE_INTERVAL_S, DEFAULT_KILL_MESSAGE_QUEUE_SIZE,
    DEFAULT_MESSAGE_QUEUE_SIZE,
};

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

/// How the server should retrieve its certificate.
#[derive(Debug, Clone)]
pub enum CertificateRetrievalMode {
    /// The server will always generate a new self-signed certificate when starting up
    GenerateSelfSigned,
    /// Try to load cert & key from files.
    LoadFromFile { cert_file: String, key_file: String },
    /// Try to load cert & key from files.
    /// If the files do not exist, generate a self-signed certificate, and optionally save it to disk.
    LoadFromFileOrGenerateSelfSigned {
        cert_file: String,
        key_file: String,
        save_on_disk: bool,
    },
}

#[derive(Debug)]
pub(crate) enum InternalAsyncMessage {
    ClientConnected(ClientConnection),
    ClientLostConnection(ClientId),
}

#[derive(Debug, Clone)]
pub(crate) enum InternalSyncMessage {
    StartListening {
        config: ServerConfigurationData,
        cert_mode: CertificateRetrievalMode,
    },
    ClientConnectedAck(ClientId),
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
    pub(crate) internal_sender: broadcast::Sender<InternalSyncMessage>,
}

impl Server {
    /// Run the server with the given [ServerConfigurationData] and [CertificateRetrievalMode]
    pub fn start(
        &self,
        config: ServerConfigurationData,
        cert_mode: CertificateRetrievalMode,
    ) -> Result<(), QuinnetError> {
        match self
            .internal_sender
            .send(InternalSyncMessage::StartListening { config, cert_mode })
        {
            Ok(_) => Ok(()),
            Err(_) => Err(QuinnetError::FullQueue),
        }
    }

    pub fn disconnect_client(&mut self, client_id: ClientId) {
        match self.clients.remove(&client_id) {
            Some(client_connection) => {
                if let Err(_) = client_connection.close_sender.send(()) {
                    error!(
                        "Failed to close client streams & resources while disconnecting client {}",
                        client_id
                    )
                }
            }
            None => error!(
                "Failed to disconnect client {}, client not found",
                client_id
            ),
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

    pub fn send_group_message<'a, I: Iterator<Item = &'a ClientId>, T: serde::Serialize>(
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
            }
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
            client_connection
                .sender
                .try_send(payload.clone().into())
                .unwrap();
        }
    }

    pub fn send_payload<T: Into<Bytes>>(&mut self, client_id: ClientId, payload: T) {
        // TODO Fix: Error handling
        if let Some(client) = self.clients.get(&client_id) {
            client.sender.try_send(payload.into()).unwrap();
        } else {
            warn!("Failed to send payload to unknown client {}", client_id)
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

fn read_certs_from_files(
    cert_file: &String,
    key_file: &String,
) -> Result<(Vec<rustls::Certificate>, rustls::PrivateKey), Box<dyn Error>> {
    let mut cert_chain_reader = BufReader::new(File::open(cert_file)?);
    let certs = rustls_pemfile::certs(&mut cert_chain_reader)?
        .into_iter()
        .map(rustls::Certificate)
        .collect();

    let mut key_reader = BufReader::new(File::open(key_file)?);
    let mut keys = rustls_pemfile::pkcs8_private_keys(&mut key_reader)?;

    assert_eq!(keys.len(), 1);
    let key = rustls::PrivateKey(keys.remove(0));

    Ok((certs, key))
}

fn write_certs_to_files(
    cert: &rcgen::Certificate,
    cert_file: &String,
    key_file: &String,
) -> Result<(), Box<dyn Error>> {
    let pem_cert = cert.serialize_pem()?;
    let pem_key = cert.serialize_private_key_pem();

    fs::write(cert_file, pem_cert)?;
    fs::write(key_file, pem_key)?;

    Ok(())
}

fn generate_self_signed_certificate(
    server_host: &String,
) -> Result<
    (
        Vec<rustls::Certificate>,
        rustls::PrivateKey,
        rcgen::Certificate,
    ),
    Box<dyn Error>,
> {
    let cert = rcgen::generate_simple_self_signed(vec![server_host.into()]).unwrap();
    let cert_der = cert.serialize_der().unwrap();
    let priv_key = rustls::PrivateKey(cert.serialize_private_key_der());
    let cert_chain = vec![rustls::Certificate(cert_der.clone())];

    Ok((cert_chain, priv_key, cert))
}

fn retrieve_certificate(
    server_host: &String,
    cert_mode: CertificateRetrievalMode,
) -> Result<(Vec<rustls::Certificate>, rustls::PrivateKey), Box<dyn Error>> {
    match cert_mode {
        CertificateRetrievalMode::GenerateSelfSigned => {
            trace!("Generating a new self-signed certificate");
            match generate_self_signed_certificate(server_host) {
                Ok((cert_chain, priv_key, _rcgen_cert)) => Ok((cert_chain, priv_key)),
                Err(e) => Err(e),
            }
        }
        CertificateRetrievalMode::LoadFromFile {
            cert_file,
            key_file,
        } => match read_certs_from_files(&cert_file, &key_file) {
            Ok((cert_chain, priv_key)) => {
                trace!("Successfuly loaded cert and key from files");
                Ok((cert_chain, priv_key))
            }
            Err(e) => Err(e),
        },
        CertificateRetrievalMode::LoadFromFileOrGenerateSelfSigned {
            save_on_disk,
            cert_file,
            key_file,
        } => {
            if Path::new(&cert_file).exists() && Path::new(&key_file).exists() {
                match read_certs_from_files(&cert_file, &key_file) {
                    Ok((cert_chain, priv_key)) => {
                        trace!("Successfuly loaded cert and key from files");
                        Ok((cert_chain, priv_key))
                    }
                    Err(e) => Err(e),
                }
            } else {
                warn!("{} and/or {} do not exist, could not load existing certificate. Generating a self-signed one.", cert_file, key_file);
                match generate_self_signed_certificate(server_host) {
                    Ok((cert_chain, priv_key, rcgen_cert)) => {
                        if save_on_disk {
                            write_certs_to_files(&rcgen_cert, &cert_file, &key_file)?;
                            trace!("Successfuly saved cert and key to files");
                        }
                        Ok((cert_chain, priv_key))
                    }
                    Err(e) => Err(e),
                }
            }
        }
    }
}

async fn connections_listening_task(
    config: ServerConfigurationData,
    cert_mode: CertificateRetrievalMode,
    to_sync_server: mpsc::Sender<InternalAsyncMessage>,
    mut from_sync_server: broadcast::Receiver<InternalSyncMessage>,
    from_clients_sender: mpsc::Sender<ClientPayload>,
) {
    // TODO Fix: handle unwraps
    let server_adr_str = format!("{}:{}", config.local_bind_host, config.port);
    info!("Starting server on: {} ...", server_adr_str);

    let server_addr: SocketAddr = server_adr_str
        .parse()
        .expect("Failed to parse server address");

    // Endpoint configuration
    let (cert_chain, priv_key) =
        retrieve_certificate(&config.host, cert_mode).expect("Failed to retrieve certificate");
    let mut server_config = ServerConfig::with_single_cert(cert_chain, priv_key).unwrap();
    Arc::get_mut(&mut server_config.transport)
        .unwrap()
        .keep_alive_interval(Some(Duration::from_secs(DEFAULT_KEEP_ALIVE_INTERVAL_S)));

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

        let send_stream = connection.open_uni().await.expect(
            format!(
                "Failed to open unidirectional send stream for client: {}",
                client_id
            )
            .as_str(),
        );

        let mut framed_send_stream = FramedWrite::new(send_stream, LengthDelimitedCodec::new());

        let to_sync_server_clone = to_sync_server.clone();
        let close_sender_clone = close_sender.clone();
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
                            error!("Client {} seems disconnected, closing resources", client_id);
                            if let Err(_) = close_sender_clone.send(()) {
                                error!("Failed to close all client streams & resources for client {}", client_id)
                            }
                            to_sync_server_clone.send(
                                InternalAsyncMessage::ClientLostConnection(client_id))
                                .await
                                .expect("Failed to signal connection lost to sync server");
                        };
                    }
                } => {}
            }
        });

        // Signal the sync server of this new connection
        to_sync_server
            .send(InternalAsyncMessage::ClientConnected(ClientConnection {
                client_id: client_id,
                sender: to_client_sender,
                close_sender: close_sender.clone(),
            }))
            .await
            .expect("Failed to signal connection to sync client");

        // Wait for the sync server to acknowledge the connection before spawning reception tasks.
        while let Ok(message) = from_sync_server.recv().await {
            match message {
                InternalSyncMessage::ClientConnectedAck(id) => {
                    if id == client_id {
                        break;
                    }
                }
                _ => {}
            }
        }

        // Spawn a task to listen for stream opened from this client
        let from_client_sender_clone = from_clients_sender.clone();
        let mut uni_receivers: JoinSet<()> = JoinSet::new();
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
            trace!(
                "All unidirectional stream receivers cleaned for client: {}",
                client_id
            )
        });
    }
}

fn start_async_server(mut commands: Commands, runtime: Res<Runtime>) {
    // TODO Clean: Configure size
    let (from_clients_sender, from_clients_receiver) =
        mpsc::channel::<ClientPayload>(DEFAULT_MESSAGE_QUEUE_SIZE);

    let (to_sync_server, from_async_server) =
        mpsc::channel::<InternalAsyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);
    let (to_async_server, mut from_sync_server) =
        broadcast::channel::<InternalSyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);

    commands.insert_resource(Server {
        clients: HashMap::new(),
        receiver: from_clients_receiver,
        internal_receiver: from_async_server,
        internal_sender: to_async_server.clone(),
    });

    // Create async server task
    runtime.spawn(async move {
        // Wait for the sync server to to signal us to start listening
        while let Ok(message) = from_sync_server.recv().await {
            match message {
                InternalSyncMessage::StartListening { config, cert_mode } => {
                    connections_listening_task(
                        config,
                        cert_mode,
                        to_sync_server.clone(),
                        to_async_server.subscribe(),
                        from_clients_sender.clone(),
                    )
                    .await;
                }
                _ => {}
            }
        }
    });
}

// Receive messages from the async server tasks and update the sync server.
fn update_sync_server(
    mut server: ResMut<Server>,
    mut connection_events: EventWriter<ConnectionEvent>,
    mut connection_lost_events: EventWriter<ConnectionLostEvent>,
) {
    while let Ok(message) = server.internal_receiver.try_recv() {
        match message {
            InternalAsyncMessage::ClientConnected(connection) => {
                let id = connection.client_id;
                server.clients.insert(id, connection);
                server
                    .internal_sender
                    .send(InternalSyncMessage::ClientConnectedAck(id))
                    .unwrap();
                connection_events.send(ConnectionEvent { id: id });
            }
            InternalAsyncMessage::ClientLostConnection(client_id) => {
                server.clients.remove(&client_id);
                connection_lost_events.send(ConnectionLostEvent { id: client_id });
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
        .add_event::<ConnectionEvent>()
        .add_event::<ConnectionLostEvent>()
        .add_startup_system_to_stage(StartupStage::PreStartup, start_async_server)
        .add_system_to_stage(CoreStage::PreUpdate, update_sync_server);
    }
}
