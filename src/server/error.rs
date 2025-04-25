use crate::shared::{channels::ChannelId, error::AsyncChannelError, ClientId};

#[derive(thiserror::Error, Debug)]
pub enum SendError {
    /// A client id is unknown
    #[error("Client with id `{0}` is unknown")]
    UnknownClient(ClientId),
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
#[error("Error while sending to multiple recipients")]
pub struct GroupSendError(pub Vec<(ClientId, SendError)>);

#[derive(thiserror::Error, Debug)]
pub enum GroupPayloadSendError {
    /// There is no default channel
    #[error("There is no default channel")]
    NoDefaultChannel,

    #[error("TODO")]
    GroupSendError(#[from] GroupSendError),
}

#[derive(thiserror::Error, Debug)]
pub enum GroupMessageSendError {
    /// Failed serialization
    #[error("Failed serialization")]
    Serialization,
    /// There is no default channel
    #[error("There is no default channel")]
    NoDefaultChannel,

    #[error("TODO")]
    GroupSendError(#[from] GroupSendError),
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
    /// A client id is unknown
    #[error("Client with id `{0}` is unknown")]
    UnknownClient(ClientId),
    /// The receiving half of an internal channel was explicitly closed or has been dropped
    #[error(
        "The receiving half of the internal channel was explicitly closed or has been dropped"
    )]
    InternalChannelClosed,
}

#[derive(thiserror::Error, Debug)]
pub enum DisconnectError {
    /// A client id is unknown
    #[error("Client with id `{0}` is unknown")]
    UnknownClient(ClientId),
    /// A client is already disconnected
    #[error("Client with id `{0}` is already disconnected")]
    ClientAlreadyDisconnected(ClientId),
}

pub enum StopEndpointError {}

/// Endpoint is already closed
#[derive(thiserror::Error, Debug)]
#[error("Endpoint is already closed")]
pub struct EndpointAlreadyClosed;

#[derive(thiserror::Error, Debug)]
pub enum EndpointStartError {
    /// A lock acquisition failed
    #[error("Lock acquisition failure")]
    LockAcquisitionFailure,
    /// I/O Error
    #[error("I/O error")]
    IoError(#[from] std::io::Error),
    /// Certificate error
    #[error("Certificate error")]
    CertificateError(#[from] CertificateError),
    ///Rustls protocol error
    #[error("Rustls protocol error")]
    RustlsError(#[from] rustls::Error),

    #[error("TODO")]
    AsyncChannelError(#[from] AsyncChannelError),
}

#[derive(thiserror::Error, Debug)]
pub enum CertificateError {
    /// Failed to generate a self-signed certificate
    #[error("Failed to generate a self-signed certificate")]
    CertificateGenerationFailed(#[from] rcgen::Error),
    /// I/O Error
    #[error("I/O error")]
    IoError(#[from] std::io::Error),
}

#[derive(thiserror::Error, Debug)]
pub enum ConnectionCloseError {
    /// A connection is already closed
    #[error("Connection is already closed")]
    ConnectionAlreadyClosed,
}
