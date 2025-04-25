use std::sync::PoisonError;

use crate::shared::{channels::ChannelId, error::AsyncChannelError};

use super::connection::ConnectionLocalId;

#[derive(thiserror::Error, Debug)]
pub enum SendError {
    /// A connection is closed
    #[error("Connection is 'disconnected'")]
    ConnectionClosed,
    /// A channel id is invalid
    #[error("Channel with id `{0}` is invalid")]
    InvalidChannelId(ChannelId),
    /// A channel is closed
    #[error("Channel is closed")]
    ChannelClosed,

    #[error("TODO")]
    ChannelSendError(#[from] AsyncChannelError),
}

#[derive(thiserror::Error, Debug)]
pub enum PayloadSendError {
    /// There is no default channel
    #[error("There is no default channel")]
    NoDefaultChannel,

    #[error("TODO")]
    SendError(#[from] SendError),
}

#[derive(thiserror::Error, Debug)]
pub enum MessageSendError {
    /// Failed serialization
    #[error("Failed serialization")]
    Serialization,
    /// There is no default channel
    #[error("There is no default channel")]
    NoDefaultChannel,

    #[error("TODO")]
    SendError(#[from] SendError),
}

#[derive(thiserror::Error, Debug)]
pub enum MessageReceiveError {
    /// Failed deserialization
    #[error("Failed deserialization")]
    Deserialization,

    #[error("TODO")]
    ReceiveError(#[from] ReceiveError),
}

#[derive(thiserror::Error, Debug)]
pub enum ReceiveError {
    /// A connection is closed
    #[error("Connection is 'disconnected'")]
    ConnectionClosed,
    /// The receiving half of an internal channel was explicitly closed or has been dropped
    #[error(
        "The receiving half of the internal channel was explicitly closed or has been dropped"
    )]
    InternalChannelClosed,
}

#[derive(thiserror::Error, Debug)]
pub enum ConnectionCloseError {
    /// A connection is already closed
    #[error("Connection is already closed")]
    ConnectionAlreadyClosed,
    /// A connection id is invalid
    #[error("Connection id `{0}` is invalid")]
    InvalidConnectionId(ConnectionLocalId),
}

#[derive(thiserror::Error, Debug)]
/// An host file is invalid
#[error("The hosts file is invalid")]
pub struct InvalidHostFile;

#[derive(thiserror::Error, Debug)]
pub enum CertificateInteractionError {
    /// A Certificate action was already sent for a CertificateInteractionEvent
    #[error("A Certificate action was already sent for a CertificateInteractionEvent")]
    CertificateActionAlreadyApplied,
    /// A lock acquisition failed
    #[error("Lock acquisition failure")]
    LockAcquisitionFailure,

    #[error("TODO")]
    AsyncChannelError(#[from] AsyncChannelError),
}

impl<T> From<PoisonError<T>> for CertificateInteractionError {
    fn from(_: PoisonError<T>) -> Self {
        Self::LockAcquisitionFailure
    }
}
