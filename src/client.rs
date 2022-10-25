use std::{net::SocketAddr, sync::Arc};

use bevy::prelude::*;
use bytes::Bytes;
use futures::sink::SinkExt;
use futures_util::StreamExt;
use quinn::{ClientConfig, Endpoint};
use serde::Deserialize;
use tokio::{
    runtime::Runtime,
    sync::{
        broadcast,
        mpsc::{
            self,
            error::{TryRecvError, TrySendError},
        },
    },
    task::JoinSet,
};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

use crate::{QuinnetError, DEFAULT_KILL_MESSAGE_QUEUE_SIZE, DEFAULT_MESSAGE_QUEUE_SIZE};

pub const DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE: usize = 100;

/// Connection event raised when the client just connected to the server. Raised in the CoreStage::PreUpdate stage.
pub struct ConnectionEvent;

#[derive(Deserialize)]
pub struct ClientConfigurationData {
    server_host: String,
    server_port: u16,
    local_bind_host: String,
    local_bind_port: u16,
}

impl ClientConfigurationData {
    pub fn new(
        server_host: String,
        server_port: u16,
        local_bind_host: String,
        local_bind_port: u16,
    ) -> Self {
        Self {
            server_host,
            server_port,
            local_bind_host,
            local_bind_port,
        }
    }
}

/// Current state of the client driver
#[derive(Debug, PartialEq, Eq)]
enum ClientState {
    Disconnected,
    Connected,
}

#[derive(Debug)]
pub(crate) enum InternalAsyncMessage {
    Connected,
}

#[derive(Debug)]
pub(crate) enum InternalSyncMessage {
    Connect,
}

pub struct Client {
    state: ClientState,
    // TODO Perf: multiple channels
    sender: mpsc::Sender<Bytes>,
    receiver: mpsc::Receiver<Bytes>,
    close_sender: broadcast::Sender<()>,

    pub(crate) internal_receiver: mpsc::Receiver<InternalAsyncMessage>,
    pub(crate) internal_sender: mpsc::Sender<InternalSyncMessage>,
}

impl Client {
    pub fn connect(&self) -> Result<(), QuinnetError> {
        match self.internal_sender.try_send(InternalSyncMessage::Connect) {
            Ok(_) => Ok(()),
            Err(_) => Err(QuinnetError::FullQueue),
        }
    }

    /// Disconnect the client. This does not send any message to the server, and simply closes all the connection tasks locally.
    pub fn disconnect(&mut self) -> Result<(), QuinnetError> {
        if self.is_connected() {
            if let Err(_) = self.close_sender.send(()) {
                return Err(QuinnetError::ChannelClosed);
            }
        }
        self.state = ClientState::Disconnected;
        Ok(())
    }

    pub fn receive_message<T: serde::de::DeserializeOwned>(
        &mut self,
    ) -> Result<Option<T>, QuinnetError> {
        match self.receive_payload()? {
            Some(payload) => match bincode::deserialize(&payload) {
                Ok(msg) => Ok(Some(msg)),
                Err(_) => Err(QuinnetError::Deserialization),
            },
            None => Ok(None),
        }
    }

    pub fn send_message<T: serde::Serialize>(&self, message: T) -> Result<(), QuinnetError> {
        match bincode::serialize(&message) {
            Ok(payload) => self.send_payload(payload),
            Err(_) => Err(QuinnetError::Serialization),
        }
    }

    pub fn send_payload<T: Into<Bytes>>(&self, payload: T) -> Result<(), QuinnetError> {
        match self.sender.try_send(payload.into()) {
            Ok(_) => Ok(()),
            Err(err) => match err {
                TrySendError::Full(_) => Err(QuinnetError::FullQueue),
                TrySendError::Closed(_) => Err(QuinnetError::ChannelClosed),
            },
        }
    }

    pub fn receive_payload(&mut self) -> Result<Option<Bytes>, QuinnetError> {
        match self.receiver.try_recv() {
            Ok(msg_payload) => Ok(Some(msg_payload)),
            Err(err) => match err {
                TryRecvError::Empty => Ok(None),
                TryRecvError::Disconnected => Err(QuinnetError::ChannelClosed),
            },
        }
    }

    pub fn is_connected(&self) -> bool {
        return self.state == ClientState::Connected;
    }
}

// Implementation of `ServerCertVerifier` that verifies everything as trustworthy.
struct SkipServerVerification;

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl rustls::client::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _server_name: &rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: std::time::SystemTime,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::ServerCertVerified::assertion())
    }
}

fn configure_client() -> ClientConfig {
    let crypto = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_custom_certificate_verifier(SkipServerVerification::new())
        .with_no_client_auth();

    ClientConfig::new(Arc::new(crypto))
}

