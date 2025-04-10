use bevy::log::trace;
use bytes::Bytes;
use std::fmt::Display;
use tokio::sync::mpsc::{self};

use crate::shared::channels::{ChannelId, CloseRecv, CHANNEL_ID_LEN};

pub(crate) async fn unreliable_channel_receiver_task<T: Display>(
    task_id: T,
    connection: quinn::Connection,
    mut close_recv: CloseRecv,
    bytes_incoming_send: mpsc::Sender<(ChannelId, Bytes)>,
) {
    tokio::select! {
        _ = close_recv.recv() => {
            trace!("Listener for unreliable datagrams with id {} received a close signal", task_id)
        }
        _ = async {
            while let Ok(mut msg_bytes) = connection.read_datagram().await {
                if msg_bytes.len() <= CHANNEL_ID_LEN {
                    continue;
                }
                let payload = msg_bytes.split_off(1).into();
                let channel_id = msg_bytes[0];
                // TODO Clean: error handling
                bytes_incoming_send.send((channel_id, payload)).await.unwrap();
            }
        } => {
            trace!("Listener for unreliable datagrams with id {} ended", task_id)
        }
    };
}
