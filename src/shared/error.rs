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
