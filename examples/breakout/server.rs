use std::collections::HashMap;

use bevy::prelude::{info, warn, EventReader, ResMut};
use bevy_quinnet::{
    server::{CertificateRetrievalMode, ConnectionLostEvent, Server, ServerConfigurationData},
    ClientId,
};

use crate::protocol::{ClientMessage, ServerMessage};

#[derive(Debug, Clone, Default)]
pub(crate) struct Users {
    names: HashMap<ClientId, String>,
}

pub(crate) fn handle_client_messages(mut server: ResMut<Server>, mut users: ResMut<Users>) {
    while let Ok(Some((message, client_id))) = server.receive_message::<ClientMessage>() {
        match message {
            ClientMessage::Join {} => {}
            ClientMessage::Disconnect {} => {
                // We tell the server to disconnect this user
                server.disconnect_client(client_id);
                handle_disconnect(&mut server, &mut users, client_id);
            }
            ClientMessage::PaddleInput {} => todo!(),
        }
    }
}

pub(crate) fn handle_server_events(
    mut connection_lost_events: EventReader<ConnectionLostEvent>,
    mut server: ResMut<Server>,
    mut users: ResMut<Users>,
) {
    // The server signals us about users that lost connection
    for client in connection_lost_events.iter() {
        handle_disconnect(&mut server, &mut users, client.id);
    }
}

/// Shared disconnection behaviour, whether the client lost connection or asked to disconnect
pub(crate) fn handle_disconnect(
    server: &mut ResMut<Server>,
    users: &mut ResMut<Users>,
    client_id: ClientId,
) {
    // Remove this user
    if let Some(username) = users.names.remove(&client_id) {
        // Broadcast its deconnection
        server
            .send_group_message(
                users.names.keys().into_iter(),
                ServerMessage::ClientDisconnected {
                    client_id: client_id,
                },
            )
            .unwrap();
        info!("{} disconnected", username);
    } else {
        warn!(
            "Received a Disconnect from an unknown or disconnected client: {}",
            client_id
        )
    }
}

pub(crate) fn start_listening(server: ResMut<Server>) {
    server
        .start(
            ServerConfigurationData::new("127.0.0.1".to_string(), 6000, "0.0.0.0".to_string()),
            CertificateRetrievalMode::GenerateSelfSigned,
        )
        .unwrap();
}
