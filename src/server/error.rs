use crate::shared::{channels::ChannelId, error::AsyncChannelError, ClientId};

/// Error when sending data from the server
#[derive(thiserror::Error, Debug)]
pub enum ServerSendError {
    /// A client id is unknown
    #[error("Client with id `{0}` is unknown")]
    UnknownClient(ClientId),
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

/// Error while sending a payload on the server
#[derive(thiserror::Error, Debug)]
pub enum ServerPayloadSendError {
    /// There is no default channel
    #[error("There is no default channel")]
    NoDefaultChannel,
    /// Error when sending data
    #[error("Error when sending data")]
    SendError(#[from] ServerSendError),
}

/// Error while sending data on the server to a group of clients
#[derive(thiserror::Error, Debug)]
#[error("Error while sending to multiple recipients")]
pub struct ServerGroupSendError(pub Vec<(ClientId, ServerSendError)>);

/// Error while sending a payload on the server to a group of clients
#[derive(thiserror::Error, Debug)]
pub enum ServerGroupPayloadSendError {
    /// There is no default channel
    #[error("There is no default channel")]
    NoDefaultChannel,
    /// Error while sending data to a group of clients
    #[error("Error while sending data to a group of clients")]
    GroupSendError(#[from] ServerGroupSendError),
}

/// Error while receiving data on the server
#[derive(thiserror::Error, Debug)]
pub enum ServerReceiveError {
    /// A client id is unknown
    #[error("Client with id `{0}` is unknown")]
    UnknownClient(ClientId),
    /// A channel id is invalid
    #[error("Channel with id `{0}` is invalid")]
    InvalidChannel(ChannelId),
}

/// Error while disconnecting a client on the server
#[derive(thiserror::Error, Debug)]
pub enum ServerDisconnectError {
    /// A client id is unknown
    #[error("Client with id `{0}` is unknown")]
    UnknownClient(ClientId),
    /// A client is already disconnected
    #[error("Client with id `{0}` is already disconnected")]
    ClientAlreadyDisconnected(ClientId),
}

/// Endpoint is already closed
#[derive(thiserror::Error, Debug)]
#[error("Endpoint is already closed")]
pub struct EndpointAlreadyClosed;

/// Error while starting an Endpoint
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
    CertificateError(#[from] EndpointCertificateError),
    ///Rustls protocol error
    #[error("Rustls protocol error")]
    RustlsError(#[from] rustls::Error),
    /// Quinnet async channel error
    #[error("Quinnet async channel error")]
    AsyncChannelError(#[from] AsyncChannelError),
}

/// Error while retrieving a certificate on the server
#[derive(thiserror::Error, Debug)]
pub enum EndpointCertificateError {
    /// Failed to generate a self-signed certificate
    #[error("Failed to generate a self-signed certificate")]
    CertificateGenerationFailed(#[from] rcgen::Error),
    /// I/O Error
    #[error("I/O error")]
    IoError(#[from] std::io::Error),
}

/// Endpoint connection is already closed
#[derive(thiserror::Error, Debug)]
#[error("Endpoint connection is already closed")]
pub(crate) struct EndpointConnectionAlreadyClosed;
