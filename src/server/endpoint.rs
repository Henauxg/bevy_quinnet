use bevy::{log::error, platform::collections::HashMap};
use bytes::Bytes;
use tokio::sync::{
    broadcast,
    mpsc::{self, error::TryRecvError},
};

use crate::{
    server::{
        connection::ServerConnection, ServerAsyncMessage, ServerDisconnectError,
        ServerGroupPayloadSendError, ServerGroupSendError, ServerPayloadSendError,
        ServerReceiveError, ServerSendError, ServerSyncMessage,
    },
    shared::{
        channels::{Channel, ChannelConfig, ChannelId, CloseReason},
        connection::{ChannelsIdsPool, PeerConnection},
        error::{AsyncChannelError, ChannelCloseError, ChannelCreationError},
        ClientId,
    },
};

/// By default, when starting an [Endpoint], Quinnet creates 1 channel instance of each [ChannelConfig], each with their own [ChannelId].
/// Among those, there is a `default` channel which will be used when you don't specify the channel. At startup, this default channel is a [ChannelConfig::OrderedReliable] channel.
pub struct Endpoint {
    pub(crate) clients: HashMap<ClientId, PeerConnection<ServerConnection>>,
    /// Incremental client id generator
    client_id_gen: ClientId,
    /// Opened send channels types on this endpoint
    opened_channels: HashMap<ChannelId, ChannelConfig>,
    /// Internal ordered pool of available channel ids
    send_channel_ids: ChannelsIdsPool,
    close_sender: broadcast::Sender<()>,
    /// Receiver for internal quinnet messages coming from the async endpoint
    from_async_endpoint_recv: mpsc::Receiver<ServerAsyncMessage>,
    stats: EndpointStats,
    /// If `true`, payloads on receive channels that were not read during this update will be cleared at the end of the update of the sync server, in the [crate::shared::QuinnetSyncPostUpdate] schedule.
    pub clear_stale_received_payloads: bool,
}

impl Endpoint {
    pub(crate) fn new(
        endpoint_close_send: broadcast::Sender<()>,
        from_async_endpoint_recv: mpsc::Receiver<ServerAsyncMessage>,
        clear_stale_received_payloads: bool,
    ) -> Self {
        Self {
            clients: HashMap::new(),
            client_id_gen: 0,
            opened_channels: HashMap::new(),
            send_channel_ids: ChannelsIdsPool::new(),
            close_sender: endpoint_close_send,
            from_async_endpoint_recv,
            stats: EndpointStats::default(),
            clear_stale_received_payloads,
        }
    }

    /// Returns an allocated vector of all the currently connected client ids
    pub fn clients(&self) -> Vec<ClientId> {
        self.clients.keys().cloned().collect()
    }

    /// Attempts to receive a full payload sent by the specified client.
    ///
    /// - Returns an [`Ok`] result containg [`Some`] if there is a message from the client in the message buffer
    /// - Returns an [`Ok`] result containg [`None`] if there is no message from the client in the message buffer
    /// - Can return an [`Err`] if:
    ///  - the client id is not valid
    ///  - the channel id is not valid
    pub fn receive_payload_from<C: Into<ChannelId>>(
        &mut self,
        client_id: ClientId,
        channel_id: C,
    ) -> Result<Option<Bytes>, ServerReceiveError> {
        match self.clients.get_mut(&client_id) {
            Some(connection) => {
                let payload = connection.internal_receive_payload(channel_id.into());
                if payload.is_some() {
                    self.stats.received_messages_count += 1;
                }
                Ok(payload)
            }
            None => Err(ServerReceiveError::UnknownClient(client_id)),
        }
    }

    /// [`Endpoint::receive_payload_from`] that logs the error instead of returning a result.
    pub fn try_receive_payload_from<C: Into<ChannelId>>(
        &mut self,
        client_id: ClientId,
        channel_id: C,
    ) -> Option<Bytes> {
        match self.receive_payload_from(client_id, channel_id.into()) {
            Ok(payload) => payload,
            Err(err) => {
                error!("try_receive_payload: {}", err);
                None
            }
        }
    }

