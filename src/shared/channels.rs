use bevy::log::trace;
use bytes::Bytes;
use quinn::VarInt;
use std::fmt::Debug;
use tokio::sync::{
    broadcast,
    mpsc::{self, error::TrySendError},
};

use crate::shared::channels::{
    reliable::send::{ordered_reliable_channel_task, unordered_reliable_channel_task},
    unreliable::send::unreliable_channel_task,
};

use self::{
    reliable::recv::reliable_channels_receiver_task,
    unreliable::recv::unreliable_channel_receiver_task,
};

mod reliable;
mod unreliable;

pub use reliable::DEFAULT_MAX_RELIABLE_FRAME_LEN;

use super::error::{AsyncChannelError, ChannelCloseError, ChannelConfigError};

/// Id of an opened channel
pub type ChannelId = u8;
/// Maximum number of channels that can be opened simultaneously
pub const MAX_CHANNEL_COUNT: usize = u8::MAX as usize + 1;

pub(crate) const CHANNEL_ID_LEN: usize = 1;
pub(crate) const PROTOCOL_HEADER_LEN: usize = CHANNEL_ID_LEN;
pub(crate) type CloseSend = broadcast::Sender<CloseReason>;
pub(crate) type CloseRecv = broadcast::Receiver<CloseReason>;

#[derive(PartialEq, Clone, Copy)]
pub(crate) enum CloseReason {
    LocalOrder,
    PeerClosed,
}

/// Type of a channel, offering different delivery guarantees.
#[derive(Debug, Copy, Clone)]
pub enum ChannelKind {
    /// An OrderedReliable channel ensures that messages sent are delivered, and are processed by the receiving end in the same order as they were sent.
    OrderedReliable {
        /// Maximum size of payloads sent on this channel, in bytes
        max_frame_size: usize,
    },
    /// An UnorderedReliable channel ensures that messages sent are delivered, but they may be delivered out of order.
    UnorderedReliable {
        /// Maximum size of payloads sent on this channel, in bytes
        max_frame_size: usize,
    },
    /// Channel which transmits messages as unreliable and unordered datagrams (may be lost or delivered out of order).
    ///
    /// The maximum allowed size of a datagram may change over the lifetime of a connection according to variation in the path MTU estimate. This is guaranteed to be a little over a kilobyte at minimum.
    Unreliable,
}

impl Default for ChannelKind {
    fn default() -> Self {
        ChannelKind::OrderedReliable {
            max_frame_size: DEFAULT_MAX_RELIABLE_FRAME_LEN,
        }
    }
}

#[derive(Debug)]
/// From async to sync
pub(crate) enum ChannelAsyncMessage {
    LostConnection,
}

#[derive(Debug)]
/// From sync to async
pub(crate) enum ChannelSyncMessage {
    CreateChannel {
        id: ChannelId,
        kind: ChannelKind,
        bytes_to_channel_recv: mpsc::Receiver<Bytes>,
        channel_close_recv: mpsc::Receiver<()>,
    },
}

#[derive(Debug)]
pub(crate) struct Channel {
    id: ChannelId,
    sender: mpsc::Sender<Bytes>,
    close_sender: mpsc::Sender<()>,
}

impl Channel {
    pub(crate) fn new(
        id: ChannelId,
        sender: mpsc::Sender<Bytes>,
        close_sender: mpsc::Sender<()>,
    ) -> Self {
        Self {
            id,
            sender,
            close_sender,
        }
    }

    pub fn id(&self) -> ChannelId {
        self.id
    }

    pub(crate) fn send_payload(&self, payload: Bytes) -> Result<(), AsyncChannelError> {
        match self.sender.try_send(payload) {
            Ok(_) => Ok(()),
            Err(err) => match err {
                TrySendError::Full(_) => Err(AsyncChannelError::FullQueue),
                TrySendError::Closed(_) => Err(AsyncChannelError::InternalChannelClosed),
            },
        }
    }

    pub(crate) fn close(&self) -> Result<(), ChannelCloseError> {
        match self.close_sender.blocking_send(()) {
            Ok(_) => Ok(()),
            Err(_) => {
                // The only possible error for a send is that there is no active receivers, meaning that the tasks are already terminated.
                Err(ChannelCloseError::ChannelAlreadyClosed)
            }
        }
    }
}

/// Stores a configuration that represents multiple channels to be opened by a [`crate::client::connection::ClientSideConnection`] or [`crate::server::Endpoint`]
///
/// Each channel in a [ChannelsConfiguration] is assigned a [ChannelId], starting from 0 and incrementing sequentially by 1.
///
/// ### Example
///
/// Declare 3 configured channels with their respective ids `0`, `1` and `2`:
/// ```
/// use bevy_quinnet::shared::channels::{ChannelKind, ChannelsConfiguration};
///
/// let configs = ChannelsConfiguration::from_types(vec![
///     ChannelKind::OrderedReliable {
///         max_frame_size: 8 * 1_024 * 1_024,
///     },
///     ChannelKind::UnorderedReliable {
///         max_frame_size: 10 * 1_024,
///     },
///     ChannelKind::OrderedReliable {
///         max_frame_size: 10 * 1_024,
///     },
/// ]).unwrap();
/// ```
#[derive(Debug, Clone)]
pub struct ChannelsConfiguration {
    channels: Vec<ChannelKind>,
}

