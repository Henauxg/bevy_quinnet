use super::QuinnetError;
use bevy::prelude::{error, trace};
use bytes::Bytes;
use futures::sink::SinkExt;
use quinn::{SendStream, VarInt};
use std::fmt::Debug;
use tokio::sync::{
    broadcast,
    mpsc::{self, error::TrySendError},
};
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
pub(crate) enum ChannelAsyncMessage {
    LostConnection,
}

#[derive(Debug)]
pub(crate) enum ChannelSyncMessage {
    CreateChannel {
        channel_id: ChannelId,
        bytes_to_channel_recv: mpsc::Receiver<Bytes>,
        channel_close_recv: mpsc::Receiver<()>,
    },
}

#[derive(Debug)]
pub(crate) struct Channel {
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

pub(crate) fn get_channel_id_from_type<F>(
    channel_type: ChannelType,
    mut multi_id_generator: F,
) -> ChannelId
where
    F: FnMut() -> MultiChannelId,
{
    match channel_type {
        ChannelType::OrderedReliable => ChannelId::OrderedReliable(multi_id_generator()),
        ChannelType::UnorderedReliable => ChannelId::UnorderedReliable,
        ChannelType::Unreliable => ChannelId::Unreliable,
    }
}

pub(crate) async fn channels_task(
    connection: quinn::Connection,
    mut close_recv: broadcast::Receiver<()>,
    mut to_channels_recv: mpsc::Receiver<ChannelSyncMessage>,
    from_channels_send: mpsc::Sender<ChannelAsyncMessage>,
) {
    // Use an mpsc channel where, instead of sending messages, we wait for the channel to be closed, which happens when every sender has been dropped. We can't use a JoinSet as simply here since we would also need to drain closed channels from it.
    let (channel_tasks_keepalive, mut channel_tasks_waiter) = mpsc::channel::<()>(1);

    let close_receiver_clone = close_recv.resubscribe();
    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Connection Channels listener received a close signal")
        }
        _ = async {
            while let Some(sync_message) = to_channels_recv.recv().await {
                let ChannelSyncMessage::CreateChannel{ channel_id,  bytes_to_channel_recv, channel_close_recv } = sync_message;

                let close_receiver = close_receiver_clone.resubscribe();
                let connection_handle = connection.clone();
                let from_channels_send = from_channels_send.clone();
                let channels_keepalive_clone = channel_tasks_keepalive.clone();

                match channel_id {
                    ChannelId::OrderedReliable(_) => {
                        tokio::spawn(async move {
                            ordered_reliable_channel_task(
                                connection_handle,
                                channels_keepalive_clone,
                                from_channels_send,
                                close_receiver,
                                channel_close_recv,
                                bytes_to_channel_recv
                            )
                            .await
                        });
                    },
                    ChannelId::UnorderedReliable => {
                        tokio::spawn(async move {
                            unordered_reliable_channel_task(
                                connection_handle,
                                channels_keepalive_clone,
                                from_channels_send,
                                close_receiver,
                                channel_close_recv,
                                bytes_to_channel_recv
                            )
                            .await
                        });
                    },
                    ChannelId::Unreliable => {
                        tokio::spawn(async move {
                            unreliable_channel_task(
                                connection_handle,
                                channels_keepalive_clone,
                                from_channels_send,
                                close_receiver,
                                channel_close_recv,
                                bytes_to_channel_recv
                            )
                            .await
                        });
                    },
                }
            }
        } => {
            trace!("Connection Channels listener ended")
        }
    };

    // Wait for all the channels to have flushed/finished:
    // We drop our sender first because the recv() call otherwise sleeps forever.
    // When every sender has gone out of scope, the recv call will return with an error. We ignore the error.
    drop(channel_tasks_keepalive);
    let _ = channel_tasks_waiter.recv().await;

    connection.close(VarInt::from_u32(0), "closed".as_bytes());
}

