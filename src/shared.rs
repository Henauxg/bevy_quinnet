use std::mem::size_of;

use bevy::prelude::{Deref, DerefMut, Resource};
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
