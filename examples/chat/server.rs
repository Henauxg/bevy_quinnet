use std::collections::HashMap;

use bevy::{app::ScheduleRunnerPlugin, log::LogPlugin, prelude::*};
use bevy_quinnet::{
    server::{
        certificate::CertificateRetrievalMode, ConnectionLostEvent, Endpoint, QuinnetServerPlugin,
        Server, ServerConfiguration,
    },
    shared::{channel::ChannelId, ClientId},
};

use protocol::{ClientMessage, ServerMessage};

mod protocol;

#[derive(Resource, Debug, Clone, Default)]
struct Users {
    names: HashMap<ClientId, String>,
}

fn handle_client_messages(mut server: ResMut<Server>, mut users: ResMut<Users>) {
    let endpoint = server.endpoint_mut();
    for client_id in endpoint.clients() {
        while let Some(message) = endpoint.try_receive_message_from::<ClientMessage>(client_id) {
            match message {
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
                        endpoint
                            .send_message(
                                client_id,
                                ServerMessage::InitClient {
                                    client_id: client_id,
                                    usernames: users.names.clone(),
                                },
                            )
                            .unwrap();
                        // Broadcast the connection event
                        endpoint
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
                    endpoint.disconnect_client(client_id).unwrap();
                    handle_disconnect(endpoint, &mut users, client_id);
                }
                ClientMessage::ChatMessage { message } => {
                    info!(
                        "Chat message | {:?}: {}",
                        users.names.get(&client_id),
                        message
                    );
                    endpoint.try_send_group_message_on(
                        users.names.keys().into_iter(),
                        ChannelId::UnorderedReliable,
                        ServerMessage::ChatMessage {
                            client_id: client_id,
                            message: message,
                        },
                    );
                }
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
        handle_disconnect(server.endpoint_mut(), &mut users, client.id);
    }
}

/// Shared disconnection behaviour, whether the client lost connection or asked to disconnect
fn handle_disconnect(endpoint: &mut Endpoint, users: &mut ResMut<Users>, client_id: ClientId) {
    // Remove this user
    if let Some(username) = users.names.remove(&client_id) {
        // Broadcast its deconnection

        endpoint
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

fn start_listening(mut server: ResMut<Server>) {
    server
        .start_endpoint(
            ServerConfiguration::from_string("0.0.0.0:6000").unwrap(),
            CertificateRetrievalMode::GenerateSelfSigned {
                server_hostname: "127.0.0.1".to_string(),
            },
        )
        .unwrap();
}

fn main() {
    App::new()
        .add_plugins((
            ScheduleRunnerPlugin::default(),
            LogPlugin::default(),
            QuinnetServerPlugin::default(),
        ))
        .insert_resource(Users::default())
        .add_systems(Startup, start_listening)
        .add_systems(Update, (handle_client_messages, handle_server_events))
        .run();
}
