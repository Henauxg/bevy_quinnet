use bevy::{
    log::warn,
    utils::tracing::{error, trace},
};
use futures::sink::SinkExt;
use quinn::SendStream;
use tokio_util::codec::FramedWrite;

use crate::shared::channels::{ChannelAsyncMessage, ChannelId, CloseReason, SendChannelTask};

use super::codec::QuinnetProtocolCodecEncoder;

async fn new_uni_frame_sender(
    connection: &quinn::Connection,
    raw_channel_id: ChannelId,
    max_frame_len: usize,
) -> FramedWrite<SendStream, QuinnetProtocolCodecEncoder> {
    let uni_sender = connection
        .open_uni()
        .await
        .expect("Failed to open send stream");
    FramedWrite::new(
        uni_sender,
        QuinnetProtocolCodecEncoder::new(raw_channel_id, max_frame_len),
    )
}

pub(crate) async fn ordered_reliable_channel_task(
    mut channel_task: SendChannelTask,
    max_frame_len: usize,
) {
    let mut frame_sender =
        new_uni_frame_sender(&channel_task.connection, channel_task.id, max_frame_len).await;

    let close_reason = tokio::select! {
        close_reason = channel_task.close_recv.recv() => {
            trace!("Ordered Reliable Channel task received a close signal");
            match close_reason {
                Ok(reason) => reason,
                Err(_) => CloseReason::LocalOrder,
            }
        }
        _ = channel_task.channel_close_recv.recv() => {
            trace!("Ordered Reliable Channel task received a channel close signal");
            CloseReason::LocalOrder
        }
        _ = async {
            // Send channel messages
            while let Some(msg_bytes) = channel_task.bytes_recv.recv().await {
                if let Err(err) = frame_sender.send(msg_bytes).await {
                    error!("Error while sending on Ordered Reliable Channel, {}", err);
                    channel_task.from_channels_send.send(
                        ChannelAsyncMessage::LostConnection)
                        .await
                        .expect("Failed to signal connection lost on Ordered Reliable Channel");
                }
            }
        } => {
            trace!("Ordered Reliable Channel task ended");
            CloseReason::LocalOrder
        }
    };
    // No need to try to flush if we know that the peer is already closed
    if close_reason != CloseReason::PeerClosed {
        while let Ok(msg_bytes) = channel_task.bytes_recv.try_recv() {
            if let Err(err) = frame_sender.send(msg_bytes).await {
                warn!(
                    "Failed to send a remaining message on Ordered Reliable Channel, {}",
                    err
                );
            }
        }
        if let Err(err) = frame_sender.flush().await {
            warn!(
                "Error while flushing Ordered Reliable Channel stream: {}",
                err
            );
        }
        if let Err(err) = frame_sender.into_inner().finish() {
            warn!(
                "Failed to shutdown Ordered Reliable Channel stream gracefully: {}",
                err
            );
        }
    }
}

pub(crate) async fn unordered_reliable_channel_task(
    mut channel_task: SendChannelTask,
    max_frame_len: usize,
) {
    let close_reason = tokio::select! {
        close_reason = channel_task.close_recv.recv() => {
            trace!("Unordered Reliable Channel task received a close signal");
            match close_reason {
                Ok(reason) => reason,
                Err(_) => CloseReason::LocalOrder,
            }
        }
        _ = channel_task.channel_close_recv.recv() => {
            trace!("Unordered Reliable Channel task received a channel close signal");
            CloseReason::LocalOrder
        }
        _ = async {
            while let Some(msg_bytes) = channel_task.bytes_recv.recv().await {
                let conn = channel_task.connection.clone();
                let from_channels_send_clone = channel_task.from_channels_send.clone();
                let channels_keepalive_clone = channel_task.channels_keepalive.clone();
                tokio::spawn(async move {
                    let mut frame_sender = new_uni_frame_sender(&conn,channel_task.id, max_frame_len).await;
                    if let Err(err) = frame_sender.send(msg_bytes).await {
                        error!("Error while sending on Unordered Reliable Channel, {}", err);
                        from_channels_send_clone.send(
                            ChannelAsyncMessage::LostConnection)
                            .await
                            .expect("Failed to signal connection lost on Unordered Reliable Channel");
                    }
                    if let Err(err) = frame_sender.into_inner().finish() {
                        warn!("Failed to shutdown Unordered Reliable Channel stream gracefully: {}", err);
                    }
                    drop(channels_keepalive_clone)
                });
            }
        } => {
            trace!("Unordered Reliable Channel task ended");
            CloseReason::LocalOrder
        }
    };
    // No need to try to flush if we know that the peer is already closed
    if close_reason != CloseReason::PeerClosed {
        while let Ok(msg_bytes) = channel_task.bytes_recv.try_recv() {
            let conn = channel_task.connection.clone();
            let channels_keepalive_clone = channel_task.channels_keepalive.clone();
            tokio::spawn(async move {
                let mut frame_sender =
                    new_uni_frame_sender(&conn, channel_task.id, max_frame_len).await;
                if let Err(err) = frame_sender.send(msg_bytes).await {
                    warn!(
                        "Failed to send a remaining message on Unordered Reliable Channel, {}",
                        err
                    );
                }
                if let Err(err) = frame_sender.into_inner().finish() {
                    warn!(
                        "Failed to shutdown Unordered Reliable Channel stream gracefully: {}",
                        err
                    );
                }
                drop(channels_keepalive_clone)
            });
        }
    }
}
