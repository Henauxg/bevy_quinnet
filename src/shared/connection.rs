use std::collections::{BTreeSet, VecDeque};

use bevy::log::error;
use bytes::Bytes;
use tokio::sync::{
    broadcast,
    mpsc::{
        self,
        error::{TryRecvError, TrySendError},
    },
};

use crate::shared::{
    channels::{
        Channel, ChannelAsyncMessage, ChannelConfig, ChannelId, ChannelSyncMessage, CloseReason,
        CloseSend, MAX_CHANNEL_COUNT,
    },
    error::{
        AsyncChannelError, ChannelCloseError, ChannelCreationError, ConnectionAlreadyClosed,
        ConnectionSendError,
    },
    DEFAULT_KILL_MESSAGE_QUEUE_SIZE, DEFAULT_MESSAGE_QUEUE_SIZE,
};

/// Default value for the `max_buffered_payloads_count_per_channel` field of a [`ConnectionParameters`]
pub const DEFAULT_MAX_BUFFERED_PAYLOADS_COUNT_PER_CHANNEL: usize = 512;
/// Default value for the `max_receive_channels_count` field of a [`ConnectionParameters`]
pub const DEFAULT_MAX_RECEIVE_CHANNEL_COUNT: usize = MAX_CHANNEL_COUNT;
/// Default value for the `clear_stale_payloads` fields
pub const DEFAULT_CLEAR_STALE_RECEIVED_PAYLOADS: bool = true;

pub(crate) type PayloadSend = mpsc::Sender<(ChannelId, Bytes)>;
pub(crate) type PayloadRecv = mpsc::Receiver<(ChannelId, Bytes)>;
pub(crate) type ChannelAsyncMsgSend = mpsc::Sender<ChannelAsyncMessage>;
pub(crate) type ChannelAsyncMsgRecv = mpsc::Receiver<ChannelAsyncMessage>;
pub(crate) type ChannelSyncMsgSend = mpsc::Sender<ChannelSyncMessage>;
pub(crate) type ChannelSyncMsgRecv = mpsc::Receiver<ChannelSyncMessage>;

/// Configuration for a [PeerConnection].
///
/// See [crate::server::connection::ServerSideConnection] and [crate::client::connection::ClientSideConnection].
#[derive(Debug, Clone)]
pub struct ConnectionParameters {
    /// Maximum number of payloads that can be buffered per receive channel.
    pub max_buffered_payloads_count_per_channel: usize,
    /// Maximum number of receive channels that can be opened on this connection.
    pub max_receive_channels_count: usize,
    /// If `true`, payloads on receive channels that were not read during this update will be cleared at the end of an Update cycle, in the [crate::shared::QuinnetSyncPostUpdate] schedule.
    ///
    /// Defaults to [DEFAULT_CLEAR_STALE_RECEIVED_PAYLOADS].
    pub clear_stale_received_payloads: bool,
}
impl Default for ConnectionParameters {
    fn default() -> Self {
        Self {
            max_buffered_payloads_count_per_channel:
                DEFAULT_MAX_BUFFERED_PAYLOADS_COUNT_PER_CHANNEL,
            max_receive_channels_count: DEFAULT_MAX_RECEIVE_CHANNEL_COUNT,
            clear_stale_received_payloads: DEFAULT_CLEAR_STALE_RECEIVED_PAYLOADS,
        }
    }
}

pub(crate) struct ChannelsIdsPool {
    /// Internal ordered pool of available channel ids
    available_channel_ids: BTreeSet<ChannelId>,

    /// Default channel id
    default_channel: Option<ChannelId>,
}

impl ChannelsIdsPool {
    pub(crate) fn new() -> Self {
        let available_channel_ids = (0..=u8::MAX).collect();
        Self {
            available_channel_ids,
            default_channel: None,
        }
    }

    /// Allocates a new channel id from the pool
    ///
    /// Returns an error if no channel ids are available
    pub(crate) fn take_id(&mut self) -> Result<ChannelId, ChannelCreationError> {
        match self.available_channel_ids.pop_first() {
            Some(channel_id) => {
                if self.default_channel.is_none() {
                    self.default_channel = Some(channel_id);
                }
                Ok(channel_id)
            }
            None => Err(ChannelCreationError::MaxChannelsCountReached),
        }
    }

    /// Releases a previously allocated channel id back to the pool
    pub(crate) fn release_id(&mut self, channel_id: ChannelId) {
        self.available_channel_ids.insert(channel_id);
        if self.default_channel == Some(channel_id) {
            self.default_channel = None;
        }
    }

    /// Sets the default send channel id
    #[inline(always)]
    pub(crate) fn set_default_channel(&mut self, channel_id: ChannelId) {
        self.default_channel = Some(channel_id);
    }

    /// Gets the default send channel id
    #[inline(always)]
    pub(crate) fn default_channel(&self) -> Option<ChannelId> {
        self.default_channel
    }
}

