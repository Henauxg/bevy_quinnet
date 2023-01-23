use super::QuinnetError;
use bevy::prelude::{error, trace};
use bytes::Bytes;
use futures::{sink::SinkExt, StreamExt};
use quinn::{RecvStream, SendDatagramError, SendStream, VarInt};
use serde::{Deserialize, Serialize};
use std::fmt::{Debug, Display};
use tokio::sync::{
    broadcast,
    mpsc::{self, error::TrySendError},
};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

const DEFAULT_MAX_DATAGRAM_SIZE: usize = 1250;
const UNRELIABLE_HEADER_MARGIN: usize = 4;

pub(crate) type MultiChannelId = u16;

#[derive(Debug, Copy, Clone)]
pub enum ChannelType {
    /// An OrderedReliable channel ensures that messages sent are delivered, and are processed by the receiving end in the same order as they were sent.
    OrderedReliable,
    /// An UnorderedReliable channel ensures that messages sent are delivered, but they may be delivered out of order.
    UnorderedReliable,
    /// Channel which transmits messages as unreliable and unordered datagrams (may be lost or delivered out of order).
    ///
    /// The maximum allowed size of a datagram may change over the lifetime of a connection according to variation in the path MTU estimate. This is guaranteed to be a little over a kilobyte at minimum.
    Unreliable,
    UnreliableSequenced,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum ChannelId {
    /// There may be more than one OrderedReliable channel instance. This may be useful to avoid some Head of line blocking (<https://en.wikipedia.org/wiki/Head-of-line_blocking>) issues.
    ///
    /// See [ChannelType::OrderedReliable] for more information.
    OrderedReliable(MultiChannelId),
    /// One `UnorderedReliable` channel instance is enough since messages are not ordered on those, in fact even if you tried to create more, Quinnet would just reuse the existing one. This is why you can directly use this [ChannelId::UnorderedReliable] when sending messages.
    ///
    /// See [ChannelType::UnorderedReliable] for more information.
    UnorderedReliable,
    /// One `Unreliable` channel instance is enough since messages are not ordered on those, in fact even if you tried to create more, Quinnet would just reuse the existing one. This is why you can directly use this [ChannelId::Unreliable] when sending messages.
    ///
    /// See [ChannelType::Unreliable] for more information.
    Unreliable,
    UnreliableSequenced(MultiChannelId),
}

impl std::fmt::Display for ChannelId {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Serialize, Deserialize)]
enum DatagramHeader {
    FirstFragmentSequenced {
        id: u16,
        channel_id: MultiChannelId,
        frag_count: u8,
    },
    FragmentSequenced {
        id: u16,
        channel_id: MultiChannelId,
        fragment: u8,
    },
    FirstFragmentNotSequenced {
        id: u16,
        frag_count: u8,
    },
    FragmentNotSequenced {
        id: u16,
        fragment: u8,
    },
    CompleteSequenced {
        id: u16,
        channel_id: MultiChannelId,
    },
    CompleteNotSequenced,
}

enum DatagramHeaderType {
    FirstFrag(u8),
    Frag(u8),
    Complete,
}

#[derive(Serialize, Deserialize)]
struct Datagram {
    header: DatagramHeader,
    payload: Vec<u8>,
}

#[derive(Debug)]
pub(crate) enum ChannelAsyncMessage {
    LostConnection,
}

#[derive(Debug)]
pub(crate) enum ChannelSyncMessage {
    CreateChannel {
        channel_id: ChannelId,
        bytes_to_channel_recv: mpsc::Receiver<Bytes>,
        channel_close_recv: mpsc::Receiver<()>,
    },
}

#[derive(Debug)]
pub(crate) struct Channel {
    sender: mpsc::Sender<Bytes>,
    close_sender: mpsc::Sender<()>,
}

impl Channel {
    pub(crate) fn new(sender: mpsc::Sender<Bytes>, close_sender: mpsc::Sender<()>) -> Self {
        Self {
            sender,
            close_sender,
        }
    }

