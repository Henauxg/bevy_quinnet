use std::{mem::size_of, time::Duration};

use bevy::{
    ecs::schedule::SystemSet,
    prelude::{Deref, DerefMut, Resource},
};
use channels::MAX_CHANNEL_COUNT;
use tokio::runtime::Runtime;

/// Certificate features shared by client & server
pub mod certificate;
/// Channel features shared by client & server
pub mod channels;
/// Shared error types
pub mod error;

/// Default max size of async channels used to hold network messages. 1 async channel per connection.
pub const DEFAULT_MESSAGE_QUEUE_SIZE: usize = 150;
/// Default period of inactivity before sending a keep-alive packet
///
/// Keep-alive packets prevent an inactive but otherwise healthy connection from timing out.
pub const DEFAULT_KEEP_ALIVE_INTERVAL_S: Duration = Duration::from_secs(4);

/// Default max size for quinnet internal message channels
pub const DEFAULT_INTERNAL_MESSAGES_CHANNEL_SIZE: usize = 100;

/// Default max size for Quinnet Channels messages
///
/// At least MAX_CHANNEL_COUNT capacity if all available channel slots are requested to open
pub const DEFAULT_QCHANNEL_MESSAGES_CHANNEL_SIZE: usize = 2 * MAX_CHANNEL_COUNT;

/// Default max size of the queues used to transmit close messages for async tasks
pub(crate) const DEFAULT_KILL_MESSAGE_QUEUE_SIZE: usize = 10;

/// Represents the id of a client on the server.
pub type ClientId = u64;
pub(crate) const CLIENT_ID_LEN: usize = size_of::<ClientId>();

/// Async runtime newtype wrapping the tokio runtime handle. used by both quinnet client and server's async back-ends.
#[derive(Resource, Deref, DerefMut)]
pub struct AsyncRuntime(pub(crate) Runtime);
pub(crate) type InternalConnectionRef = quinn::Connection;

/// System set used to update the sync client & server from updates coming from the async quinnet back-end.
///
/// This is where client & server events are raised.
///
/// This system set runs in PreUpdate.
#[derive(Debug, SystemSet, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QuinnetSyncUpdate;

// May add a `QuinnetFlush` SystemSet to buffer and flush messages.