/// A connection to a peer, containing multiple channels for sending data, as well as buffers for received data.
pub struct PeerConnection<S> {
    pub(crate) specific: S,
    /// Send channels opened on this connection
    send_channels: Vec<Option<Channel>>,
    /// Buffers of received payloads per receive channel
    receive_channels: Vec<VecDeque<Bytes>>,
    /// Internal queue of received bytes from the peer
    bytes_from_peer_recv: mpsc::Receiver<(ChannelId, Bytes)>,
    /// Sender for internal quinnet messages going to the async channels task
    to_channels_send: mpsc::Sender<ChannelSyncMessage>,
    /// Receiver for internal quinnet messages coming from the async channels task
    from_channels_recv: mpsc::Receiver<ChannelAsyncMessage>,
    close_send: broadcast::Sender<CloseReason>,
    stats: ConnectionStats,
    /// Parameters for the connection
    parameters: ConnectionParameters,
}

impl<S> PeerConnection<S> {
    pub(crate) fn new(
        specific: S,
        bytes_from_peer_recv: mpsc::Receiver<(ChannelId, Bytes)>,
        close_send: broadcast::Sender<CloseReason>,
        from_channels_recv: mpsc::Receiver<ChannelAsyncMessage>,
        to_channels_send: mpsc::Sender<ChannelSyncMessage>,
        config: ConnectionParameters,
    ) -> Self {
        Self {
            specific,
            send_channels: Vec::new(),
            receive_channels: Vec::new(),
            bytes_from_peer_recv,
            close_send,
            to_channels_send,
            from_channels_recv,
            parameters: config,
            stats: ConnectionStats::default(),
        }
    }

    pub(crate) fn internal_send_payload(
        &mut self,
        channel_id: ChannelId,
        payload: Bytes,
    ) -> Result<(), ConnectionSendError> {
        match self.send_channels.get(channel_id as usize) {
            Some(Some(channel)) => {
                self.stats.sent(payload.len());
                Ok(channel.send_payload(payload)?)
            }
            Some(None) => Err(ConnectionSendError::ChannelClosed),
            None => Err(ConnectionSendError::InvalidChannelId(channel_id)),
        }
    }

    pub(crate) fn internal_receive_payload(&mut self, channel_id: ChannelId) -> Option<Bytes> {
        match self.receive_channels.get_mut(channel_id as usize) {
            // TODO Drain variant ?
            Some(payloads) => payloads.pop_front(),
            None => None,
        }
    }

    // TODO Result<(), (Vec<(ReceiveError,ChannelId)>)>
    pub(crate) fn dispatch_received_payloads_to_channel_buffers(&mut self) {
        // Note on handling of: TryRecvError::Disconnected
        // This error means that the receiving end of the channel is closed, which only happens when the client connection is closed/closing.
        // In this case we decide to consider that there is no more messages to receive.
        while let Ok((channel_id, payload)) = self.bytes_from_peer_recv.try_recv() {
            self.stats.received(payload.len());

            match self.receive_channels.get_mut(channel_id as usize) {
                Some(payloads) => {
                    if payloads.len() < self.parameters.max_buffered_payloads_count_per_channel {
                        payloads.push_back(payload);
                    } else {
                        error!("Dropping received payload on channel {} because the receive queue is full", channel_id);
                    }
                }
                None => {
                    if self.receive_channels.len() < self.parameters.max_receive_channels_count {
                        self.receive_channels.extend(
                            (self.receive_channels.len()..channel_id as usize)
                                .map(|_| VecDeque::new()),
                        );
                        self.receive_channels.push(VecDeque::from([payload]));
                    } else {
                        error!("Dropping received payload on channel {} because the maximum number of opened receive channels has been reached", channel_id);
                    }
                }
            }
        }
    }

    pub(crate) fn checked_clear_stale_received_payloads(&mut self) {
        if self.parameters.clear_stale_received_payloads {
            self.internal_clear_stale_received_payloads();
        }
    }

    #[inline(always)]
    pub(crate) fn unchecked_clear_stale_received_payloads(&mut self) {
        self.internal_clear_stale_received_payloads();
    }

    fn internal_clear_stale_received_payloads(&mut self) {
        for payloads in self.receive_channels.iter_mut() {
            payloads.clear();
        }
    }