    pub(crate) fn send_payload(&self, payload: Bytes) -> Result<(), QuinnetError> {
        match self.sender.try_send(payload) {
            Ok(_) => Ok(()),
            Err(err) => match err {
                TrySendError::Full(_) => Err(QuinnetError::FullQueue),
                TrySendError::Closed(_) => Err(QuinnetError::InternalChannelClosed),
            },
        }
    }

    pub(crate) fn close(&self) -> Result<(), QuinnetError> {
        match self.close_sender.blocking_send(()) {
            Ok(_) => Ok(()),
            Err(_) => {
                // The only possible error for a send is that there is no active receivers, meaning that the tasks are already terminated.
                Err(QuinnetError::ChannelAlreadyClosed)
            }
        }
    }
}

pub(crate) fn get_channel_id_from_type<F>(
    channel_type: ChannelType,
    mut multi_id_generator: F,
) -> ChannelId
where
    F: FnMut() -> MultiChannelId,
{
    match channel_type {
        ChannelType::OrderedReliable => ChannelId::OrderedReliable(multi_id_generator()),
        ChannelType::UnorderedReliable => ChannelId::UnorderedReliable,
        ChannelType::Unreliable => ChannelId::Unreliable,
        ChannelType::UnreliableSequenced => ChannelId::UnreliableSequenced(multi_id_generator()),
    }
}

pub(crate) async fn channels_task(
    connection: quinn::Connection,
    mut close_recv: broadcast::Receiver<()>,
    mut to_channels_recv: mpsc::Receiver<ChannelSyncMessage>,
    from_channels_send: mpsc::Sender<ChannelAsyncMessage>,
) {
    // Use an mpsc channel where, instead of sending messages, we wait for the channel to be closed, which happens when every sender has been dropped. We can't use a JoinSet as simply here since we would also need to drain closed channels from it.
    let (channel_tasks_keepalive, mut channel_tasks_waiter) = mpsc::channel::<()>(1);

    let close_receiver_clone = close_recv.resubscribe();
    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Connection Channels listener received a close signal")
        }
        _ = async {
            while let Some(sync_message) = to_channels_recv.recv().await {
                let ChannelSyncMessage::CreateChannel{ channel_id,  bytes_to_channel_recv, channel_close_recv } = sync_message;

                let close_receiver = close_receiver_clone.resubscribe();
                let connection_handle = connection.clone();
                let from_channels_send = from_channels_send.clone();
                let channels_keepalive_clone = channel_tasks_keepalive.clone();

                match channel_id {
                    ChannelId::OrderedReliable(_) => {
                        tokio::spawn(async move {
                            ordered_reliable_channel_task(
                                connection_handle,
                                channels_keepalive_clone,
                                from_channels_send,
                                close_receiver,
                                channel_close_recv,
                                bytes_to_channel_recv
                            )
                            .await
                        });
                    },
                    ChannelId::UnorderedReliable => {
                        tokio::spawn(async move {
                            unordered_reliable_channel_task(
                                connection_handle,
                                channels_keepalive_clone,
                                from_channels_send,
                                close_receiver,
                                channel_close_recv,
                                bytes_to_channel_recv
                            )
                            .await
                        });
                    },
                    ChannelId::Unreliable => {
                        tokio::spawn(async move {
                            unreliable_channel_task(
                                connection_handle,
                                channels_keepalive_clone,
                                from_channels_send,
                                close_receiver,
                                channel_close_recv,
                                bytes_to_channel_recv
                            )
                            .await
                        });
                    },
                    ChannelId::UnreliableSequenced(internal_channel_id) => {
                        tokio::spawn(async move {
                            sequenced_unreliable_channel_task(
                                connection_handle,
                                internal_channel_id,
                                channels_keepalive_clone,
                                from_channels_send,
                                close_receiver,
                                channel_close_recv,
                                bytes_to_channel_recv
                            )
                            .await
                        });
                    },
                }
            }
        } => {
            trace!("Connection Channels listener ended")
        }
    };

    // Wait for all the channels to have flushed/finished:
    // We drop our sender first because the recv() call otherwise sleeps forever.
    // When every sender has gone out of scope, the recv call will return with an error. We ignore the error.
    drop(channel_tasks_keepalive);
    let _ = channel_tasks_waiter.recv().await;

    connection.close(VarInt::from_u32(0), "closed".as_bytes());
}

