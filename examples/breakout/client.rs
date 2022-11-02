use std::collections::HashMap;

use bevy::prelude::{warn, EventReader, ResMut};
use bevy_quinnet::{
    client::{CertificateVerificationMode, Client, ClientConfigurationData, ConnectionEvent},
    ClientId,
};

use crate::protocol::ServerMessage;

#[derive(Debug, Clone, Default)]
pub(crate) struct Users {
    self_id: ClientId,
    names: HashMap<ClientId, String>,
}

pub(crate) fn handle_server_messages(mut client: ResMut<Client>, mut users: ResMut<Users>) {
    while let Ok(Some(message)) = client.receive_message::<ServerMessage>() {
        match message {
            ServerMessage::ClientConnected { client_id: _ } => {}
            ServerMessage::ClientDisconnected { client_id } => {
                if let Some(username) = users.names.remove(&client_id) {
                    println!("{} left", username);
                } else {
                    warn!("ClientDisconnected for an unknown client_id: {}", client_id)
                }
            }
            ServerMessage::InitClient { client_id } => {
                users.self_id = client_id;
            }
            ServerMessage::BrickDestroyed {} => todo!(),
            ServerMessage::BallPosition {} => todo!(),
            ServerMessage::PaddlePosition {} => todo!(),
        }
    }
}

pub(crate) fn start_connection(client: ResMut<Client>) {
    client
        .connect(
            ClientConfigurationData::new("127.0.0.1".to_string(), 6000, "0.0.0.0".to_string(), 0),
            CertificateVerificationMode::SkipVerification,
        )
        .unwrap();
}

pub(crate) fn handle_client_events(
    connection_events: EventReader<ConnectionEvent>,
    client: ResMut<Client>,
) {
    if !connection_events.is_empty() {
        // We are connected
        // let username: String = rand::thread_rng()
        //     .sample_iter(&Alphanumeric)
        //     .take(7)
        //     .map(char::from)
        //     .collect();

        // println!("--- Joining with name: {}", username);
        // println!("--- Type 'quit' to disconnect");

        // client
        //     .send_message(ClientMessage::Join { name: username })
        //     .unwrap();

        connection_events.clear();
    }
}
