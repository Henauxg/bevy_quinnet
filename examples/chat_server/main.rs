use std::collections::HashMap;

use bevy::{app::ScheduleRunnerPlugin, log::LogPlugin, prelude::*};
use bevy_quinnet::{
    server::{QuinnetServerPlugin, Server, ServerConfigurationData},
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
                if !users.names.contains_key(&client_id) {
                    warn!(
                        "Received a Disconnect from an unknown or disconnected client: {}",
                        client_id
                    )
                } else {
                    server.disconnect_client(client_id);
                    let username = users.names.remove(&client_id);
                    // Broadcast its deconnection
                    server
                        .send_group_message(
                            users.names.keys().into_iter(),
                            ServerMessage::ClientDisconnected {
                                client_id: client_id,
                            },
                        )
                        .unwrap();
                    info!("{:?} disconnected", username);
                }
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

fn main() {
    App::new()
        .add_plugin(ScheduleRunnerPlugin::default())
        .add_plugin(LogPlugin::default())
        .add_plugin(QuinnetServerPlugin::default())
        .insert_resource(ServerConfigurationData::new(
            "127.0.0.1".to_string(),
            5000,
            "127.0.0.1".to_string(),
        ))
        .insert_resource(Users::default())
        .add_system(handle_client_messages)
        .run();
}