    /// Same as [Endpoint::send_group_payload_on] but on the default channel
    pub fn send_group_payload<'a, I: Iterator<Item = &'a ClientId>, T: Into<Bytes>>(
        &mut self,
        client_ids: I,
        payload: T,
    ) -> Result<(), ServerGroupPayloadSendError> {
        match self.send_channel_ids.default_channel() {
            Some(channel) => self.send_group_payload_on(client_ids, channel, payload),
            None => Err(ServerGroupPayloadSendError::NoDefaultChannel),
        }
    }

    /// [`Endpoint::send_group_payload`] that logs the error instead of returning a result.
    pub fn try_send_group_payload<'a, I: Iterator<Item = &'a ClientId>, T: Into<Bytes>>(
        &mut self,
        client_ids: I,
        payload: T,
    ) {
        if let Err(err) = self.send_group_payload(client_ids, payload) {
            error!("try_send_group_payload: {}", err);
        }
    }

    /// Sends the payload to the specified clients on the specified channel.
    ///
    /// Tries to send to each client before returning.
    ///
    /// Returns an [`Err`] if sending failed for at least 1 client. Information about the failed sendings will be available in the [`ServerGroupPayloadSendError`].
    pub fn send_group_payload_on<
        'a,
        I: Iterator<Item = &'a ClientId>,
        T: Into<Bytes>,
        C: Into<ChannelId>,
    >(
        &mut self,
        client_ids: I,
        channel_id: C,
        payload: T,
    ) -> Result<(), ServerGroupPayloadSendError> {
        let channel_id = channel_id.into();
        let bytes = payload.into();
        let mut errs = vec![];

        for &client_id in client_ids {
            if let Err(e) = self.send_payload_on(client_id, channel_id, bytes.clone()) {
                errs.push((client_id, e.into()));
            }
        }

        match errs.is_empty() {
            true => Ok(()),
            false => Err(ServerGroupSendError(errs).into()),
        }
    }

    /// [`Endpoint::send_group_payload_on`] that logs the error instead of returning a result.
    pub fn try_send_group_payload_on<
        'a,
        I: Iterator<Item = &'a ClientId>,
        T: Into<Bytes>,
        C: Into<ChannelId>,
    >(
        &mut self,
        client_ids: I,
        channel_id: C,
        payload: T,
    ) {
        if let Err(err) = self.send_group_payload_on(client_ids, channel_id, payload) {
            error!("try_send_group_payload_on: {}", err);
        }
    }

    /// Same as [Endpoint::broadcast_payload_on] but on the default channel
    pub fn broadcast_payload<T: Into<Bytes>>(
        &mut self,
        payload: T,
    ) -> Result<(), ServerGroupPayloadSendError> {
        match self.send_channel_ids.default_channel() {
            Some(channel) => Ok(self.broadcast_payload_on(channel, payload)?),
            None => Err(ServerGroupPayloadSendError::NoDefaultChannel),
        }
    }

    /// Sends the payload to all connected clients on the specified channel.
    ///
    /// Tries to send to each client before returning.
    ///
    /// Returns an [`Err`] if sending failed for at least 1 client. Information about the failed sendings will be available in the [`ServerGroupSendError`].
    pub fn broadcast_payload_on<T: Into<Bytes>, C: Into<ChannelId>>(
        &mut self,
        channel_id: C,
        payload: T,
    ) -> Result<(), ServerGroupSendError> {
        let payload: Bytes = payload.into();
        let channel_id = channel_id.into();

        let mut errs = vec![];
        for (&client_id, connection) in self.clients.iter_mut() {
            if let Err(e) = connection.internal_send_payload(channel_id.into(), payload.clone()) {
                errs.push((client_id, e.into()));
            }
        }
        match errs.is_empty() {
            true => Ok(()),
            false => Err(ServerGroupSendError(errs)),
        }
    }

