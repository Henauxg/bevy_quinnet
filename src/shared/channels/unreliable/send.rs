use bevy::{
    log::warn,
    utils::tracing::{error, trace},
};
use bytes::{BufMut, Bytes, BytesMut};
use quinn::SendDatagramError;

use crate::shared::channels::{
    ChannelAsyncMessage, ChannelId, CloseReason, SendChannelTask, PROTOCOL_HEADER_LEN,
};

pub(crate) async fn unreliable_channel_task(mut task: SendChannelTask) {
    let close_reason = tokio::select! {
        close_reason = task.close_recv.recv() => {
            trace!("Unreliable Channel task received a close signal");
            match close_reason {
                Ok(reason) => reason,
                Err(_) => CloseReason::LocalOrder,
            }
        }
        _ = task.channel_close_recv.recv() => {
            trace!("Unreliable Channel task received a channel close signal");
            CloseReason::LocalOrder
        }
        _ = async {
            while let Some(msg_bytes) = task.bytes_recv.recv().await {
                if let Err(err) = send_unreliable_message(&task.connection, msg_bytes, task.id) {
                    error!("Error while sending message on Unreliable Channel, {}", err);
                    match err {
                        SendDatagramError::UnsupportedByPeer => (),
                        SendDatagramError::Disabled => (),
                        SendDatagramError::TooLarge => (),
                        SendDatagramError::ConnectionLost(_) => {
                            task.from_channels_send.send(
                                ChannelAsyncMessage::LostConnection)
                                .await
                                .expect("Failed to signal connection lost from channels");
                        },
                    }
                }
            }
        } => {
            trace!("Unreliable Channel task ended");
            CloseReason::LocalOrder
        }
    };
    // No need to try to flush if we know that the peer is already closed
    if close_reason != CloseReason::PeerClosed {
        while let Ok(msg_bytes) = task.bytes_recv.try_recv() {
            if let Err(err) = send_unreliable_message(&task.connection, msg_bytes, task.id)
            {
                warn!(
                    "Failed to send a remaining message on Unreliable Channel, {}",
                    err
                );
            }
        }
    }
}

fn send_unreliable_message(
    connection: &quinn::Connection,
    msg_bytes: Bytes,
    channel_id: ChannelId,
) -> Result<(), SendDatagramError> {
    let mut datagram = BytesMut::with_capacity(PROTOCOL_HEADER_LEN + msg_bytes.len());
    datagram.put_u8(channel_id);
    datagram.extend_from_slice(&msg_bytes[..]);
    connection.send_datagram(datagram.into())
}
