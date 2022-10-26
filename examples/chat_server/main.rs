use std::collections::HashMap;

use bevy::{app::ScheduleRunnerPlugin, log::LogPlugin, prelude::*};
use bevy_quinnet::{
    server::{
        CertificateRetrievalMode, ConnectionLostEvent, QuinnetServerPlugin, Server,
        ServerConfigurationData,
    },
    ClientId,
};

use chat_protocol::{ClientMessage, ServerMessage};

#[path = "../chat_protocol/lib.rs"] // Because we can't have a shared lib between Cargo examples
mod chat_protocol;

#[derive(Debug, Clone, Default)]
struct Users {
    names: HashMap<ClientId, String>,
}

fn handle_client_messages(mut server: ResMut<Server>, mut users: ResMut<Users>) {
    while let Ok(Some(message)) = server.receive_message::<ClientMessage>() {
        // Retrieve the assigned ClientId.
        let client_id = message.1;
        match message.0 {
            ClientMessage::Join { name } => {
                if users.names.contains_key(&client_id) {
                    warn!(
                        "Received a Join from an already connected client: {}",
                        client_id
                    )
                } else {
                    info!("{} connected", name);
                    users.names.insert(client_id, name.clone());
                    // Initialize this client with existing state
                    server
                        .send_message(
                            client_id,
                            ServerMessage::InitClient {
                                client_id: client_id,
                                usernames: users.names.clone(),
                            },
                        )
                        .unwrap();
                    // Broadcast the connection event
                    server
                        .send_group_message(
                            users.names.keys().into_iter(),
                            ServerMessage::ClientConnected {
                                client_id: client_id,
                                username: name,
                            },
                        )
                        .unwrap();
                }
            }
            ClientMessage::Disconnect {} => {
                // We tell the server to disconnect this user
                server.disconnect_client(client_id);
                handle_disconnect(&mut server, &mut users, client_id);
            }
            ClientMessage::ChatMessage { message } => {
                info!(
                    "Chat message | {:?}: {}",
                    users.names.get(&client_id),
                    message
                );
                server
                    .send_group_message(
                        users.names.keys().into_iter(),
                        ServerMessage::ChatMessage {
                            client_id: client_id,
                            message: message,
                        },
                    )
                    .unwrap();
            }
        }
    }
}

fn handle_server_events(
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
fn handle_disconnect(server: &mut ResMut<Server>, users: &mut ResMut<Users>, client_id: ClientId) {
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

fn start_listening(server: ResMut<Server>) {
    server
        .start(
            ServerConfigurationData::new("127.0.0.1".to_string(), 6000, "0.0.0.0".to_string()),
            CertificateRetrievalMode::GenerateSelfSigned,
        )
        .unwrap();
}

fn main() {
    App::new()
        .add_plugin(ScheduleRunnerPlugin::default())
        .add_plugin(LogPlugin::default())
        .add_plugin(QuinnetServerPlugin::default())
        .insert_resource(Users::default())
        .add_startup_system(start_listening)
        .add_system(handle_client_messages)
        .add_system(handle_server_events)
        .run();
}