    /// Same as [Endpoint::broadcast_payload] but will log the error instead of returning it
    pub fn try_broadcast_payload<T: Into<Bytes>>(&mut self, payload: T) {
        if let Err(err) = self.broadcast_payload(payload) {
            error!("try_broadcast_payload: {}", err);
        }
    }

    /// Same as [Endpoint::broadcast_payload_on] but will log the error instead of returning it
    pub fn try_broadcast_payload_on<T: Into<Bytes>, C: Into<ChannelId>>(
        &mut self,
        channel_id: C,
        payload: T,
    ) {
        if let Err(err) = self.broadcast_payload_on(channel_id, payload) {
            error!("try_broadcast_payload_on: {}", err);
        }
    }

    /// Same as [Endpoint::send_payload] but on the default channel
    pub fn send_payload<T: Into<Bytes>>(
        &mut self,
        client_id: ClientId,
        payload: T,
    ) -> Result<(), ServerPayloadSendError> {
        match self.send_channel_ids.default_channel() {
            Some(channel) => Ok(self.send_payload_on(client_id, channel, payload)?),
            None => Err(ServerPayloadSendError::NoDefaultChannel.into()),
        }
    }

    /// Sends the payload to the specified client on the specified channel
    ///
    /// Will return an [`Err`] if:
    /// - the channel does not exist/is closed
    /// - or if the client is disconnected
    /// - (or if the message queue is full)
    pub fn send_payload_on<T: Into<Bytes>, C: Into<ChannelId>>(
        &mut self,
        client_id: ClientId,
        channel_id: C,
        payload: T,
    ) -> Result<(), ServerSendError> {
        if let Some(client_connection) = self.clients.get_mut(&client_id) {
            Ok(client_connection.internal_send_payload(channel_id.into(), payload.into())?)
        } else {
            Err(ServerSendError::UnknownClient(client_id))
        }
    }

    /// Same as [Endpoint::send_payload] but will log the error instead of returning it
    pub fn try_send_payload<T: Into<Bytes>>(&mut self, client_id: ClientId, payload: T) {
        match self.send_payload(client_id, payload) {
            Ok(_) => {}
            Err(err) => error!("try_send_payload: {}", err),
        }
    }

    /// Same as [Endpoint::send_payload_on] but will log the error instead of returning it
    pub fn try_send_payload_on<T: Into<Bytes>, C: Into<ChannelId>>(
        &mut self,
        client_id: ClientId,
        channel_id: C,
        payload: T,
    ) {
        match self.send_payload_on(client_id, channel_id, payload) {
            Ok(_) => {}
            Err(err) => error!("try_send_payload_on: {}", err),
        }
    }

    fn internal_disconnect_client(
        &mut self,
        client_id: ClientId,
        reason: CloseReason,
    ) -> Result<(), ServerDisconnectError> {
        match self.clients.remove(&client_id) {
            Some(mut client_connection) => {
                self.stats.disconnect_count += 1;
                client_connection
                    .close(reason)
                    .map_err(|_| ServerDisconnectError::ClientAlreadyDisconnected(client_id))
            }
            None => Err(ServerDisconnectError::UnknownClient(client_id)),
        }
    }

    /// Logical "Disconnect", the client already closed/lost the connection.
    pub(crate) fn try_disconnect_closed_client(&mut self, client_id: ClientId) {
        if let Err(err) = self.internal_disconnect_client(client_id, CloseReason::PeerClosed) {
            error!(
                "Failed to properly disconnect client {}: {}",
                client_id, err
            );
        }
    }

    /// Disconnect a specific client. Removes it from the server.
    ///
    /// Disconnecting a client immediately prevents new messages from being sent on its connection and signal the underlying connection to closes all its background tasks. Before trully closing, the connection will wait for all buffered messages in all its opened channels to be properly sent according to their respective channel type.
    ///
    /// This may fail if no client if found for client_id, or if the client is already disconnected.
    pub fn disconnect_client(&mut self, client_id: ClientId) -> Result<(), ServerDisconnectError> {
        self.internal_disconnect_client(client_id, CloseReason::LocalOrder)
    }