impl Default for ChannelsConfiguration {
    fn default() -> Self {
        Self {
            channels: vec![ChannelKind::OrderedReliable {
                max_frame_size: DEFAULT_MAX_RELIABLE_FRAME_LEN,
            }],
        }
    }
}

impl ChannelsConfiguration {
    /// New empty configuration
    pub fn new() -> Self {
        Self {
            channels: Vec::new(),
        }
    }

    /// New configuration from a simple list of [`ChannelKind`].
    ///
    /// Opened channels (and their [`ChannelId`]) will have the same order as in this collection
    pub fn from_types(
        channel_types: Vec<ChannelKind>,
    ) -> Result<ChannelsConfiguration, ChannelConfigError> {
        if channel_types.len() > MAX_CHANNEL_COUNT {
            Err(ChannelConfigError::MaxChannelsCountReached)
        } else {
            Ok(Self {
                channels: channel_types,
            })
        }
    }

    /// Adds one element to the configuration from a [`ChannelKind`].
    ///
    /// Opened channels (and their [`ChannelId`]) will have the same order as their insertion order.
    pub fn add(&mut self, channel_type: ChannelKind) -> Option<ChannelId> {
        if self.channels.len() < MAX_CHANNEL_COUNT {
            self.channels.push(channel_type);
            Some((self.channels.len() - 1) as u8)
        } else {
            None
        }
    }

    pub(crate) fn configs(&self) -> &Vec<ChannelKind> {
        &self.channels
    }
}

/// Spawn a task to handle send channels creation for this connection
pub(crate) fn spawn_send_channels_tasks_spawner(
    connection_handle: quinn::Connection,
    close_recv: broadcast::Receiver<CloseReason>,
    to_channels_recv: mpsc::Receiver<ChannelSyncMessage>,
    from_channels_send: mpsc::Sender<ChannelAsyncMessage>,
) {
    tokio::spawn(async move {
        send_channels_tasks_spawner(
            connection_handle,
            close_recv,
            to_channels_recv,
            from_channels_send,
        )
        .await
    });
}

struct SendChannelTask {
    connection: quinn::Connection,
    id: ChannelId,
    channels_keepalive: mpsc::Sender<()>,
    from_channels_send: mpsc::Sender<ChannelAsyncMessage>,
    close_recv: CloseRecv,
    channel_close_recv: mpsc::Receiver<()>,
    bytes_recv: mpsc::Receiver<Bytes>,
}

pub(crate) async fn send_channels_tasks_spawner(
    connection: quinn::Connection,
    mut close_recv: broadcast::Receiver<CloseReason>,
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
            while let Some(ChannelSyncMessage::CreateChannel {
                id,
                kind,
                bytes_to_channel_recv: bytes_recv,
                channel_close_recv,
            }) = to_channels_recv.recv().await {

                let channel_task_data = SendChannelTask {
                    connection: connection.clone(),
                    id,
                    channels_keepalive: channel_tasks_keepalive.clone(),
                    from_channels_send: from_channels_send.clone(),
                    close_recv: close_receiver_clone.resubscribe(),
                    channel_close_recv,
                    bytes_recv,
                };

                match kind {
                    ChannelKind::OrderedReliable { max_frame_size } => {
                        tokio::spawn(async move { ordered_reliable_channel_task(channel_task_data, max_frame_size).await });
                    }
                    ChannelKind::UnorderedReliable { max_frame_size } => {
                        tokio::spawn(
                            async move { unordered_reliable_channel_task(channel_task_data, max_frame_size).await },
                        );
                    }
                    ChannelKind::Unreliable => {
                        tokio::spawn(async move { unreliable_channel_task(channel_task_data).await });
                    }
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

pub(crate) fn spawn_recv_channels_tasks(
    connection_handle: quinn::Connection,
    connection_id: u64,
    close_recv: broadcast::Receiver<CloseReason>,
    bytes_incoming_send: mpsc::Sender<(ChannelId, Bytes)>,
) {
    // Spawn a task to listen for reliable messages
    {
        let connection_handle = connection_handle.clone();
        let close_recv = close_recv.resubscribe();
        let bytes_incoming_send = bytes_incoming_send.clone();
        tokio::spawn(async move {
            reliable_channels_receiver_task(
                connection_id,
                connection_handle,
                close_recv,
                bytes_incoming_send,
            )
            .await
        });
    }

    // Spawn a task to listen for unreliable datagrams
    {
        let connection_handle = connection_handle.clone();
        let close_recv = close_recv.resubscribe();
        let bytes_incoming_send = bytes_incoming_send.clone();
        tokio::spawn(async move {
            unreliable_channel_receiver_task(
                connection_id,
                connection_handle,
                close_recv,
                bytes_incoming_send,
            )
            .await
        });
    }
}
