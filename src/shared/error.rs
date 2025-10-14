use super::channels::ChannelId;

/// Quinnet internal error in async<->sync communications
#[derive(thiserror::Error, Debug)]
pub enum AsyncChannelError {
    /// The data could not be sent on the channel because the channel is currently full and sending would require blocking
    #[error("The data could not be sent on the channel because the channel is currently full and sending would require blocking")]
    FullQueue,
    /// The receiving half of an internal channel was explicitly closed or has been dropped
    #[error(
        "The receiving half of the internal channel was explicitly closed or has been dropped"
    )]
    InternalChannelClosed,
}

/// Error while closing a channel
#[derive(thiserror::Error, Debug)]
pub enum ChannelCloseError {
    /// A channel is closed
    #[error("Channel is closed already")]
    ChannelAlreadyClosed,
    /// A channel id is invalid
    #[error("Channel with id `{0}` is invalid")]
    InvalidChannelId(ChannelId),
}

/// Errro while creating a channel
#[derive(thiserror::Error, Debug)]
pub enum ChannelCreationError {
    /// The maximum number of simultaneously opened channels has been reached
    #[error("The maximum number of simultaneously opened channels has been reached")]
    MaxChannelsCountReached,
    /// Quinnet async channel error
    #[error("Quinnet async channel error")]
    AsyncChannelError(#[from] AsyncChannelError),
}

/// Error while configuring channels
#[derive(thiserror::Error, Debug)]
pub enum ChannelConfigError {
    /// The maximum number of configured channels has been reached
    #[error("The maximum number of configured channels has been reached")]
    MaxChannelsCountReached,
}

/// Error when sending data from the server
#[derive(thiserror::Error, Debug)]
pub enum ConnectionSendError {
    /// A channel id is invalid
    #[error("Channel with id `{0}` is invalid")]
    InvalidChannelId(ChannelId),
    /// A channel is closed
    #[error("Channel is closed")]
    ChannelClosed,
    /// Quinnet async channel error
    #[error("Quinnet async channel error")]
    ChannelSendError(#[from] AsyncChannelError),
}

/// Connection is already closed
#[derive(thiserror::Error, Debug)]
#[error("Connection is already closed")]
pub struct ConnectionAlreadyClosed;

#[cfg(feature = "recv_channels")]
/// Error while receiving payloads on a recv channel
#[derive(thiserror::Error, Debug, Clone)]
#[error("Error while receiving payload on a recv channel")]
pub enum RecvChannelError {
    /// The receiving channel queue is full, a payload has been dropped
    #[error("Channel queue with id {0} is full, a payload has been dropped")]
    RecvChannelFull(ChannelId),
    /// The maximum number of opened receive channels has been reached, a payload has been dropped
    #[error(
        "The maximum number of opened receive channels has been reached, triggered by channel id {0}. A payload has been dropped"
    )]
    MaxRecvChannelCountReached(ChannelId),
}

#[cfg(feature = "recv_channels")]
/// Event raised when there is an error while receiving data from the server
#[derive(bevy::ecs::message::Message, Debug, Clone)]
pub struct RecvChannelErrorEvent<T> {
    /// Local id of the connection
    pub id: T,
    /// Error raised during the reception
    pub error: RecvChannelError,
}