fn initialize_client(
    mut commands: Commands,
    runtime: Res<Runtime>,
    config: Res<ClientConfigurationData>,
) {
    let server_adr_str = format!("{}:{}", config.server_host, config.server_port);
    let srv_host = config.server_host.clone();
    let local_bind_adr = format!("{}:{}", config.local_bind_host, config.local_bind_port);

    info!("Trying to connect to server on: {} ...", server_adr_str);

    let server_addr: SocketAddr = server_adr_str
        .parse()
        .expect("Failed to parse server address");

    let client_cfg = configure_client();

    let (from_server_sender, from_server_receiver) =
        mpsc::channel::<Bytes>(DEFAULT_MESSAGE_QUEUE_SIZE);
    let (to_server_sender, mut to_server_receiver) =
        mpsc::channel::<Bytes>(DEFAULT_MESSAGE_QUEUE_SIZE);

    let (to_sync_client, from_async_client) =
        mpsc::channel::<InternalAsyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);
    let (to_async_client, mut from_sync_client) =
        mpsc::channel::<InternalSyncMessage>(DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE);

    // Create a close channel for this connection
    let (close_sender, mut close_receiver): (
        tokio::sync::broadcast::Sender<()>,
        tokio::sync::broadcast::Receiver<()>,
    ) = broadcast::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);

    let close_sender_clone = close_sender.clone();
    runtime.spawn(async move {
        // Wait for a connection signal before starting client
        if let Some(message) = from_sync_client.recv().await {
            match message {
                InternalSyncMessage::Connect => info!("Client requested to connect"),
            }
        } else {
            warn!("Client closed before requesting a connection");
            return;
        }

        let mut endpoint = Endpoint::client(local_bind_adr.parse().unwrap())
            .expect("Failed to create client endpoint");
        endpoint.set_default_client_config(client_cfg);

        let mut new_connection = endpoint
            .connect(server_addr, &srv_host) // TODO Clean: error handling
            .expect("Failed to connect: configuration error")
            .await
            .expect("Failed to connect");
        info!(
            "Connected to {}",
            new_connection.connection.remote_address()
        );

        to_sync_client
            .send(InternalAsyncMessage::Connected)
            .await
            .expect("Failed to signal connection to sync client");

        let send = new_connection
            .connection
            .open_uni()
            .await
            .expect("Failed to open send stream");
        let mut frame_send = FramedWrite::new(send, LengthDelimitedCodec::new());

        let _network_sends = tokio::spawn(async move {
            tokio::select! {
                _ = close_receiver.recv() => {
                    trace!("Unidirectional send Stream forced to disconnected")
                }
                _ = async {
                    while let Some(msg_bytes) = to_server_receiver.recv().await {
                        if let Err(err) = frame_send.send(msg_bytes).await {
                            error!("Error while sending, {}", err); // TODO Fix: error event
                        }
                    }
                } => {
                    trace!("Unidirectional send Stream ended")
                }
            }
        });

        let mut uni_receivers: JoinSet<()> = JoinSet::new();
        let mut close_receiver = close_sender_clone.subscribe();
        let _network_reads = tokio::spawn(async move {
            tokio::select! {
                _ = close_receiver.recv() => {
                    trace!("New Stream listener forced to disconnected")
                }
                _ = async {
                    while let Some(Ok(recv)) = new_connection.uni_streams.next().await {
                        let mut frame_recv = FramedRead::new(recv, LengthDelimitedCodec::new());
                        let from_server_sender = from_server_sender.clone();

                        uni_receivers.spawn(async move {
                            while let Some(Ok(msg_bytes)) = frame_recv.next().await {
                                from_server_sender.send(msg_bytes.into()).await.unwrap();
                                // TODO Fix: error event
                            }
                        });
                    }
                } => {
                    trace!("New Stream listener ended ")
                }
            }
            uni_receivers.shutdown().await;
            trace!("All unidirectional stream receivers cleaned");
        });
    });

    commands.insert_resource(Client {
        state: ClientState::Disconnected,
        sender: to_server_sender,
        receiver: from_server_receiver,
        close_sender: close_sender,
        internal_receiver: from_async_client,
        internal_sender: to_async_client,
    });
}

// Receive messages from the async client tasks and update the sync client.
fn update_sync_client(
    mut client: ResMut<Client>,
    mut connection_events: EventWriter<ConnectionEvent>,
) {
    while let Ok(message) = client.internal_receiver.try_recv() {
        match message {
            InternalAsyncMessage::Connected => {
                client.state = ClientState::Connected;
                connection_events.send(ConnectionEvent);
            }
        }
    }
}

pub struct QuinnetClientPlugin {}

impl Default for QuinnetClientPlugin {
    fn default() -> Self {
        Self {}
    }
}

impl Plugin for QuinnetClientPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap(),
        )
        .add_event::<ConnectionEvent>()
        // StartupStage::PreStartup so that resources created in commands are available to default startup_systems
        .add_startup_system_to_stage(StartupStage::PreStartup, initialize_client)
        .add_system(update_sync_client);
    }
}