pub(crate) async fn ordered_reliable_channel_task(
    connection: quinn::Connection,
    _: mpsc::Sender<()>,
    from_channels_send: mpsc::Sender<ChannelAsyncMessage>,
    mut close_recv: broadcast::Receiver<()>,
    mut channel_close_recv: mpsc::Receiver<()>,
    mut bytes_to_channel_recv: mpsc::Receiver<Bytes>,
) {
    let mut frame_sender = new_uni_frame_sender(&connection).await;

    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Ordered Reliable Channel task received a close signal")
        }
        _ = channel_close_recv.recv() => {
            trace!("Ordered Reliable Channel task received a channel close signal")
        }
        _ = async {
            while let Some(msg_bytes) = bytes_to_channel_recv.recv().await {
                if let Err(err) = frame_sender.send(msg_bytes).await {
                    error!("Error while sending on Ordered Reliable Channel, {}", err);
                    from_channels_send.send(
                        ChannelAsyncMessage::LostConnection)
                        .await
                        .expect("Failed to signal connection lost on Ordered Reliable Channel");
                }
            }
        } => {
            trace!("Ordered Reliable Channel task ended")
        }
    };
    while let Ok(msg_bytes) = bytes_to_channel_recv.try_recv() {
        if let Err(err) = frame_sender.send(msg_bytes).await {
            error!(
                "Error while sending a remaining message on Ordered Reliable Channel, {}",
                err
            );
        }
    }
    if let Err(err) = frame_sender.flush().await {
        error!(
            "Error while flushing Ordered Reliable Channel stream: {}",
            err
        );
    }
    if let Err(err) = frame_sender.into_inner().finish().await {
        error!(
            "Failed to shutdown Ordered Reliable Channel stream gracefully: {}",
            err
        );
    }
}

pub(crate) async fn unordered_reliable_channel_task(
    connection: quinn::Connection,
    channel_tasks_keepalive: mpsc::Sender<()>,
    from_channels_send: mpsc::Sender<ChannelAsyncMessage>,
    mut close_recv: broadcast::Receiver<()>,
    mut channel_close_recv: mpsc::Receiver<()>,
    mut bytes_to_channel_recv: mpsc::Receiver<Bytes>,
) {
    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Unordered Reliable Channel task received a close signal")
        }
        _ = channel_close_recv.recv() => {
            trace!("Unordered Reliable Channel task received a channel close signal")
        }
        _ = async {
            while let Some(msg_bytes) = bytes_to_channel_recv.recv().await {
                let conn = connection.clone();
                let from_channels_send_clone = from_channels_send.clone();
                let channels_keepalive_clone = channel_tasks_keepalive.clone();
                tokio::spawn(async move {
                    let mut frame_sender = new_uni_frame_sender(&conn).await;
                    if let Err(err) = frame_sender.send(msg_bytes).await {
                        error!("Error while sending on Unordered Reliable Channel, {}", err);
                        from_channels_send_clone.send(
                            ChannelAsyncMessage::LostConnection)
                            .await
                            .expect("Failed to signal connection lost on Unordered Reliable Channel");
                    }
                    if let Err(err) = frame_sender.into_inner().finish().await {
                        error!("Failed to shutdown Unordered Reliable Channel stream gracefully: {}", err);
                    }
                    drop(channels_keepalive_clone)
                });
            }
        } => {
            trace!("Unordered Reliable Channel task ended")
        }
    };
    while let Ok(msg_bytes) = bytes_to_channel_recv.try_recv() {
        let conn = connection.clone();
        let channels_keepalive_clone = channel_tasks_keepalive.clone();
        tokio::spawn(async move {
            let mut frame_sender = new_uni_frame_sender(&conn).await;
            if let Err(err) = frame_sender.send(msg_bytes).await {
                error!(
                    "Error while sending a remaining message on Unordered Reliable Channel, {}",
                    err
                );
            }
            if let Err(err) = frame_sender.into_inner().finish().await {
                error!(
                    "Failed to shutdown Unordered Reliable Channel stream gracefully: {}",
                    err
                );
            }
            drop(channels_keepalive_clone)
        });
    }
}

