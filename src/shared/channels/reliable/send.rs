use bevy::{
    log::warn,
    utils::tracing::{error, trace},
};
use bytes::Bytes;
use futures::sink::SinkExt;
use quinn::SendStream;
use tokio::sync::{broadcast, mpsc};
use tokio_util::codec::FramedWrite;

use crate::shared::channels::{ChannelAsyncMessage, ChannelId};

use super::{codec::QuinnetProtocolCodecEncoder, DEFAULT_MAX_RELIABLE_FRAME_LEN};

async fn new_uni_frame_sender(
    connection: &quinn::Connection,
    raw_channel_id: ChannelId,
) -> FramedWrite<SendStream, QuinnetProtocolCodecEncoder> {
    let uni_sender = connection
        .open_uni()
        .await
        .expect("Failed to open send stream");
    FramedWrite::new(
        uni_sender,
        QuinnetProtocolCodecEncoder::new(raw_channel_id, DEFAULT_MAX_RELIABLE_FRAME_LEN),
    )
}

pub(crate) async fn ordered_reliable_channel_task(
    connection: quinn::Connection,
    raw_channel_id: ChannelId,
    _: mpsc::Sender<()>,
    from_channels_send: mpsc::Sender<ChannelAsyncMessage>,
    mut close_recv: broadcast::Receiver<()>,
    mut channel_close_recv: mpsc::Receiver<()>,
    mut bytes_to_channel_recv: mpsc::Receiver<Bytes>,
) {
    let mut frame_sender = new_uni_frame_sender(&connection, raw_channel_id).await;

    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Ordered Reliable Channel task received a close signal")
        }
        _ = channel_close_recv.recv() => {
            trace!("Ordered Reliable Channel task received a channel close signal")
        }
        _ = async {
            // Send channel messages
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
            warn!(
                "Failed to send a remaining message on Ordered Reliable Channel, {}",
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
    raw_channel_id: ChannelId,
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
                    let mut frame_sender = new_uni_frame_sender(&conn,raw_channel_id).await;
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
            let mut frame_sender = new_uni_frame_sender(&conn, raw_channel_id).await;
            if let Err(err) = frame_sender.send(msg_bytes).await {
                warn!(
                    "Failed to send a remaining message on Unordered Reliable Channel, {}",
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
