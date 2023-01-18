use super::QuinnetError;
use bevy::prelude::{error, trace};
use bytes::Bytes;
use futures::{sink::SinkExt, StreamExt};
use quinn::{RecvStream, SendDatagramError, SendStream, VarInt};
use std::fmt::{Debug, Display};
use tokio::sync::{
    broadcast,
    mpsc::{self, error::TrySendError},
};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

pub(crate) type MultiChannelId = u64;

#[derive(Debug, Copy, Clone)]
pub enum ChannelType {
    /// An OrderedReliable channel ensures that messages sent are delivered, and are processed by the receiving end in the same order as they were sent.
    OrderedReliable,
    /// An UnorderedReliable channel ensures that messages sent are delivered, but they may be delivered out of order.
    UnorderedReliable,
    /// Channel which transmits messages as unreliable and unordered datagrams (may be lost or delivered out of order).
    ///
    /// The maximum allowed size of a datagram may change over the lifetime of a connection according to variation in the path MTU estimate. This is guaranteed to be a little over a kilobyte at minimum.
    Unreliable,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum ChannelId {
    /// There may be more than one OrderedReliable channel instance. This may be useful to avoid some Head of line blocking (<https://en.wikipedia.org/wiki/Head-of-line_blocking>) issues.
    ///
    /// See [ChannelType::OrderedReliable] for more information.
    OrderedReliable(MultiChannelId),
    /// One `UnorderedReliable` channel instance is enough since messages are not ordered on those, in fact even if you tried to create more, Quinnet would just reuse the existing one. This is why you can directly use this [ChannelId::UnorderedReliable] when sending messages.
    ///
    /// See [ChannelType::UnorderedReliable] for more information.
    UnorderedReliable,
    /// One `Unreliable` channel instance is enough since messages are not ordered on those, in fact even if you tried to create more, Quinnet would just reuse the existing one. This is why you can directly use this [ChannelId::Unreliable] when sending messages.
    ///
    /// See [ChannelType::Unreliable] for more information.
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
                    error!("Error while sending on Ordered Reliable Channel, {}", err);
                    from_channels_send.send(
                        ChannelAsyncMessage::LostConnection)
                        .await
                        .expect("Failed to signal connection lost on Ordered Reliable Channel");
                }
            }
        } => {
            trace!("Ordered Reliable Channel task ended")
        }
    };
    while let Ok(msg_bytes) = bytes_to_channel_recv.try_recv() {
        if let Err(err) = frame_sender.send(msg_bytes).await {
            error!(
                "Error while sending a remaining message on Ordered Reliable Channel, {}",
                err
            );
        }
    }
    if let Err(err) = frame_sender.flush().await {
        error!(
            "Error while flushing Ordered Reliable Channel stream: {}",
            err
        );
    }
    if let Err(err) = frame_sender.into_inner().finish().await {
        error!(
            "Failed to shutdown Ordered Reliable Channel stream gracefully: {}",
            err
        );
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
            trace!("Unordered Reliable Channel task received a close signal")
        }
        _ = channel_close_recv.recv() => {
            trace!("Unordered Reliable Channel task received a channel close signal")
        }
        _ = async {
            while let Some(msg_bytes) = bytes_to_channel_recv.recv().await {
                let conn = connection.clone();
                let from_channels_send_clone = from_channels_send.clone();
                let channels_keepalive_clone = channel_tasks_keepalive.clone();
                tokio::spawn(async move {
                    let mut frame_sender = new_uni_frame_sender(&conn).await;
                    if let Err(err) = frame_sender.send(msg_bytes).await {
                        error!("Error while sending on Unordered Reliable Channel, {}", err);
                        from_channels_send_clone.send(
                            ChannelAsyncMessage::LostConnection)
                            .await
                            .expect("Failed to signal connection lost on Unordered Reliable Channel");
                    }
                    if let Err(err) = frame_sender.into_inner().finish().await {
                        error!("Failed to shutdown Unordered Reliable Channel stream gracefully: {}", err);
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
                error!(
                    "Error while sending a remaining message on Unordered Reliable Channel, {}",
                    err
                );
            }
            if let Err(err) = frame_sender.into_inner().finish().await {
                error!(
                    "Failed to shutdown Unordered Reliable Channel stream gracefully: {}",
                    err
                );
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
                if let Err(err) = connection.send_datagram(msg_bytes) {
                    error!("Error while sending message on Unreliable Channel, {}", err);
                    match err {
                        SendDatagramError::UnsupportedByPeer => (),
                        SendDatagramError::Disabled => (),
                        SendDatagramError::TooLarge => (),
                        SendDatagramError::ConnectionLost(_) => {
                            from_channels_send.send(
                                ChannelAsyncMessage::LostConnection)
                                .await
                                .expect("Failed to signal connection lost from channels");
                        },
                    }

                }
            }
        } => {
            trace!("Unreliable Channel task ended")
        }
    };
    while let Ok(msg_bytes) = bytes_to_channel_recv.try_recv() {
        if let Err(err) = connection.send_datagram(msg_bytes) {
            error!(
                "Error while sending a remaining message on Unreliable Channel, {}",
                err
            );
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

async fn uni_receiver_task(
    recv: RecvStream,
    mut close_recv: broadcast::Receiver<()>,
    bytes_from_server_send: mpsc::Sender<Bytes>,
) {
    tokio::select! {
        _ = close_recv.recv() => {}
        _ = async {
            let mut frame_recv = FramedRead::new(recv, LengthDelimitedCodec::new());
            while let Some(Ok(msg_bytes)) = frame_recv.next().await {
                // TODO Clean: error handling
                bytes_from_server_send.send(msg_bytes.into()).await.unwrap();
            }
        } => {}
    };
}

pub(crate) async fn reliable_receiver_task<T: Display>(
    id: T,
    connection: quinn::Connection,
    mut close_recv: broadcast::Receiver<()>,
    bytes_incoming_send: mpsc::Sender<Bytes>,
) {
    let close_recv_clone = close_recv.resubscribe();
    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Listener for new Unidirectional Receiving Streams with id {} received a close signal", id)
        }
        _ = async {
            while let Ok(recv) = connection.accept_uni().await {
                let bytes_from_server_send = bytes_incoming_send.clone();
                let close_recv_clone = close_recv_clone.resubscribe();
                tokio::spawn(async move {
                    uni_receiver_task(
                        recv,
                        close_recv_clone,
                        bytes_from_server_send
                    ).await;
                });
            }
        } => {
            trace!("Listener for new Unidirectional Receiving Streams with id {} ended", id)
        }
    };
}

pub(crate) async fn unreliable_receiver_task<T: Display>(
    id: T,
    connection: quinn::Connection,
    mut close_recv: broadcast::Receiver<()>,
    bytes_incoming_send: mpsc::Sender<Bytes>,
) {
    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Listener for unreliable datagrams with id {} received a close signal", id)
        }
        _ = async {
            while let Ok(msg_bytes) = connection.read_datagram().await {
                // TODO Clean: error handling
                bytes_incoming_send.send(msg_bytes.into()).await.unwrap();
            }
        } => {
            trace!("Listener for unreliable datagrams with id {} ended", id)
        }
    };
}
