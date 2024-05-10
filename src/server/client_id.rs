use bevy::prelude::*;
use bytes::{BufMut, BytesMut};
use futures::SinkExt;
use tokio::sync::mpsc::{self};
use tokio_util::codec::{FramedWrite, LengthDelimitedCodec};

use crate::shared::{channels::ChannelAsyncMessage, ClientId, CLIENT_ID_LEN};

pub(crate) fn spawn_client_id_sender(
    connection_handle: quinn::Connection,
    client_id: ClientId,
    from_channels_send: mpsc::Sender<ChannelAsyncMessage>,
) {
    tokio::spawn(async move {
        let (stream_send, _) = connection_handle
            .open_bi()
            .await
            .expect("Failed to open send stream");
        let mut frame_sender = FramedWrite::new(stream_send, LengthDelimitedCodec::new());

        let mut msg_bytes = BytesMut::with_capacity(CLIENT_ID_LEN);
        msg_bytes.put_uint(client_id, CLIENT_ID_LEN);
        if let Err(err) = frame_sender.send(msg_bytes.into()).await {
            error!(
                "Error while sending client Id {} on Quinnet Protocol Channel, {}",
                client_id, err
            );
            from_channels_send
                .send(ChannelAsyncMessage::LostConnection)
                .await
                .expect("Failed to signal connection lost on Quinnet Protocol Channel");
        }
    });
}
