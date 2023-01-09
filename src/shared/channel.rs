use super::QuinnetError;
use bevy::prelude::{error, trace};
use bytes::Bytes;
use futures::sink::SinkExt;
use quinn::VarInt;
use std::fmt::Debug;
use tokio::sync::{
    broadcast,
    mpsc::{self, error::TrySendError},
};
use tokio_util::codec::{FramedWrite, LengthDelimitedCodec};

pub(crate) type OrdRelChannelId = u64;

#[derive(Debug, Copy, Clone)]
pub enum ChannelType {
    OrderedReliable,
    UnorderedReliable,
    Unreliable,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum ChannelId {
    OrderedReliable(OrdRelChannelId),
    UnorderedReliable,
    Unreliable,
}

impl std::fmt::Display for ChannelId {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug)]
pub struct Channel {
    sender: mpsc::Sender<Bytes>,
}

impl Channel {
    pub fn send_payload<T: Into<Bytes>>(&self, payload: T) -> Result<(), QuinnetError> {
        match self.sender.try_send(payload.into()) {
            Ok(_) => Ok(()),
            Err(err) => match err {
                TrySendError::Full(_) => Err(QuinnetError::FullQueue),
                TrySendError::Closed(_) => Err(QuinnetError::InternalChannelClosed),
            },
        }
    }

    pub(crate) fn new(sender: mpsc::Sender<Bytes>) -> Self {
        Self { sender }
    }
}

pub(crate) async fn ordered_reliable_channel_task<T: Debug>(
    connection: quinn::Connection,
    to_sync_client: mpsc::Sender<T>,
    on_lost_connection: fn() -> T,
    mut close_receiver: broadcast::Receiver<()>,
    mut to_server_receiver: mpsc::Receiver<Bytes>,
) {
    let uni_sender = connection
        .open_uni()
        .await
        .expect("Failed to open send stream");
    let mut frame_sender = FramedWrite::new(uni_sender, LengthDelimitedCodec::new());

    tokio::select! {
        _ = close_receiver.recv() => {
            trace!("Ordered Reliable Channel task received a close signal")
        }
        _ = async {
            while let Some(msg_bytes) = to_server_receiver.recv().await {
                if let Err(err) = frame_sender.send(msg_bytes).await {
                    // TODO Clean: error handling
                    error!("Error while sending, {}", err);
                    // error!("Client seems disconnected, closing resources");
                    // if let Err(_) = close_sender_clone.send(()) {
                    //     error!("Failed to close all client streams & resources")
                    // }
                    to_sync_client.send(
                        on_lost_connection())
                        .await
                        .expect("Failed to signal connection lost to sync client");
                }
            }
        } => {
            trace!("Ordered Reliable Channel task ended")
        }
    }
    while let Ok(msg_bytes) = to_server_receiver.try_recv() {
        if let Err(err) = frame_sender.send(msg_bytes).await {
            error!("Error while sending, {}", err);
        }
    }
    if let Err(err) = frame_sender.flush().await {
        error!("Error while flushing stream: {}", err);
    }
    if let Err(err) = frame_sender.into_inner().finish().await {
        error!("Failed to shutdown stream gracefully: {}", err);
    }
    todo!("Do not close here, wait for all channels to be flushed");
    connection.close(VarInt::from_u32(0), "closed".as_bytes());
}

pub(crate) async fn unordered_reliable_channel_task<T: Debug>(
    connection: quinn::Connection,
    to_sync_client: mpsc::Sender<T>,
    on_lost_connection: fn() -> T,
    mut close_receiver: broadcast::Receiver<()>,
    mut to_server_receiver: mpsc::Receiver<Bytes>,
) {
    tokio::select! {
        _ = close_receiver.recv() => {
            trace!("Unordered Reliable Channel task received a close signal")
        }
        _ = async {
            while let Some(msg_bytes) = to_server_receiver.recv().await {
                let uni_sender = connection
                    .open_uni()
                    .await
                    .expect("Failed to open send stream");
                let mut frame_sender = FramedWrite::new(uni_sender, LengthDelimitedCodec::new());

                if let Err(err) = frame_sender.send(msg_bytes).await {
                    // TODO Clean: error handling
                    error!("Error while sending, {}", err);
                    // error!("Client seems disconnected, closing resources");
                    // if let Err(_) = close_sender_clone.send(()) {
                    //     error!("Failed to close all client streams & resources")
                    // }
                    to_sync_client.send(
                        on_lost_connection())
                        .await
                        .expect("Failed to signal connection lost to sync client");
                }
            }
        } => {
            trace!("Unordered Reliable Channel task ended")
        }
    }
}

// async fn send_msg(
//     // close_sender: &tokio::sync::broadcast::Sender<()>,
//     to_sync_client: &mpsc::Sender<InternalAsyncMessage>,
//     frame_send: &mut FramedWrite<SendStream, LengthDelimitedCodec>,
//     msg_bytes: Bytes,
// ) {
//     if let Err(err) = frame_send.send(msg_bytes).await {
//         error!("Error while sending, {}", err);
//         error!("Client seems disconnected, closing resources");
//         // Emit LostConnection to properly update the connection about its state.
//         // Raise LostConnection event before emitting a close signal because we have no guarantee to continue this async execution after the close signal has been processed.
//         to_sync_client
//             .send(InternalAsyncMessage::LostConnection)
//             .await
//             .expect("Failed to signal connection lost to sync client");
//         // if let Err(_) = close_sender.send(()) {
//         //     error!("Failed to close all client streams & resources")
//         // }
//     }
// }
