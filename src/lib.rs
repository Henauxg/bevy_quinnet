pub const DEFAULT_MESSAGE_QUEUE_SIZE: usize = 150;
pub const DEFAULT_KILL_MESSAGE_QUEUE_SIZE: usize = 10;
pub const DEFAULT_KEEP_ALIVE_INTERVAL_S: u64 = 4;

pub mod client;
pub mod server;

pub type ClientId = u64;

/// Enum with possibles errors that can occur.
#[derive(Debug)]
pub enum QuinnetError {
    /// Failed serialization
    Serialization,
    /// Failed deserialization
    Deserialization,
    /// The data could not be sent on the channel because the channel is
    /// currently full and sending would require blocking.
    FullQueue,
    /// The receive half of the channel was explicitly closed or has been
    /// dropped.
    ChannelClosed,
    InvalidHostFile,
}
