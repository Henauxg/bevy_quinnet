pub const DEFAULT_MESSAGE_QUEUE_SIZE: usize = 150;
pub const DEFAULT_KILL_MESSAGE_QUEUE_SIZE: usize = 10;
pub const DEFAULT_KEEP_ALIVE_INTERVAL_S: u64 = 4;

pub mod client;
pub mod server;

pub type ClientId = u64;

/// Enum with possibles errors that can occur in Bevy Quinnet
#[derive(thiserror::Error, Debug)]
pub enum QuinnetError {
    #[error("Client with id `{0}` is unknown")]
    UnknownClient(ClientId),
    #[error("Failed serialization")]
    Serialization,
    #[error("Failed deserialization")]
    Deserialization,
    #[error("The data could not be sent on the channel because the channel is currently full and sending would require blocking")]
    FullQueue,
    #[error("The receiving half of the channel was explicitly closed or has been dropped")]
    ChannelClosed,
    #[error("The hosts file is invalid")]
    InvalidHostFile,
}
