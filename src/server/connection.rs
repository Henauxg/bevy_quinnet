use std::net::SocketAddr;

use tokio::sync::mpsc::{self, error::TrySendError};

use crate::{
    server::ServerSyncMessage,
    shared::{peer_connection::PeerConnection, InternalConnectionRef},
};

/// A connection to a client from the server's perspective.
pub type ServerSideConnection = PeerConnection<ServerConnection>;

/// Specific data for a server-side connection
pub struct ServerConnection {
    connection_handle: InternalConnectionRef,
    to_connection_send: mpsc::Sender<ServerSyncMessage>,
}
impl ServerConnection {
    pub(crate) fn new(
        connection_handle: InternalConnectionRef,
        to_connection_send: mpsc::Sender<ServerSyncMessage>,
    ) -> Self {
        Self {
            connection_handle,
            to_connection_send,
        }
    }
}

impl ServerSideConnection {
    /// See [quinn::Connection::max_datagram_size]
    #[inline(always)]
    pub fn max_datagram_size(&self) -> Option<usize> {
        self.specific.connection_handle.max_datagram_size()
    }

    /// Returns statistics about a client connection
    #[inline(always)]
    pub fn quinn_connection_stats(&self) -> quinn::ConnectionStats {
        self.specific.connection_handle.stats()
    }

    #[inline(always)]
    pub(crate) fn try_send_to_async_connection(
        &self,
        msg: ServerSyncMessage,
    ) -> Result<(), TrySendError<ServerSyncMessage>> {
        self.specific.to_connection_send.try_send(msg)
    }

    /// Returns the remote IP address of the client.
    #[inline(always)]
    pub fn remote_addr(&self) -> SocketAddr {
        self.specific.connection_handle.remote_address()
    }
}
