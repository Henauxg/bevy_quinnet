use bevy::log::trace;
use bytes::{Buf, Bytes, BytesMut};
use futures::StreamExt;
use quinn::RecvStream;
use std::{fmt::Display, io::Cursor};
use tokio::sync::mpsc::{self};
use tokio_util::codec::FramedRead;

use crate::shared::channels::{
    reliable::{codec::QuinnetProtocolCodecDecoder, DEFAULT_MAX_RELIABLE_FRAME_LEN},
    ChannelId, CloseRecv, CHANNEL_ID_LEN,
};

pub(crate) async fn reliable_channels_receiver_task<T: Display>(
    task_id: T,
    connection: quinn::Connection,
    mut close_recv: CloseRecv,
    bytes_incoming_send: mpsc::Sender<(ChannelId, Bytes)>,
) {
    let close_recv_clone = close_recv.resubscribe();
    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Listener for new Unidirectional Receiving Streams with id {} received a close signal", task_id)
        }
        _ = async {
            while let Ok(recv) = connection.accept_uni().await {
                let bytes_incoming_send_clone = bytes_incoming_send.clone();
                let close_recv_clone = close_recv_clone.resubscribe();
                tokio::spawn(async move {
                    reliable_stream_receiver_task(
                        recv,
                        close_recv_clone,
                        bytes_incoming_send_clone
                    ).await;
                });
            }
        } => {
            trace!("Listener for new Unidirectional Receiving Streams with id {} ended", task_id)
        }
    };
}

async fn reliable_stream_receiver_task(
    recv: RecvStream,
    mut close_recv: CloseRecv,
    bytes_incoming_send: mpsc::Sender<(ChannelId, Bytes)>,
) {
    tokio::select! {
        _ = close_recv.recv() => {}
        _ = async {
            let mut frame_recv = FramedRead::new(recv, QuinnetProtocolCodecDecoder::new(DEFAULT_MAX_RELIABLE_FRAME_LEN));
            while let Some(Ok(msg_bytes)) = frame_recv.next().await {
                // TODO Clean: error handling
                bytes_incoming_send
                    .send(decode_incoming_reliable_message(msg_bytes))
                    .await
                    .unwrap();
            }
        } => {}
    };
}

fn decode_incoming_reliable_message(mut msg_bytes: BytesMut) -> (ChannelId, Bytes) {
    let mut msg = Cursor::new(&msg_bytes);
    let channel_id = msg.get_u8();
    let payload = msg_bytes.split_off(CHANNEL_ID_LEN).into();
    (channel_id, payload)
}
