use bevy::log::error;
use bytes::Bytes;
use tokio::sync::{
    broadcast,
    mpsc::{self, error::TrySendError},
};

use crate::{
    server::{EndpointConnectionAlreadyClosed, ServerSendError, ServerSyncMessage},
    shared::{
        channels::{
            Channel, ChannelAsyncMessage, ChannelId, ChannelKind, ChannelSyncMessage, CloseReason,
        },
        error::{AsyncChannelError, ChannelCloseError},
        InternalConnectionRef, DEFAULT_KILL_MESSAGE_QUEUE_SIZE, DEFAULT_MESSAGE_QUEUE_SIZE,
    },
};

/// Represents a connection from a quinnet client to a server's [`Endpoint`], viewed from the server.
#[derive(Debug)]
pub struct ServerSideConnection {
    connection_handle: InternalConnectionRef,

    channels: Vec<Option<Channel>>,
    bytes_from_client_recv: mpsc::Receiver<(ChannelId, Bytes)>,
    close_sender: broadcast::Sender<CloseReason>,

    pub(crate) to_connection_send: mpsc::Sender<ServerSyncMessage>,
    pub(crate) to_channels_send: mpsc::Sender<ChannelSyncMessage>,
    pub(crate) from_channels_recv: mpsc::Receiver<ChannelAsyncMessage>,

    pub(crate) received_bytes_count: usize,
    pub(crate) sent_bytes_count: usize,
}

impl ServerSideConnection {
    pub(crate) fn new(
        connection_handle: InternalConnectionRef,
        bytes_from_client_recv: mpsc::Receiver<(ChannelId, Bytes)>,
        close_sender: broadcast::Sender<CloseReason>,
        to_connection_send: mpsc::Sender<ServerSyncMessage>,
        from_channels_recv: mpsc::Receiver<ChannelAsyncMessage>,
        to_channels_send: mpsc::Sender<ChannelSyncMessage>,
    ) -> Self {
        Self {
            connection_handle,
            bytes_from_client_recv,
            close_sender,
            to_connection_send,
            to_channels_send,
            from_channels_recv,
            channels: Vec::new(),
            received_bytes_count: 0,
            sent_bytes_count: 0,
        }
    }

    /// Immediately prevents new messages from being sent on the channel and signal the channel to closes all its background tasks.
    /// Before trully closing, the channel will wait for all buffered messages to be properly sent according to the channel type.
    /// Can fail if the [ChannelId] is unknown, or if the channel is already closed.
    pub(crate) fn close_channel(&mut self, channel_id: ChannelId) -> Result<(), ChannelCloseError> {
        if (channel_id as usize) < self.channels.len() {
            match self.channels[channel_id as usize].take() {
                Some(channel) => channel.close(),
                None => Err(ChannelCloseError::ChannelAlreadyClosed),
            }
        } else {
            Err(ChannelCloseError::InvalidChannelId(channel_id))
        }
    }

    pub(crate) fn create_connection_channel(
        &mut self,
        id: ChannelId,
        kind: ChannelKind,
    ) -> Result<(), AsyncChannelError> {
        let channel = self.create_unregistered_connection_channel(id, kind)?;
        self.register_connection_channel(channel);
        Ok(())
    }

    pub(crate) fn create_unregistered_connection_channel(
        &mut self,
        id: ChannelId,
        kind: ChannelKind,
    ) -> Result<Channel, AsyncChannelError> {
        let (bytes_to_channel_send, bytes_to_channel_recv) =
            mpsc::channel::<Bytes>(DEFAULT_MESSAGE_QUEUE_SIZE);
        let (channel_close_send, channel_close_recv) =
            mpsc::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);

        match self
            .to_channels_send
            .try_send(ChannelSyncMessage::CreateChannel {
                id,
                kind,
                bytes_to_channel_recv,
                channel_close_recv,
            }) {
            Ok(_) => Ok(Channel::new(id, bytes_to_channel_send, channel_close_send)),
            Err(err) => match err {
                TrySendError::Full(_) => Err(AsyncChannelError::FullQueue),
                TrySendError::Closed(_) => Err(AsyncChannelError::InternalChannelClosed),
            },
        }
    }

    pub(crate) fn register_connection_channel(&mut self, channel: Channel) {
        let channel_index = channel.id() as usize;
        if channel_index < self.channels.len() {
            self.channels[channel_index] = Some(channel);
        } else {
            self.channels
                .extend((self.channels.len()..channel_index).map(|_| None));
            self.channels.push(Some(channel));
        }
    }

    /// Signal the connection to closes all its background tasks. Before trully closing, the connection will wait for all buffered messages in all its opened channels to be properly sent according to their respective channel type.r
    pub(crate) fn close(
        &mut self,
        reason: CloseReason,
    ) -> Result<(), EndpointConnectionAlreadyClosed> {
        match self.close_sender.send(reason) {
            Ok(_) => Ok(()),
            Err(_) => {
                // The only possible error for a send is that there is no active receivers, meaning that the tasks are already terminated.
                Err(EndpointConnectionAlreadyClosed)
            }
        }
    }

    pub(crate) fn try_close(&mut self, reason: CloseReason) {
        match &self.close(reason) {
            Ok(_) => (),
            Err(err) => error!("Failed to properly close clonnection: {}", err),
        }
    }

    /// See [quinn::Connection::max_datagram_size]
    #[inline(always)]
    pub fn max_datagram_size(&self) -> Option<usize> {
        self.connection_handle.max_datagram_size()
    }

    /// Returns statistics about a client connection
    #[inline(always)]
    pub fn connection_stats(&self) -> quinn::ConnectionStats {
        self.connection_handle.stats()
    }

    /// Returns how many bytes were received on this connection since the last time it was cleared and reset this value to 0
    pub fn clear_received_bytes_count(&mut self) -> usize {
        let bytes_count = self.received_bytes_count;
        self.received_bytes_count = 0;
        bytes_count
    }

    /// Returns how many bytes were received on this connection since the last time it was cleared
    #[inline(always)]
    pub fn received_bytes_count(&self) -> usize {
        self.received_bytes_count
    }

    /// Returns how many bytes were received on this connection since the last time it was cleared and reset this value to 0
    pub fn clear_sent_bytes_count(&mut self) -> usize {
        let bytes_count = self.sent_bytes_count;
        self.sent_bytes_count = 0;
        bytes_count
    }

    /// Returns how many bytes were received on this connection since the last time it was cleared
    #[inline(always)]
    pub fn sent_bytes_count(&self) -> usize {
        self.sent_bytes_count
    }

    #[inline(always)]
    pub(crate) fn try_recv_bytes(&mut self) -> Result<(u8, Bytes), mpsc::error::TryRecvError> {
        self.bytes_from_client_recv.try_recv()
    }

    pub(crate) fn send_payload(
        &mut self,
        channel_id: ChannelId,
        payload: Bytes,
    ) -> Result<(), ServerSendError> {
        match self.channels.get(channel_id as usize) {
            Some(Some(channel)) => {
                self.sent_bytes_count += payload.len();
                Ok(channel.send_payload(payload)?)
            }
            Some(None) => Err(ServerSendError::ChannelClosed),
            None => Err(ServerSendError::InvalidChannelId(channel_id)),
        }
    }
}
