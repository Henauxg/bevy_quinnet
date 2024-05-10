use std::mem::size_of;

use bevy::{
    ecs::schedule::SystemSet,
    prelude::{Deref, DerefMut, Resource},
};
use tokio::runtime::Runtime;

pub mod certificate;
pub mod channels;
pub mod error;

pub const DEFAULT_MESSAGE_QUEUE_SIZE: usize = 150;
pub const DEFAULT_KILL_MESSAGE_QUEUE_SIZE: usize = 10;
pub const DEFAULT_KEEP_ALIVE_INTERVAL_S: u64 = 4;

pub type ClientId = u64;
pub const CLIENT_ID_LEN: usize = size_of::<ClientId>();

#[derive(Resource, Deref, DerefMut)]
pub struct AsyncRuntime(pub(crate) Runtime);
pub(crate) type InternalConnectionRef = quinn::Connection;

/// System set used to update the sync client & server from updates coming from the async quinnet back-end.
///
/// This is where client & sderver events are raised.
///
/// This system set runs in PreUpdate.
#[derive(Debug, SystemSet, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QuinnetSyncUpdate;

// May add a `QuinnetFlush` SystemSet to buffer and flush messages.
