use bevy::log::error;

use crate::{
    client::{connection::ClientSideConnection, ClientSendError, ConnectionClosed},
    shared::channels::ChannelId,
};

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

impl ClientSideConnection {
    /// Attempt to deserialise a message into type `T`.
    ///
    /// Will return an [`Err`] if:
    /// - the bytes accumulated from the server aren't deserializable to T
    /// - or if the client is disconnected
    /// - (or if the message queue is full)
    pub fn receive_message<T: serde::de::DeserializeOwned, C: Into<ChannelId>>(
        &mut self,
        channel_id: C,
    ) -> Result<Option<T>, ClientMessageReceiveError> {
        match self.receive_payload(channel_id)? {
            Some(payload) => {
                match bincode::serde::decode_from_slice(&payload, bincode::config::standard()) {
                    Ok((msg, _size)) => Ok(Some(msg)),
                    Err(_) => Err(ClientMessageReceiveError::Deserialization),
                }
            }
            None => Ok(None),
        }
    }

    /// Same as [Self::receive_message] but will log the error instead of returning it
    pub fn try_receive_message<T: serde::de::DeserializeOwned, C: Into<ChannelId>>(
        &mut self,
        channel_id: C,
    ) -> Option<T> {
        match self.receive_message(channel_id) {
            Ok(message) => message,
            Err(err) => {
                error!("try_receive_message: {}", err);
                None
            }
        }
    }

    /// Queues a message to be sent to the server on the specified channel
    ///
    /// Will return an [`Err`] if:
    /// - the specified channel does not exist/is closed
    /// - or if the client is disconnected
    /// - or if a serialization error occurs
    /// - (or if the message queue is full)
    pub fn send_message_on<T: serde::Serialize, C: Into<ChannelId>>(
        &mut self,
        channel_id: C,
        message: T,
    ) -> Result<(), ClientMessageSendError> {
        match bincode::serde::encode_to_vec(&message, bincode::config::standard()) {
            Ok(payload) => Ok(self.send_payload_on(channel_id, payload)?),
            Err(_) => Err(ClientMessageSendError::Serialization),
        }
    }

    /// Same as [Self::send_message_on] but on the default channel
    pub fn send_message<T: serde::Serialize>(
        &mut self,
        message: T,
    ) -> Result<(), ClientMessageSendError> {
        match self.default_channel() {
            Some(channel) => self.send_message_on(channel, message),
            None => Err(ClientMessageSendError::NoDefaultChannel),
        }
    }

    /// Same as [Self::send_message] but will log the error instead of returning it
    pub fn try_send_message<T: serde::Serialize>(&mut self, message: T) {
        if let Err(err) = self.send_message(message) {
            error!("try_send_message: {}", err);
        }
    }

    /// Same as [Self::send_message_on] but will log the error instead of returning it
    pub fn try_send_message_on<T: serde::Serialize, C: Into<ChannelId>>(
        &mut self,
        channel_id: C,
        message: T,
    ) {
        if let Err(err) = self.send_message_on(channel_id, message) {
            error!("try_send_message_on: {}", err);
        }
    }
}
