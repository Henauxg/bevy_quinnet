use std::sync::PoisonError;

use crate::shared::{channels::ChannelId, error::AsyncChannelError};

use super::connection::ConnectionLocalId;

/// Error when sending data from the client
#[derive(thiserror::Error, Debug)]
pub enum ClientSendError {
    /// A connection is closed
    #[error("Connection is 'disconnected'")]
    ConnectionClosed,
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

/// Error when sending a payload from the client
#[derive(thiserror::Error, Debug)]
pub enum ClientPayloadSendError {
    /// There is no default channel
    #[error("There is no default channel")]
    NoDefaultChannel,
    /// Error when sending
    #[error("Error when sending")]
    SendError(#[from] ClientSendError),
}

/// Error when sending a message to be serialized from the client
#[derive(thiserror::Error, Debug)]
pub enum ClientMessageSendError {
    /// Failed serialization
    #[error("Failed serialization")]
    Serialization,
    /// There is no default channel
    #[error("There is no default channel")]
    NoDefaultChannel,
    /// Error when sending data
    #[error("Error when sending data")]
    SendError(#[from] ClientSendError),
}

/// Error while receiving a message on the client
#[derive(thiserror::Error, Debug)]
pub enum ClientMessageReceiveError {
    /// Failed deserialization
    #[error("Failed deserialization")]
    Deserialization,
    /// Error while receiving data
    #[error("Error while receiving data")]
    ConnectionClosed(#[from] ConnectionClosed),
}

/// The client connection is closed
#[derive(thiserror::Error, Debug)]
#[error("The client connection is closed")]
pub struct ConnectionClosed;

/// Error while closing a connection
#[derive(thiserror::Error, Debug)]
pub enum ClientConnectionCloseError {
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

/// Error while applying a certificate action
#[derive(thiserror::Error, Debug)]
pub enum CertificateInteractionError {
    /// A Certificate action was already sent for a CertificateInteractionEvent
    #[error("A Certificate action was already sent for a CertificateInteractionEvent")]
    CertificateActionAlreadyApplied,
    /// A lock acquisition failed
    #[error("Lock acquisition failure")]
    LockAcquisitionFailure,
    /// Quinnet async channel error
    #[error("Quinnet async channel error")]
    AsyncChannelError(#[from] AsyncChannelError),
}

impl<T> From<PoisonError<T>> for CertificateInteractionError {
    fn from(_: PoisonError<T>) -> Self {
        Self::LockAcquisitionFailure
    }
}