    /// Same as [Endpoint::disconnect_client] but errors are logged instead of returned
    pub fn try_disconnect_client(&mut self, client_id: ClientId) {
        if let Err(err) = self.disconnect_client(client_id) {
            error!(
                "Failed to properly disconnect client {}: {}",
                client_id, err
            );
        }
    }

    /// Disconnects all connect clients
    pub fn disconnect_all_clients(&mut self) {
        for (_, mut client_connection) in self.clients.drain() {
            let _ = client_connection.close(CloseReason::LocalOrder);
        }
    }

    /// Returns statistics about a client if connected.
    pub fn get_connection_stats(&self, client_id: ClientId) -> Option<quinn::ConnectionStats> {
        match &self.clients.get(&client_id) {
            Some(client) => Some(client.connection_stats()),
            None => None,
        }
    }

    /// Returns a mutable reference to a client connection if it exists
    pub fn connection_mut(
        &mut self,
        client_id: ClientId,
    ) -> Option<&mut PeerConnection<ServerConnection>> {
        match self.clients.get_mut(&client_id) {
            Some(client_connection) => Some(client_connection),
            None => None,
        }
    }

    /// Returns a reference to a client connection if it exists
    pub fn connection(&self, client_id: ClientId) -> Option<&PeerConnection<ServerConnection>> {
        match self.clients.get(&client_id) {
            Some(client_connection) => Some(client_connection),
            None => None,
        }
    }

    /// Returns statistics about the server's endpoint
    pub fn endpoint_stats(&self) -> &EndpointStats {
        &self.stats
    }

    /// Opens a channel of the requested [ChannelConfig] and returns its [ChannelId].
    ///
    /// If no channels were previously opened, the opened channel will be the new default channel.
    ///
    /// Can fail if the Endpoint is closed or if too many channels are already opened.
    pub fn open_channel(
        &mut self,
        channel_type: ChannelConfig,
    ) -> Result<ChannelId, ChannelCreationError> {
        let channel_id = self.send_channel_ids.take_id()?;
        Ok(self.create_endpoint_channel(channel_id, channel_type)?)
    }

    /// Assumes presence of available channel ids
    pub(crate) fn unchecked_open_channel(
        &mut self,
        channel_type: ChannelConfig,
    ) -> Result<ChannelId, AsyncChannelError> {
        let channel_id = self.send_channel_ids.take_id().unwrap();
        self.create_endpoint_channel(channel_id, channel_type)
    }

    /// `channel_id` must be an available [ChannelId]
    fn create_endpoint_channel(
        &mut self,
        channel_id: ChannelId,
        channel_type: ChannelConfig,
    ) -> Result<ChannelId, AsyncChannelError> {
        let unregistered_channels =
            match self.create_unregistered_endpoint_channels(channel_id, channel_type) {
                Ok(channels) => channels,
                Err(err) => {
                    self.send_channel_ids.release_id(channel_id);
                    return Err(err);
                }
            };
        // Only commit the changes once all channels have been confirmed to be created.
        for (client_id, channel) in unregistered_channels {
            self.clients
                .get_mut(&client_id)
                .unwrap()
                .register_connection_channel(channel);
        }
        self.opened_channels.insert(channel_id, channel_type);
        Ok(channel_id)
    }

    fn create_unregistered_endpoint_channels(
        &mut self,
        channel_id: ChannelId,
        channel_type: ChannelConfig,
    ) -> Result<HashMap<ClientId, Channel>, AsyncChannelError> {
        let mut unregistered_channels = HashMap::new();
        for (&client_id, client_connection) in self.clients.iter_mut() {
            // Unregistered channels are dropped here on error, created async tasks are closing too.
            let channel = client_connection
                .create_unregistered_connection_channel(channel_id, channel_type)?;
            unregistered_channels.insert(client_id, channel);
        }
        Ok(unregistered_channels)
    }

