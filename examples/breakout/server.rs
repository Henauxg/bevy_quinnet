use std::collections::HashMap;

use bevy::prelude::{info, warn, EventReader, ResMut, State};
use bevy_quinnet::{
    server::{
        CertificateRetrievalMode, ConnectionEvent, ConnectionLostEvent, Server,
        ServerConfigurationData,
    },
    ClientId,
};

use crate::{
    protocol::{ClientMessage, ServerMessage},
    GameState,
};

#[derive(Debug, Clone, Default)]
pub(crate) struct Player {
    score: u64,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Players {
    scores: HashMap<ClientId, Player>,
}

pub(crate) fn handle_client_messages(mut server: ResMut<Server>, mut users: ResMut<Players>) {
    while let Ok(Some((message, client_id))) = server.receive_message::<ClientMessage>() {
        match message {
            // ClientMessage::Disconnect {} => {
            //     // We tell the server to disconnect this user
            //     server.disconnect_client(client_id);
            //     handle_disconnect(&mut server, &mut users, client_id);
            // }
            ClientMessage::PaddleInput {} => todo!(),
        }
    }
}

pub(crate) fn handle_server_events(
    mut connection_events: EventReader<ConnectionEvent>,
    mut connection_lost_events: EventReader<ConnectionLostEvent>,
    mut server: ResMut<Server>,
    mut players: ResMut<Players>,
    mut game_state: ResMut<State<GameState>>,
) {
    // The server signals us about new connections
    for client in connection_events.iter() {
        // Refuse connection once we already have two players
        if players.scores.len() >= 2 {
            server.disconnect_client(client.id)
        } else {
            players.scores.insert(client.id, Player { score: 0 });
            if players.scores.len() == 2 {
                server
                    .send_group_message(
                        players.scores.keys().into_iter(),
                        ServerMessage::GameStart {},
                    )
                    .unwrap();
            }
        }
    }
    // The server signals us about users that lost connection
    // for client in connection_lost_events.iter() {
    //     handle_disconnect(&mut server, &mut players, client.id);
    // }
}

// /// Shared disconnection behaviour, whether the client lost connection or asked to disconnect
// pub(crate) fn handle_disconnect(
//     server: &mut ResMut<Server>,
//     players: &mut ResMut<Players>,
//     client_id: ClientId,
// ) {
//     // Remove this user

// }

pub(crate) fn start_listening(mut server: ResMut<Server>) {
    server
        .start(
            ServerConfigurationData::new("127.0.0.1".to_string(), 6000, "0.0.0.0".to_string()),
            CertificateRetrievalMode::GenerateSelfSigned,
        )
        .unwrap();
}
