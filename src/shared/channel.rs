use bevy::prelude::{error, trace};
use bytes::Bytes;
use futures::sink::SinkExt;
use futures_util::StreamExt;
use tokio::sync::{
    broadcast,
    mpsc::{self, error::TrySendError},
};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

use super::QuinnetError;

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
                TrySendError::Closed(_) => Err(QuinnetError::ChannelClosed),
            },
        }
    }

    pub(crate) fn new(sender: mpsc::Sender<Bytes>) -> Self {
        Self { sender }
    }
}

// async fn ordered_reliable_channel_task(
//     connection: quinn::Connection,
//     to_sync_client: mpsc::Sender<InternalAsyncMessage>,
//     mut close_receiver: broadcast::Receiver<()>,
//     mut to_server_receiver: mpsc::Receiver<Bytes>,
// ) {
//     tokio::select! {
//         _ = close_receiver.recv() => {
//             trace!("Ordered Reliable Send Channel received a close signal")
//         }
//         _ = async {
//             let uni_sender = connection
//                 .open_uni()
//                 .await
//                 .expect("Failed to open send stream");
//             let mut frame_sender = FramedWrite::new(uni_sender, LengthDelimitedCodec::new());

//             while let Some(msg_bytes) = to_server_receiver.recv().await {
//                 if let Err(err) = frame_sender.send(msg_bytes).await {
//                     // TODO Clean: error handling
//                     error!("Error while sending, {}", err);
//                     // error!("Client seems disconnected, closing resources");
//                     // if let Err(_) = close_sender_clone.send(()) {
//                     //     error!("Failed to close all client streams & resources")
//                     // }
//                     to_sync_client.send(
//                         InternalAsyncMessage::LostConnection)
//                         .await
//                         .expect("Failed to signal connection lost to sync client");
//                 }
//             }
//         } => {
//             trace!("Ordered Reliable Send Channel ended")
//         }
//     }
// }

// async fn unordered_reliable_channel_task(
//     connection: quinn::Connection,
//     to_sync_client: mpsc::Sender<InternalAsyncMessage>,
//     mut close_receiver: broadcast::Receiver<()>,
//     mut to_server_receiver: mpsc::Receiver<Bytes>,
// ) {
//     tokio::select! {
//         _ = close_receiver.recv() => {
//             trace!("Unordered Reliable Send Channel received a close signal")
//         }
//         _ = async {
//             while let Some(msg_bytes) = to_server_receiver.recv().await {
//                 let uni_sender = connection
//                     .open_uni()
//                     .await
//                     .expect("Failed to open send stream");
//                 let mut frame_sender = FramedWrite::new(uni_sender, LengthDelimitedCodec::new());

//                 if let Err(err) = frame_sender.send(msg_bytes).await {
//                     // TODO Clean: error handling
//                     error!("Error while sending, {}", err);
//                     // error!("Client seems disconnected, closing resources");
//                     // if let Err(_) = close_sender_clone.send(()) {
//                     //     error!("Failed to close all client streams & resources")
//                     // }
//                     to_sync_client.send(
//                         InternalAsyncMessage::LostConnection)
//                         .await
//                         .expect("Failed to signal connection lost to sync client");
//                 }
//             }
//         } => {
//             trace!("Unordered Reliable Send Channel ended")
//         }
//     }
// }