    /// Closes the channel with the corresponding [ChannelId].
    ///
    /// No new messages will be able to be sent on this channel, however, the channel will properly try to send all the messages that were previously pushed to it, according to its [ChannelConfig], before fully closing.
    ///
    /// If the closed channel is the current default channel, the default channel gets set to `None`.
    ///
    /// Can fail if the [ChannelId] is unknown, or if the channel is already closed.
    pub fn close_channel(&mut self, channel_id: ChannelId) -> Result<(), ChannelCloseError> {
        match self.opened_channels.remove(&channel_id) {
            Some(_) => {
                for (_, connection) in self.clients.iter_mut() {
                    connection.internal_close_channel(channel_id)?;
                }
                self.send_channel_ids.release_id(channel_id);
                Ok(())
            }
            None => Err(ChannelCloseError::InvalidChannelId(channel_id)),
        }
    }

    /// Set the default channel via its [ChannelId]
    #[inline(always)]
    pub fn set_default_channel(&mut self, channel_id: ChannelId) {
        self.send_channel_ids.set_default_channel(channel_id);
    }

    /// Get the default [ChannelId]
    #[inline(always)]
    pub fn default_channel(&self) -> Option<ChannelId> {
        self.send_channel_ids.default_channel()
    }

    pub(crate) fn close_incoming_connections_handler(&mut self) -> Result<(), AsyncChannelError> {
        match self.close_sender.send(()) {
            Ok(_) => Ok(()),
            // Connections handler is already closed
            Err(_) => Err(AsyncChannelError::InternalChannelClosed),
        }
    }

    pub(crate) fn handle_new_connection(
        &mut self,
        mut connection: PeerConnection<ServerConnection>,
    ) -> Result<ClientId, AsyncChannelError> {
        for (channel_id, channel_type) in self.opened_channels.iter() {
            if let Err(err) = connection.create_connection_channel(*channel_id, *channel_type) {
                connection.try_close(CloseReason::LocalOrder);
                return Err(err);
            };
        }

        self.client_id_gen += 1;
        let client_id = self.client_id_gen;

        match connection
            .try_send_to_async_connection(ServerSyncMessage::ClientConnectedAck(client_id))
        {
            Ok(_) => {
                self.clients.insert(client_id, connection);
                self.stats.connect_count += 1;
                Ok(client_id)
            }
            Err(_) => {
                connection.try_close(CloseReason::LocalOrder);
                Err(AsyncChannelError::InternalChannelClosed)
            }
        }
    }

    pub(crate) fn try_recv_from_async(&mut self) -> Result<ServerAsyncMessage, TryRecvError> {
        self.from_async_endpoint_recv.try_recv()
    }

    pub(crate) fn dispatch_received_payloads(&mut self) {
        for connection in self.clients.values_mut() {
            connection.dispatch_received_payloads_to_channel_buffers();
        }
    }

    pub(crate) fn clear_stale_payloads_from_clients(&mut self) {
        for connection in self.clients.values_mut() {
            connection.internal_clear_stale_received_payloads();
        }
    }
}

/// Basic quinnet stats about this server endpoint
#[derive(Default)]
pub struct EndpointStats {
    received_messages_count: u64,
    connect_count: u32,
    disconnect_count: u32,
}
impl EndpointStats {
    /// Returns how many messages were received (read) on this endpoint
    pub fn received_messages_count(&self) -> u64 {
        self.received_messages_count
    }
    /// Returns how many connection events occurred on this endpoint
    pub fn connect_count(&self) -> u32 {
        self.connect_count
    }
    /// Returns how many disconnections events occurred on this endpoint
    pub fn disconnect_count(&self) -> u32 {
        self.disconnect_count
    }
}
