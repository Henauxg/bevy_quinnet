use std::collections::VecDeque;

use bytes::Bytes;

use crate::shared::{
    channels::{ChannelId, MAX_CHANNEL_COUNT},
    error::RecvChannelError,
    peer_connection::PeerConnection,
};

/// Default value for the `max_buffered_payloads_count_per_channel` field of a [`RecvChannelsConfiguration`]
pub const DEFAULT_MAX_BUFFERED_PAYLOADS_COUNT_PER_CHANNEL: usize = 512;
/// Default value for the `max_receive_channels_count` field of a [`RecvChannelsConfiguration`]
pub const DEFAULT_MAX_RECEIVE_CHANNEL_COUNT: usize = MAX_CHANNEL_COUNT;
/// Default value for the `clear_stale_payloads` fields
pub const DEFAULT_CLEAR_STALE_RECEIVED_PAYLOADS: bool = false;

/// Configuration for a [PeerConnection].
///
/// See [crate::server::connection::ServerSideConnection] and [crate::client::connection::ClientSideConnection].
#[derive(Debug, Clone)]
pub struct RecvChannelsConfiguration {
    /// Maximum number of payloads that can be buffered per receive channel.
    pub max_buffered_payloads_count_per_channel: usize,
    /// Maximum number of receive channels that can be opened on this connection.
    pub max_receive_channels_count: usize,
    /// If `true`, payloads on receive channels that were not read during this update will be cleared at the end of an Update cycle, in the [crate::shared::QuinnetSyncLast] schedule.
    ///
    /// Defaults to [DEFAULT_CLEAR_STALE_RECEIVED_PAYLOADS].
    pub clear_stale_received_payloads: bool,
}
impl Default for RecvChannelsConfiguration {
    fn default() -> Self {
        Self {
            max_buffered_payloads_count_per_channel:
                DEFAULT_MAX_BUFFERED_PAYLOADS_COUNT_PER_CHANNEL,
            max_receive_channels_count: DEFAULT_MAX_RECEIVE_CHANNEL_COUNT,
            clear_stale_received_payloads: DEFAULT_CLEAR_STALE_RECEIVED_PAYLOADS,
        }
    }
}

impl<S> PeerConnection<S> {
    /// Enables or disables [`RecvChannelsConfiguration::clear_stale_received_payloads`] on this connection.
    #[inline(always)]
    pub fn set_clear_stale_received_payloads(&mut self, enable: bool) {
        self.recv_channels_cfg.clear_stale_received_payloads = enable;
    }

    /// Buffers of received payloads per receive channel
    pub(crate) fn internal_receive_payload(&mut self, channel_id: ChannelId) -> Option<Bytes> {
        match self.recv_channels_payloads.get_mut(channel_id as usize) {
            Some(payloads) => payloads.pop_front(),
            None => None,
        }
    }

    pub(crate) fn clear_stale_received_payloads(&mut self) {
        if self.recv_channels_cfg.clear_stale_received_payloads {
            self.clear_received_payloads();
        }
    }

    /// Clears all the received payloads buffers for this connection.
    pub fn clear_received_payloads(&mut self) {
        for payloads in self.recv_channels_payloads.iter_mut() {
            payloads.clear();
        }
    }

    pub(crate) fn dispatch_received_payloads_to_channel_buffers(
        &mut self,
    ) -> Result<(), Vec<RecvChannelError>> {
        // Note on handling of TryRecvError::Disconnected
        // This error means that the receiving end of the channel is closed, which only happens when the client connection is closed/closing.
        // In this case we decide to consider that there is no more messages to receive.
        let mut errs = Vec::new();
        while let Ok((channel_id, payload)) = self.dequeue_undispatched_bytes_from_peer() {
            match self.recv_channels_payloads.get_mut(channel_id as usize) {
                Some(payloads) => {
                    if payloads.len()
                        < self
                            .recv_channels_cfg
                            .max_buffered_payloads_count_per_channel
                    {
                        payloads.push_back(payload);
                    } else {
                        errs.push(RecvChannelError::RecvChannelFull(channel_id));
                    }
                }
                None => {
                    if (channel_id as usize) < self.recv_channels_cfg.max_receive_channels_count {
                        self.recv_channels_payloads.extend(
                            (self.recv_channels_payloads.len()..channel_id as usize)
                                .map(|_| VecDeque::new()),
                        );
                        self.recv_channels_payloads.push(VecDeque::from([payload]));
                    } else {
                        errs.push(RecvChannelError::MaxRecvChannelCountReached(channel_id));
                    }
                }
            }
        }
        match errs.is_empty() {
            true => Ok(()),
            false => Err(errs),
        }
    }
}
