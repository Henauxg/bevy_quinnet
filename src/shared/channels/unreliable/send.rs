use bevy::{
    log::warn,
    utils::tracing::{error, trace},
};
use bytes::{BufMut, Bytes, BytesMut};
use quinn::SendDatagramError;
use tokio::sync::{broadcast, mpsc};

use crate::shared::channels::{ChannelAsyncMessage, ChannelId, PROTOCOL_HEADER_LEN};

pub(crate) async fn unreliable_channel_task(
    connection: quinn::Connection,
    channel_id: ChannelId,
    _: mpsc::Sender<()>,
    from_channels_send: mpsc::Sender<ChannelAsyncMessage>,
    mut close_recv: broadcast::Receiver<()>,
    mut channel_close_recv: mpsc::Receiver<()>,
    mut bytes_to_channel_recv: mpsc::Receiver<Bytes>,
) {
    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Unreliable Channel task received a close signal")
        }
        _ = channel_close_recv.recv() => {
            trace!("Unreliable Channel task received a channel close signal")
        }
        _ = async {
            while let Some(msg_bytes) = bytes_to_channel_recv.recv().await {

                if let Err(err) = send_unreliable_message(&connection, msg_bytes, channel_id) {
                    error!("Error while sending message on Unreliable Channel, {}", err);
                    match err {
                        SendDatagramError::UnsupportedByPeer => (),
                        SendDatagramError::Disabled => (),
                        SendDatagramError::TooLarge => (),
                        SendDatagramError::ConnectionLost(_) => {
                            from_channels_send.send(
                                ChannelAsyncMessage::LostConnection)
                                .await
                                .expect("Failed to signal connection lost from channels");
                        },
                    }
                }
            }
        } => {
            trace!("Unreliable Channel task ended")
        }
    };
    while let Ok(msg_bytes) = bytes_to_channel_recv.try_recv() {
        if let Err(err) = send_unreliable_message(&connection, msg_bytes, channel_id) {
            warn!(
                "Failed to send a remaining message on Unreliable Channel, {}",
                err
            );
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
