use std::sync::PoisonError;

use crate::shared::error::{AsyncChannelError, ConnectionAlreadyClosed, ConnectionSendError};

use super::connection::ConnectionLocalId;

/// Error when sending data from the client
#[derive(thiserror::Error, Debug)]
pub enum ClientSendError {
    /// A connection is closed
    #[error("Connection is 'disconnected'")]
    ConnectionClosed,
    /// Error when sending data on the connection
    #[error("Error when sending data on the connection")]
    ConnectionSendError(#[from] ConnectionSendError),
}

/// Error when sending a payload from the client
#[derive(thiserror::Error, Debug)]
pub enum ClientPayloadSendError {
    /// There is no default channel
    #[error("There is no default channel")]
    NoDefaultChannel,

    // /// Error when sending data on the connection
    // #[error("Error when sending data on the connection")]
    // ConnectionSendError(#[from] ConnectionSendError),
    /// Error when sending
    #[error("Error when sending")]
    SendError(#[from] ClientSendError),
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
    ConnectionAlreadyClosed(#[from] ConnectionAlreadyClosed),
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