async fn send_datagram(
    datagram: Datagram,
    connection: &quinn::Connection,
    from_channels_send: &mpsc::Sender<ChannelAsyncMessage>,
    signal_connection_lost: bool,
) -> Option<usize> {
    match bincode::serialize(&datagram) {
        Ok(datagram_bytes) => {
            if let Err(err) = connection.send_datagram(datagram_bytes.into()) {
                error!("Error while sending message on Unreliable Channel, {}", err);
                match err {
                    SendDatagramError::UnsupportedByPeer => (),
                    SendDatagramError::Disabled => (),
                    SendDatagramError::TooLarge => {
                        let max_payload_size = match connection.max_datagram_size() {
                            Some(max) => max - UNRELIABLE_HEADER_MARGIN,
                            None => DEFAULT_MAX_DATAGRAM_SIZE,
                        };
                        return Some(max_payload_size);
                    }
                    SendDatagramError::ConnectionLost(_) => {
                        if signal_connection_lost {
                            from_channels_send
                                .send(ChannelAsyncMessage::LostConnection)
                                .await
                                .expect("Failed to signal connection lost from channels");
                        }
                    }
                }
            }
        }
        Err(err) => error!("Failed to serialize datagram: {}", err),
    }
    None
}

fn fragment_message<F>(
    msg_bytes: Bytes,
    max_payload_size: usize,
    header_builder: F,
) -> (Option<Vec<Datagram>>, usize)
where
    F: Fn(DatagramHeaderType) -> DatagramHeader,
{
    let total_payload_size = msg_bytes.len();
    if total_payload_size > max_payload_size {
        let partial_fragment = if msg_bytes.len() % max_payload_size == 0 {
            0
        } else {
            1
        };
        let frag_count = msg_bytes.len() / max_payload_size + partial_fragment;

        if frag_count > std::u8::MAX as usize {
            return (None, total_payload_size);
        }

        let mut frags = Vec::with_capacity(frag_count);
        for (frag_id, frag_payload) in msg_bytes.chunks(max_payload_size).enumerate() {
            let header = match frag_id {
                1 => header_builder(DatagramHeaderType::FirstFrag(frag_count as u8)),
                _ => header_builder(DatagramHeaderType::Frag(frag_id as u8)),
            };
            frags.push(Datagram {
                header,
                payload: frag_payload.into(),
            });
        }
        (Some(frags), total_payload_size)
    } else {
        (
            Some(vec![Datagram {
                header: header_builder(DatagramHeaderType::Complete),
                payload: msg_bytes.into(),
            }]),
            total_payload_size,
        )
    }
}

async fn send_unreliable_message<F>(
    connection: &quinn::Connection,
    from_channels_send: &mpsc::Sender<ChannelAsyncMessage>,
    msg_bytes: Bytes,
    max_payload_size: &mut usize,
    header_builder: F,
    signal_connection_lost: bool,
) where
    F: Fn(DatagramHeaderType) -> DatagramHeader,
{
    let (fragments, total_size) = fragment_message(msg_bytes, *max_payload_size, header_builder);

    match fragments {
        Some(fragments) => {
            for frag in fragments {
                *max_payload_size = send_datagram(
                    frag,
                    &connection,
                    &from_channels_send,
                    signal_connection_lost,
                )
                .await
                .unwrap_or(*max_payload_size);
            }
        }
        None => error!(
            "Unable to fragment, unreliable message is too large: {}",
            total_size
        ),
    }
}