pub(crate) async fn ordered_reliable_channel_task(
    connection: quinn::Connection,
    _: mpsc::Sender<()>,
    from_channels_send: mpsc::Sender<ChannelAsyncMessage>,
    mut close_recv: broadcast::Receiver<()>,
    mut channel_close_recv: mpsc::Receiver<()>,
    mut bytes_to_channel_recv: mpsc::Receiver<Bytes>,
) {
    let mut frame_sender = new_uni_frame_sender(&connection).await;

    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Ordered Reliable Channel task received a close signal")
        }
        _ = channel_close_recv.recv() => {
            trace!("Ordered Reliable Channel task received a channel close signal")
        }
        _ = async {
            while let Some(msg_bytes) = bytes_to_channel_recv.recv().await {
                if let Err(err) = frame_sender.send(msg_bytes).await {
                    error!("Error while sending, {}", err);
                    from_channels_send.send(
                        ChannelAsyncMessage::LostConnection)
                        .await
                        .expect("Failed to signal connection lost to sync client");
                }
            }
        } => {
            trace!("Ordered Reliable Channel task ended")
        }
    };
    while let Ok(msg_bytes) = bytes_to_channel_recv.try_recv() {
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
}

pub(crate) async fn unordered_reliable_channel_task(
    connection: quinn::Connection,
    channel_tasks_keepalive: mpsc::Sender<()>,
    from_channels_send: mpsc::Sender<ChannelAsyncMessage>,
    mut close_recv: broadcast::Receiver<()>,
    mut channel_close_recv: mpsc::Receiver<()>,
    mut bytes_to_channel_recv: mpsc::Receiver<Bytes>,
) {
    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Ordered Reliable Channel task received a close signal")
        }
        _ = channel_close_recv.recv() => {
            trace!("Unordered Reliable Channel task received a channel close signal")
        }
        _ = async {
            while let Some(msg_bytes) = bytes_to_channel_recv.recv().await {
                let conn = connection.clone();
                let to_sync_client_clone = from_channels_send.clone();
                let channels_keepalive_clone = channel_tasks_keepalive.clone();
                tokio::spawn(async move {
                    let mut frame_sender = new_uni_frame_sender(&conn).await;
                    if let Err(err) = frame_sender.send(msg_bytes).await {
                        error!("Error while sending, {}", err);
                        to_sync_client_clone.send(
                            ChannelAsyncMessage::LostConnection)
                            .await
                            .expect("Failed to signal connection lost to sync client");
                    }
                    if let Err(err) = frame_sender.into_inner().finish().await {
                        error!("Failed to shutdown stream gracefully: {}", err);
                    }
                    drop(channels_keepalive_clone)
                });
            }
        } => {
            trace!("Unordered Reliable Channel task ended")
        }
    };
    while let Ok(msg_bytes) = bytes_to_channel_recv.try_recv() {
        let conn = connection.clone();
        let channels_keepalive_clone = channel_tasks_keepalive.clone();
        tokio::spawn(async move {
            let mut frame_sender = new_uni_frame_sender(&conn).await;
            if let Err(err) = frame_sender.send(msg_bytes).await {
                error!("Error while sending, {}", err);
            }
            if let Err(err) = frame_sender.into_inner().finish().await {
                error!("Failed to shutdown stream gracefully: {}", err);
            }
            drop(channels_keepalive_clone)
        });
    }
}

pub(crate) async fn unreliable_channel_task(
    connection: quinn::Connection,
    _: mpsc::Sender<()>,
    from_channels_send: mpsc::Sender<ChannelAsyncMessage>,
    mut close_recv: broadcast::Receiver<()>,
    mut channel_close_recv: mpsc::Receiver<()>,
    mut bytes_to_channel_recv: mpsc::Receiver<Bytes>,
) {
    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Unreliable Channel task received a close signal")
        }
        _ = channel_close_recv.recv() => {
            trace!("Unreliable Channel task received a channel close signal")
        }
        _ = async {
            while let Some(msg_bytes) = bytes_to_channel_recv.recv().await {
                // TODO Test datagram_send_buffer_space + adapt to max_datagram_size
                if let Err(err) = connection.send_datagram(msg_bytes) {
                    error!("Error while sending, {}", err);
                    from_channels_send.send(
                        ChannelAsyncMessage::LostConnection)
                        .await
                        .expect("Failed to signal connection lost to sync client");
                }
            }
        } => {
            trace!("Unreliable Channel task ended")
        }
    };
    while let Ok(msg_bytes) = bytes_to_channel_recv.try_recv() {
        if let Err(err) = connection.send_datagram(msg_bytes) {
            error!("Error while sending, {}", err);
        }
    }
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
