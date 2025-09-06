use std::collections::VecDeque;

use bevy::log::error;
use bytes::Bytes;
use tokio::sync::{
    broadcast,
    mpsc::{
        self,
        error::{TryRecvError, TrySendError},
    },
};

use crate::{
    server::{
        EndpointConnectionAlreadyClosed, ServerReceiveError, ServerSendError, ServerSyncMessage,
    },
    shared::{
        channels::{
            Channel, ChannelAsyncMessage, ChannelId, ChannelKind, ChannelSyncMessage, CloseReason,
        },
        error::{AsyncChannelError, ChannelCloseError},
        InternalConnectionRef, DEFAULT_KILL_MESSAGE_QUEUE_SIZE, DEFAULT_MESSAGE_QUEUE_SIZE,
    },
};

#[derive(Debug)]
struct BidirChannel {
    send_channel: Channel,
    // TODO Might want to limit this buffer size
    // TODO Might move into the Channel struct
    received_payloads: VecDeque<Bytes>,
}
impl BidirChannel {
    fn close(mut self) -> Result<(), ChannelCloseError> {
        self.received_payloads.clear();
        self.send_channel.close()
    }

    fn new(channel: Channel) -> Self {
        Self {
            send_channel: channel,
            received_payloads: VecDeque::new(),
        }
    }
}

/// Represents a connection from a quinnet client to a server's [`Endpoint`], viewed from the server.
#[derive(Debug)]
pub struct ServerSideConnection {
    connection_handle: InternalConnectionRef,

    open_channels: Vec<Option<BidirChannel>>,
    /// Contains payloads directed to an unknown or closed channel.
    ///
    /// Cleared at the end of every frame by [`super::post_update_sync_server`].
    pub invalid_payloads: Vec<Bytes>,

    bytes_from_client_recv: mpsc::Receiver<(ChannelId, Bytes)>,
    close_sender: broadcast::Sender<CloseReason>,

    to_connection_send: mpsc::Sender<ServerSyncMessage>,
    to_channels_send: mpsc::Sender<ChannelSyncMessage>,
    from_channels_recv: mpsc::Receiver<ChannelAsyncMessage>,

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
            open_channels: Vec::new(),
            invalid_payloads: Vec::new(),
            received_bytes_count: 0,
            sent_bytes_count: 0,
        }
    }

    /// Immediately prevents new messages from being sent on the channel and signal the channel to closes all its background tasks.
    /// Before trully closing, the channel will wait for all buffered messages to be properly sent according to the channel type.
    /// Can fail if the [ChannelId] is unknown, or if the channel is already closed.
    pub(crate) fn close_channel(&mut self, channel_id: ChannelId) -> Result<(), ChannelCloseError> {
        if (channel_id as usize) < self.open_channels.len() {
            match self.open_channels[channel_id as usize].take() {
                Some(channel_to_close) => channel_to_close.close(),
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
        let opened_channel = Some(BidirChannel::new(channel));
        if channel_index < self.open_channels.len() {
            self.open_channels[channel_index] = opened_channel;
        } else {
            self.open_channels
                .extend((self.open_channels.len()..channel_index).map(|_| None));
            self.open_channels.push(opened_channel);
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

    pub(crate) fn send_payload(
        &mut self,
        channel_id: ChannelId,
        payload: Bytes,
    ) -> Result<(), ServerSendError> {
        match self.open_channels.get(channel_id as usize) {
            Some(Some(channel)) => {
                self.sent_bytes_count += payload.len();
                Ok(channel.send_channel.send_payload(payload)?)
            }
            Some(None) => Err(ServerSendError::ChannelClosed),
            None => Err(ServerSendError::InvalidChannelId(channel_id)),
        }
    }

    #[inline(always)]
    pub(crate) fn try_send_to_async_connection(
        &self,
        msg: ServerSyncMessage,
    ) -> Result<(), TrySendError<ServerSyncMessage>> {
        self.to_connection_send.try_send(msg)
    }

    #[inline(always)]
    pub(crate) fn try_recv_from_channels(&mut self) -> Result<ChannelAsyncMessage, TryRecvError> {
        self.from_channels_recv.try_recv()
    }

    pub(crate) fn receive_payload_from(
        &mut self,
        channel_id: ChannelId,
    ) -> (Result<Option<Bytes>, ServerReceiveError>, u64) {
        match self.open_channels.get_mut(channel_id as usize) {
            // TODO Drain variant ?
            Some(Some(channel)) => (Ok(channel.received_payloads.pop_front()), 1),
            Some(None) | None => (Err(ServerReceiveError::InvalidChannel(channel_id)), 0),
        }
    }

    pub(crate) fn dispatch_payloads_to_channels(&mut self) {
        // Note on handling of: TryRecvError::Disconnected
        // This error means that the receiving end of the channel is closed, which only happens when the client connection is closed/closing.
        // In this case we decide to consider that there is no more messages to receive.
        while let Ok((channel_id, payload)) = self.bytes_from_client_recv.try_recv() {
            self.received_bytes_count += payload.len();
            match self.open_channels.get_mut(channel_id as usize) {
                Some(Some(channel)) => channel.received_payloads.push_back(payload),
                Some(None) | None => self.invalid_payloads.push(payload),
            }
        }
    }

    pub(crate) fn clear_invalid_payloads(&mut self) {
        self.invalid_payloads.clear();
    }
}