    /// Immediately prevents new messages from being sent on the channel and signal the channel to closes all its background tasks.
    /// Before trully closing, the channel will wait for all buffered messages to be properly sent according to the channel type.
    /// Can fail if the [ChannelId] is unknown, or if the channel is already closed.
    pub(crate) fn internal_close_channel(
        &mut self,
        channel_id: ChannelId,
    ) -> Result<(), ChannelCloseError> {
        if (channel_id as usize) < self.send_channels.len() {
            match self.send_channels[channel_id as usize].take() {
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
        channel_config: ChannelConfig,
    ) -> Result<(), AsyncChannelError> {
        let channel = self.create_unregistered_connection_channel(id, channel_config)?;
        self.register_connection_channel(channel);
        Ok(())
    }

    pub(crate) fn create_unregistered_connection_channel(
        &mut self,
        id: ChannelId,
        config: ChannelConfig,
    ) -> Result<Channel, AsyncChannelError> {
        let (bytes_to_channel_send, bytes_to_channel_recv) =
            mpsc::channel::<Bytes>(DEFAULT_MESSAGE_QUEUE_SIZE);
        let (channel_close_send, channel_close_recv) =
            mpsc::channel(DEFAULT_KILL_MESSAGE_QUEUE_SIZE);

        match self
            .to_channels_send
            .try_send(ChannelSyncMessage::CreateChannel {
                id,
                config,
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
        if channel_index < self.send_channels.len() {
            self.send_channels[channel_index] = Some(channel);
        } else {
            self.send_channels
                .extend((self.send_channels.len()..channel_index).map(|_| None));
            self.send_channels.push(Some(channel));
        }
    }

    /// Signal the connection to closes all its background tasks. Before trully closing, the connection will wait for all buffered messages in all its opened channels to be properly sent according to their respective channel type.
    pub(crate) fn close(&mut self, reason: CloseReason) -> Result<(), ConnectionAlreadyClosed> {
        match self.close_send.send(reason) {
            Ok(_) => Ok(()),
            Err(_) => {
                // The only possible error for a send is that there is no active receivers, meaning that the tasks are already terminated.
                Err(ConnectionAlreadyClosed)
            }
        }
    }

    #[inline(always)]
    pub(crate) fn try_recv_from_channels(&mut self) -> Result<ChannelAsyncMessage, TryRecvError> {
        self.from_channels_recv.try_recv()
    }

    /// Returns a mutable reference to the connection statistics
    pub fn stats_mut(&mut self) -> &mut ConnectionStats {
        &mut self.stats
    }

    /// Returns a reference to the connection statistics
    pub fn stats(&self) -> &ConnectionStats {
        &self.stats
    }

    pub(crate) fn try_close(&mut self, reason: CloseReason) {
        match &self.close(reason) {
            Ok(_) => (),
            Err(err) => error!("Failed to properly close clonnection: {}", err),
        }
    }

    pub(crate) fn internal_reset(
        &mut self,
        close_send: CloseSend,
        to_channels_send: ChannelSyncMsgSend,
        from_channels_recv: mpsc::Receiver<ChannelAsyncMessage>,
        bytes_from_peer_recv: PayloadRecv,
        send_channels_capacity: usize,
    ) {
        self.close_send = close_send;
        self.to_channels_send = to_channels_send;
        self.from_channels_recv = from_channels_recv;
        self.bytes_from_peer_recv = bytes_from_peer_recv;
        self.send_channels = Vec::with_capacity(send_channels_capacity);
        self.receive_channels.clear();
        self.stats_mut().reset();
    }

    /// Enables or disables [`ConnectionParameters::clear_stale_received_payloads`] on this connection.
    #[inline(always)]
    pub fn set_clear_stale_received_payloads(&mut self, enable: bool) {
        self.parameters.clear_stale_received_payloads = enable;
    }
}

/// Basic quinnet stats about a connection
#[derive(Default)]
pub struct ConnectionStats {
    received_bytes_count: u64,
    sent_bytes_count: u64,
    received_messages_count: u64,
}
impl ConnectionStats {
    /// Returns how many bytes were received on this connection since the last time it was cleared
    pub fn received_bytes_count(&self) -> u64 {
        self.received_bytes_count
    }

    /// Returns how many bytes were sent on this connection since the last time it was cleared
    pub fn sent_bytes_count(&self) -> u64 {
        self.sent_bytes_count
    }

    /// Returns how many messages were received on this connection since the last time it was cleared
    pub fn received_messages_count(&self) -> u64 {
        self.received_messages_count
    }

    /// Resets all statistics (received/sent bytes and received messages count) to 0
    pub fn reset(&mut self) {
        self.received_bytes_count = 0;
        self.sent_bytes_count = 0;
        self.received_messages_count = 0;
    }

    /// Returns how many bytes were received on this connection since the last time it was cleared and reset this value to 0
    pub fn clear_received_bytes_count(&mut self) -> u64 {
        let bytes_count = self.received_bytes_count;
        self.received_bytes_count = 0;
        bytes_count
    }

    /// Returns how many bytes were received on this connection since the last time it was cleared and reset this value to 0
    pub fn clear_sent_bytes_count(&mut self) -> u64 {
        let bytes_count = self.sent_bytes_count;
        self.sent_bytes_count = 0;
        bytes_count
    }

    /// Returns how many messages were received on this connection since the last time it was cleared and reset this value to 0
    pub fn clear_received_messages_count(&mut self) -> u64 {
        let messages_count = self.received_messages_count;
        self.received_messages_count = 0;
        messages_count
    }

    fn received(&mut self, bytes_count: usize) {
        self.received_bytes_count += bytes_count as u64;
        self.received_messages_count += 1;
    }

    fn sent(&mut self, bytes_count: usize) {
        self.sent_bytes_count += bytes_count as u64;
    }
}
