use bevy::log::trace;
use bytes::Bytes;
use quinn::VarInt;
use tokio::sync::{
    broadcast,
    mpsc::{self},
};

use crate::shared::channels::{
    reliable::{
        recv::reliable_channels_receiver_task,
        send::{ordered_reliable_channel_task, unordered_reliable_channel_task},
    },
    unreliable::{recv::unreliable_channel_receiver_task, send::unreliable_channel_task},
    ChannelAsyncMessage, ChannelConfig, ChannelId, ChannelSyncMessage, CloseReason, CloseRecv,
};

/// Spawn a task to handle send channels creation for this connection
pub(crate) fn spawn_send_channels_tasks_spawner(
    connection_handle: quinn::Connection,
    close_recv: broadcast::Receiver<CloseReason>,
    to_channels_recv: mpsc::Receiver<ChannelSyncMessage>,
    from_channels_send: mpsc::Sender<ChannelAsyncMessage>,
) {
    tokio::spawn(async move {
        send_channels_tasks_spawner(
            connection_handle,
            close_recv,
            to_channels_recv,
            from_channels_send,
        )
        .await
    });
}

pub(crate) struct SendChannelTaskData {
    pub(crate) connection: quinn::Connection,
    pub(crate) id: ChannelId,
    pub(crate) channels_keepalive: mpsc::Sender<()>,
    pub(crate) from_channels_send: mpsc::Sender<ChannelAsyncMessage>,
    pub(crate) close_recv: CloseRecv,
    pub(crate) channel_close_recv: mpsc::Receiver<()>,
    pub(crate) bytes_recv: mpsc::Receiver<Bytes>,
}

pub(crate) async fn send_channels_tasks_spawner(
    connection: quinn::Connection,
    mut close_recv: broadcast::Receiver<CloseReason>,
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
            while let Some(ChannelSyncMessage::CreateChannel {
                id,
                config,
                bytes_to_channel_recv: bytes_recv,
                channel_close_recv,
            }) = to_channels_recv.recv().await {

                let channel_task_data = SendChannelTaskData {
                    connection: connection.clone(),
                    id,
                    channels_keepalive: channel_tasks_keepalive.clone(),
                    from_channels_send: from_channels_send.clone(),
                    close_recv: close_receiver_clone.resubscribe(),
                    channel_close_recv,
                    bytes_recv,
                };

                match config {
                    ChannelConfig::OrderedReliable { max_frame_size } => {
                        tokio::spawn(async move { ordered_reliable_channel_task(channel_task_data, max_frame_size).await });
                    }
                    ChannelConfig::UnorderedReliable { max_frame_size } => {
                        tokio::spawn(
                            async move { unordered_reliable_channel_task(channel_task_data, max_frame_size).await },
                        );
                    }
                    ChannelConfig::Unreliable => {
                        tokio::spawn(async move { unreliable_channel_task(channel_task_data).await });
                    }
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

pub(crate) fn spawn_recv_channels_tasks(
    connection_handle: quinn::Connection,
    connection_id: u64,
    close_recv: broadcast::Receiver<CloseReason>,
    bytes_incoming_send: mpsc::Sender<(ChannelId, Bytes)>,
) {
    // Spawn a task to listen for reliable messages
    {
        let connection_handle = connection_handle.clone();
        let close_recv = close_recv.resubscribe();
        let bytes_incoming_send = bytes_incoming_send.clone();
        tokio::spawn(async move {
            reliable_channels_receiver_task(
                connection_id,
                connection_handle,
                close_recv,
                bytes_incoming_send,
            )
            .await
        });
    }

    // Spawn a task to listen for unreliable datagrams
    {
        let connection_handle = connection_handle.clone();
        let close_recv = close_recv.resubscribe();
        let bytes_incoming_send = bytes_incoming_send.clone();
        tokio::spawn(async move {
            unreliable_channel_receiver_task(
                connection_id,
                connection_handle,
                close_recv,
                bytes_incoming_send,
            )
            .await
        });
    }
}
