use super::QuinnetError;
use bevy::prelude::{error, trace};
use bytes::Bytes;
use futures::sink::SinkExt;
use quinn::{SendStream, VarInt};
use std::fmt::Debug;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio_util::codec::{FramedWrite, LengthDelimitedCodec};

pub(crate) type MultiChannelId = u64;

#[derive(Debug, Copy, Clone)]
pub enum ChannelType {
    OrderedReliable,
    UnorderedReliable,
    Unreliable,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum ChannelId {
    OrderedReliable(MultiChannelId),
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
    close_sender: mpsc::Sender<()>,
}

impl Channel {
    pub(crate) fn new(sender: mpsc::Sender<Bytes>, close_sender: mpsc::Sender<()>) -> Self {
        Self {
            sender,
            close_sender,
        }
    }

    pub(crate) fn send_payload<T: Into<Bytes>>(&self, payload: T) -> Result<(), QuinnetError> {
        match self.sender.try_send(payload.into()) {
            Ok(_) => Ok(()),
            Err(err) => match err {
                TrySendError::Full(_) => Err(QuinnetError::FullQueue),
                TrySendError::Closed(_) => Err(QuinnetError::InternalChannelClosed),
            },
        }
    }

    pub(crate) fn close(&self) -> Result<(), QuinnetError> {
        match self.close_sender.blocking_send(()) {
            Ok(_) => Ok(()),
            Err(_) => {
                // The only possible error for a send is that there is no active receivers, meaning that the tasks are already terminated.
                Err(QuinnetError::ChannelAlreadyClosed)
            }
        }
    }
}

pub(crate) async fn ordered_reliable_channel_task<T: Debug>(
    connection: quinn::Connection,
    to_sync_client: mpsc::Sender<T>,
    on_lost_connection: fn() -> T,
    // mut close_receiver: broadcast::Receiver<()>,
    mut close_receiver: mpsc::Receiver<()>,
    mut to_server_receiver: mpsc::Receiver<Bytes>,
) {
    let mut frame_sender = new_uni_frame_sender(&connection).await;

    tokio::select! {
        _ = close_receiver.recv() => {
            trace!("Ordered Reliable Channel task received a close signal")
        }
        _ = async {
            while let Some(msg_bytes) = to_server_receiver.recv().await {
                if let Err(err) = frame_sender.send(msg_bytes).await {
                    error!("Error while sending, {}", err);
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
    // mut close_receiver: broadcast::Receiver<()>,
    mut close_receiver: mpsc::Receiver<()>,
    mut to_server_receiver: mpsc::Receiver<Bytes>,
) {
    tokio::select! {
        _ = close_receiver.recv() => {
            trace!("Unordered Reliable Channel task received a close signal")
        }
        _ = async {
            while let Some(msg_bytes) = to_server_receiver.recv().await {
                let mut frame_sender = new_uni_frame_sender(&connection).await;
                if let Err(err) = frame_sender.send(msg_bytes).await {
                    error!("Error while sending, {}", err);
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
    todo!("Flush and signal finished")
}

async fn new_uni_frame_sender(
    connection: &quinn::Connection,
) -> FramedWrite<SendStream, LengthDelimitedCodec> {
    let uni_sender = connection
        .open_uni()
        .await
        .expect("Failed to open send stream");
    FramedWrite::new(uni_sender, LengthDelimitedCodec::new())
}
