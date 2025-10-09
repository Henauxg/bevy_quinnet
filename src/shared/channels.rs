use bytes::Bytes;
use std::fmt::Debug;
use tokio::sync::{
    broadcast,
    mpsc::{self, error::TrySendError},
};

mod reliable;
pub(crate) mod tasks;
mod unreliable;

pub use reliable::DEFAULT_MAX_RELIABLE_FRAME_LEN;

use super::error::{AsyncChannelError, ChannelCloseError, ChannelConfigError};

/// Id of an opened channel
pub type ChannelId = u8;
/// Maximum number of channels that can be opened simultaneously
pub const MAX_CHANNEL_COUNT: usize = u8::MAX as usize + 1;

pub(crate) const CHANNEL_ID_LEN: usize = size_of::<ChannelId>();
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
pub enum ChannelConfig {
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

impl Default for ChannelConfig {
    fn default() -> Self {
        ChannelConfig::default_ordered_reliable()
    }
}
impl ChannelConfig {
    /// Default channel configuration for an Unreliable channel.
    pub const fn default_unreliable() -> Self {
        ChannelConfig::Unreliable
    }
    /// Default channel configuration for an UnorderedReliable channel.
    pub const fn default_unordered_reliable() -> Self {
        ChannelConfig::UnorderedReliable {
            max_frame_size: DEFAULT_MAX_RELIABLE_FRAME_LEN,
        }
    }
    /// Default channel configuration for an OrderedReliable channel.
    pub const fn default_ordered_reliable() -> Self {
        ChannelConfig::OrderedReliable {
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
        config: ChannelConfig,
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

/// Stores a configuration that represents multiple channels to be opened by a [`crate::client::connection::ClientSideConnection`] or [`crate::server::endpoint::Endpoint`].
///
/// Each channel in a [SendChannelsConfiguration] is assigned a [ChannelId], starting from 0 and incrementing sequentially by 1.
///
/// ### Example
///
/// Declare 3 configured channels with their respective ids `0`, `1` and `2`:
/// ```
/// use bevy_quinnet::shared::channels::{ChannelConfig, SendChannelsConfiguration};
///
/// let configs = SendChannelsConfiguration::from_configs(vec![
///     ChannelConfig::OrderedReliable {
///         max_frame_size: 8 * 1_024 * 1_024,
///     },
///     ChannelConfig::UnorderedReliable {
///         max_frame_size: 10 * 1_024,
///     },
///     ChannelConfig::OrderedReliable {
///         max_frame_size: 10 * 1_024,
///     },
/// ]).unwrap();
/// ```
#[derive(Debug, Clone)]
pub struct SendChannelsConfiguration {
    channels: Vec<ChannelConfig>,
}

impl Default for SendChannelsConfiguration {
    fn default() -> Self {
        Self {
            channels: vec![ChannelConfig::OrderedReliable {
                max_frame_size: DEFAULT_MAX_RELIABLE_FRAME_LEN,
            }],
        }
    }
}

impl SendChannelsConfiguration {
    /// New empty configuration
    pub fn new() -> Self {
        Self {
            channels: Vec::new(),
        }
    }

    /// New configuration from a simple list of [`ChannelConfig`].
    ///
    /// Opened channels (and their [`ChannelId`]) will have the same order as in this collection
    pub fn from_configs(
        channel_types: Vec<ChannelConfig>,
    ) -> Result<SendChannelsConfiguration, ChannelConfigError> {
        if channel_types.len() > MAX_CHANNEL_COUNT {
            Err(ChannelConfigError::MaxChannelsCountReached)
        } else {
            Ok(Self {
                channels: channel_types,
            })
        }
    }

    /// Adds one element to the configuration from a [`ChannelConfig`].
    ///
    /// Opened channels (and their [`ChannelId`]) will have the same order as their insertion order.
    pub fn add(&mut self, channel_type: ChannelConfig) -> Option<ChannelId> {
        if self.channels.len() < MAX_CHANNEL_COUNT {
            self.channels.push(channel_type);
            Some((self.channels.len() - 1) as u8)
        } else {
            None
        }
    }

    pub(crate) fn configs(&self) -> &Vec<ChannelConfig> {
        &self.channels
    }
}