fn build_datagram_header(header_type: DatagramHeaderType, msg_id: u16) -> DatagramHeader {
    match header_type {
        DatagramHeaderType::FirstFrag(frag_count) => DatagramHeader::FirstFragmentNotSequenced {
            id: msg_id,
            frag_count,
        },
        DatagramHeaderType::Frag(fragment) => DatagramHeader::FragmentNotSequenced {
            id: msg_id,
            fragment,
        },
        DatagramHeaderType::Complete => DatagramHeader::CompleteNotSequenced,
    }
}

pub(crate) async fn unreliable_channel_task(
    connection: quinn::Connection,
    _: mpsc::Sender<()>,
    from_channels_send: mpsc::Sender<ChannelAsyncMessage>,
    mut close_recv: broadcast::Receiver<()>,
    mut channel_close_recv: mpsc::Receiver<()>,
    mut bytes_to_channel_recv: mpsc::Receiver<Bytes>,
) {
    let mut max_payload_size = connection
        .max_datagram_size()
        .unwrap_or(DEFAULT_MAX_DATAGRAM_SIZE);
    let mut msg_id: u16 = 0;

    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Unreliable Channel task received a close signal")
        }
        _ = channel_close_recv.recv() => {
            trace!("Unreliable Channel task received a channel close signal")
        }
        _ = async {
            while let Some(msg_bytes) = bytes_to_channel_recv.recv().await {
                send_unreliable_message(
                    &connection,
                    &from_channels_send,
                    msg_bytes,
                    &mut max_payload_size,
                    |header_type| build_datagram_header(header_type, msg_id),
                    true
                ).await;
                msg_id += 1;
            }
        } => {
            trace!("Unreliable Channel task ended")
        }
    };
    while let Ok(msg_bytes) = bytes_to_channel_recv.try_recv() {
        send_unreliable_message(
            &connection,
            &from_channels_send,
            msg_bytes,
            &mut max_payload_size,
            |header_type| build_datagram_header(header_type, msg_id),
            false,
        )
        .await;
        msg_id += 1;
    }
}

fn build_sequenced_datagram_header(
    header_type: DatagramHeaderType,
    channel_id: MultiChannelId,
    msg_id: u16,
) -> DatagramHeader {
    match header_type {
        DatagramHeaderType::FirstFrag(frag_count) => DatagramHeader::FirstFragmentSequenced {
            id: msg_id,
            channel_id,
            frag_count,
        },
        DatagramHeaderType::Frag(fragment) => DatagramHeader::FragmentSequenced {
            id: msg_id,
            channel_id,
            fragment,
        },
        DatagramHeaderType::Complete => DatagramHeader::CompleteSequenced {
            id: msg_id,
            channel_id,
        },
    }
}

pub(crate) async fn sequenced_unreliable_channel_task(
    connection: quinn::Connection,
    channel_id: MultiChannelId,
    _: mpsc::Sender<()>,
    from_channels_send: mpsc::Sender<ChannelAsyncMessage>,
    mut close_recv: broadcast::Receiver<()>,
    mut channel_close_recv: mpsc::Receiver<()>,
    mut bytes_to_channel_recv: mpsc::Receiver<Bytes>,
) {
    let mut msg_id: u16 = 0;
    let mut max_payload_size = connection
        .max_datagram_size()
        .unwrap_or(DEFAULT_MAX_DATAGRAM_SIZE);

    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Unreliable Channel task received a close signal")
        }
        _ = channel_close_recv.recv() => {
            trace!("Unreliable Channel task received a channel close signal")
        }
        _ = async {
            while let Some(msg_bytes) = bytes_to_channel_recv.recv().await {
                send_unreliable_message(
                    &connection,
                    &from_channels_send,
                    msg_bytes,
                    &mut max_payload_size,
                    |header_type| build_sequenced_datagram_header(header_type,channel_id, msg_id),
                    true
                ).await;
                msg_id += 1;
            }
        } => {
            trace!("Unreliable Channel task ended")
        }
    };
    while let Ok(msg_bytes) = bytes_to_channel_recv.try_recv() {
        send_unreliable_message(
            &connection,
            &from_channels_send,
            msg_bytes,
            &mut max_payload_size,
            |header_type| build_sequenced_datagram_header(header_type, channel_id, msg_id),
            false,
        )
        .await;
        msg_id += 1;
    }
}

