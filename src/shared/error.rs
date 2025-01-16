use std::{io, net::AddrParseError, sync::PoisonError};

use crate::client::connection::ConnectionLocalId;

use super::{channels::ChannelId, ClientId};

/// Enum with possibles errors that can occur in Bevy Quinnet
#[derive(thiserror::Error, Debug)]
pub enum QuinnetError {
    /// IP/Socket address is invalid
    #[error("IP/Socket address is invalid")]
    InvalidAddress(#[from] AddrParseError),
    /// Failed to generate a self-signed certificate
    #[error("Failed to generate a self-signed certificate")]
    CertificateGenerationFailed(#[from] rcgen::Error),
    /// A client id is unknown
    #[error("Client with id `{0}` is unknown")]
    UnknownClient(ClientId),
    /// A client is already disconnected
    #[error("Client with id `{0}` is already disconnected")]
    ClientAlreadyDisconnected(ClientId),
    /// A connection id is unknown
    #[error("Connection with id `{0}` is unknown")]
    UnknownConnection(ConnectionLocalId),
    /// A connection is closed
    #[error("Connection is 'disconnected'")]
    ConnectionClosed,
    /// A connection is already closed
    #[error("Connection is already closed")]
    ConnectionAlreadyClosed,
    /// A channel is unknown
    #[error("Channel with id `{0}` is unknown")]
    UnknownChannel(ChannelId),
    /// A channel is already closed
    #[error("Channel is already closed")]
    ChannelClosed,
    /// A connection has no default channel
    #[error("The connection has no default channel")]
    NoDefaultChannel,
    /// The maximum number of simultaneously opened channels has been reached
    #[error("The maximum number of simultaneously opened channels has been reached")]
    MaxChannelsCountReached,
    /// Endpoint is already closed
    #[error("Endpoint is already closed")]
    EndpointAlreadyClosed,
    /// Failed serialization
    #[error("Failed serialization")]
    Serialization,
    /// Failed deserialization
    #[error("Failed deserialization")]
    Deserialization,
    /// The data could not be sent on the channel because the channel is currently full and sending would require blocking
    #[error("The data could not be sent on the channel because the channel is currently full and sending would require blocking")]
    FullQueue,
    /// The receiving half of an internal channel was explicitly closed or has been dropped
    #[error(
        "The receiving half of the internal channel was explicitly closed or has been dropped"
    )]
    InternalChannelClosed,
    /// An host file is invalid
    #[error("The hosts file is invalid")]
    InvalidHostFile,
    /// A lock acquisition failed
    #[error("Lock acquisition failure")]
    LockAcquisitionFailure,
    /// A Certificate action was already sent for a CertificateInteractionEvent
    #[error("A Certificate action was already sent for a CertificateInteractionEvent")]
    CertificateActionAlreadyApplied,
    /// I/O Error
    #[error("I/O error")]
    IoError(#[from] io::Error),
    ///Rustls protocol error
    #[error("Rustls protocol error")]
    RustlsError(#[from] rustls::Error),
}

impl<T> From<PoisonError<T>> for QuinnetError {
    fn from(_: PoisonError<T>) -> Self {
        Self::LockAcquisitionFailure
    }
}
