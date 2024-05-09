use std::{io, net::AddrParseError, sync::PoisonError};

use crate::client::connection::ConnectionLocalId;

use super::{channels::ChannelId, ClientId};

/// Enum with possibles errors that can occur in Bevy Quinnet
#[derive(thiserror::Error, Debug)]
pub enum QuinnetError {
    #[error("IP/Socket address is invalid")]
    InvalidAddress(#[from] AddrParseError),
    #[error("Failed to generate a self-signed certificate")]
    CertificateGenerationFailed(#[from] rcgen::Error),
    #[error("Client with id `{0}` is unknown")]
    UnknownClient(ClientId),
    #[error("Client with id `{0}` is already disconnected")]
    ClientAlreadyDisconnected(ClientId),
    #[error("Connection with id `{0}` is unknown")]
    UnknownConnection(ConnectionLocalId),
    #[error("Connection is 'disconnected'")]
    ConnectionClosed,
    #[error("Connection is already closed")]
    ConnectionAlreadyClosed,
    #[error("Channel with id `{0}` is unknown")]
    UnknownChannel(ChannelId),
    #[error("Channel is already closed")]
    ChannelClosed,
    #[error("The connection has no default channel")]
    NoDefaultChannel,
    #[error("The maximum number of simultaneously opened channels has been reached")]
    MaxChannelsCountReached,
    #[error("Endpoint is already closed")]
    EndpointAlreadyClosed,
    #[error("Failed serialization")]
    Serialization,
    #[error("Failed deserialization")]
    Deserialization,
    #[error("The data could not be sent on the channel because the channel is currently full and sending would require blocking")]
    FullQueue,
    #[error(
        "The receiving half of the internal channel was explicitly closed or has been dropped"
    )]
    InternalChannelClosed,
    #[error("The hosts file is invalid")]
    InvalidHostFile,
    #[error("Lock acquisition failure")]
    LockAcquisitionFailure,
    #[error("A Certificate action was already sent for a CertificateInteractionEvent")]
    CertificateActionAlreadyApplied,
    #[error("Failed to read/write file(s)")]
    IoError(#[from] io::Error),
    #[error("Rustls protocol error")]
    RustlsError(#[from] rustls::Error),
}

impl<T> From<PoisonError<T>> for QuinnetError {
    fn from(_: PoisonError<T>) -> Self {
        Self::LockAcquisitionFailure
    }
}