async fn new_uni_frame_sender(
    connection: &quinn::Connection,
) -> FramedWrite<SendStream, LengthDelimitedCodec> {
    let uni_sender = connection
        .open_uni()
        .await
        .expect("Failed to open send stream");
    FramedWrite::new(uni_sender, LengthDelimitedCodec::new())
}

async fn uni_receiver_task(
    recv: RecvStream,
    mut close_recv: broadcast::Receiver<()>,
    bytes_from_server_send: mpsc::Sender<Bytes>,
) {
    tokio::select! {
        _ = close_recv.recv() => {}
        _ = async {
            let mut frame_recv = FramedRead::new(recv, LengthDelimitedCodec::new());
            while let Some(Ok(msg_bytes)) = frame_recv.next().await {
                // TODO Clean: error handling
                bytes_from_server_send.send(msg_bytes.into()).await.unwrap();
            }
        } => {}
    };
}

pub(crate) async fn reliable_receiver_task<T: Display>(
    id: T,
    connection: quinn::Connection,
    mut close_recv: broadcast::Receiver<()>,
    bytes_incoming_send: mpsc::Sender<Bytes>,
) {
    let close_recv_clone = close_recv.resubscribe();
    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Listener for new Unidirectional Receiving Streams with id {} received a close signal", id)
        }
        _ = async {
            while let Ok(recv) = connection.accept_uni().await {
                let bytes_from_server_send = bytes_incoming_send.clone();
                let close_recv_clone = close_recv_clone.resubscribe();
                tokio::spawn(async move {
                    uni_receiver_task(
                        recv,
                        close_recv_clone,
                        bytes_from_server_send
                    ).await;
                });
            }
        } => {
            trace!("Listener for new Unidirectional Receiving Streams with id {} ended", id)
        }
    };
}

pub(crate) async fn unreliable_receiver_task<T: Display>(
    id: T,
    connection: quinn::Connection,
    mut close_recv: broadcast::Receiver<()>,
    bytes_incoming_send: mpsc::Sender<Bytes>,
) {
    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Listener for unreliable datagrams with id {} received a close signal", id)
        }
        _ = async {
            while let Ok(msg_bytes) = connection.read_datagram().await {
                match bincode::deserialize::<Datagram>(&msg_bytes) {
                    Ok(fragment) => {
                        match fragment.header {
                            DatagramHeader::FirstFragmentSequenced { id, channel_id, frag_count } => todo!(),
                            DatagramHeader::FragmentSequenced { id, channel_id, fragment } => todo!(),
                            DatagramHeader::FirstFragmentNotSequenced { id, frag_count } => todo!(),
                            DatagramHeader::FragmentNotSequenced { id, fragment } => todo!(),
                            DatagramHeader::CompleteSequenced { id, channel_id } => todo!(),
                            DatagramHeader::CompleteNotSequenced => bytes_incoming_send.send(fragment.payload.into()).await.unwrap(),
                        }
                    },
                    Err(err) => error!("Failed to deserialize datagram: {}", err),
                }
            }
        } => {
            trace!("Listener for unreliable datagrams with id {} ended", id)
        }
    };
}
