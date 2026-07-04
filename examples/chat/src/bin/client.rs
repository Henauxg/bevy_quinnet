use std::{
    collections::HashMap,
    thread,
    time::{Duration, Instant},
};

use bevy::{
    app::{AppExit, ScheduleRunnerPlugin},
    ecs::{
        message::{MessageReader, MessageWriter},
        schedule::{common_conditions::resource_exists, IntoScheduleConfigs},
    },
    log::{info, warn, LogPlugin},
    prelude::{App, Commands, Deref, DerefMut, Res, ResMut, Resource, Startup, Update},
};
use bevy_quinnet::{
    client::{
        certificate::CertificateVerificationMode,
        client_connected,
        connection::{
            ClientAddrConfiguration, ConnectionEvent, ConnectionFailedEvent, ConnectionLostEvent,
        },
        ClientConnectionConfiguration, QuinnetClient, QuinnetClientPlugin,
    },
    shared::ClientId,
};
use bevy_quinnet_chat::protocol::{ClientMessage, ServerMessage, SERVER_PORT};
use rand::{distr::Alphanumeric, RngExt};
use tokio::sync::mpsc;

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Resource, Debug, Clone, Default)]
struct Users {
    self_id: ClientId,
    names: HashMap<ClientId, String>,
}

#[derive(Resource, Deref, DerefMut)]
struct TerminalReceiver(mpsc::Receiver<String>);

#[derive(Resource)]
struct ShuttingDown {
    started_at: Instant,
}

fn handle_server_messages(mut users: ResMut<Users>, mut client: ResMut<QuinnetClient>) {
    while let Some(message) = client.connection_mut().try_receive_message() {
        match message {
            ServerMessage::ClientConnected {
                client_id,
                username,
            } => {
                info!("{} joined", username);
                users.names.insert(client_id, username);
            }
            ServerMessage::ClientDisconnected { client_id } => {
                if let Some(username) = users.names.remove(&client_id) {
                    println!("{} left", username);
                } else {
                    warn!("ClientDisconnected for an unknown client_id: {}", client_id)
                }
            }
            ServerMessage::ChatMessage { client_id, message } => {
                if let Some(username) = users.names.get(&client_id) {
                    if client_id != users.self_id {
                        println!("{}: {}", username, message);
                    }
                } else {
                    warn!("Chat message from an unknown client_id: {}", client_id)
                }
            }
            ServerMessage::InitClient {
                client_id,
                usernames,
            } => {
                users.self_id = client_id;
                users.names = usernames;
            }
        }
    }
}

fn handle_terminal_messages(
    mut terminal_messages: ResMut<TerminalReceiver>,
    mut commands: Commands,
    mut client: ResMut<QuinnetClient>,
    shutting_down: Option<Res<ShuttingDown>>,
) {
    if shutting_down.is_some() {
        return;
    }

    while let Ok(message) = terminal_messages.try_recv() {
        if message == "quit" {
            client
                .connection_mut()
                .send_message(ClientMessage::Disconnect {})
                .unwrap();
            commands.insert_resource(ShuttingDown {
                started_at: Instant::now(),
            });
        } else {
            client
                .connection_mut()
                .try_send_message(ClientMessage::ChatMessage { message });
        }
    }
}

fn exit_after_disconnect(
    connection_lost_events: MessageReader<ConnectionLostEvent>,
    mut app_exit_events: MessageWriter<AppExit>,
) {
    if !connection_lost_events.is_empty() {
        app_exit_events.write(AppExit::Success);
    }
}

fn shutdown_timeout_fallback(
    shutting_down: Res<ShuttingDown>,
    mut client: ResMut<QuinnetClient>,
    mut app_exit_events: MessageWriter<AppExit>,
) {
    if shutting_down.started_at.elapsed() < SHUTDOWN_TIMEOUT {
        return;
    }

    warn!("Shutdown timed out waiting for server disconnect; closing locally");
    client.connection_mut().try_disconnect();
    app_exit_events.write(AppExit::Success);
}

fn start_terminal_listener(mut commands: Commands) {
    let (from_terminal_sender, from_terminal_receiver) = mpsc::channel::<String>(100);

    thread::spawn(move || loop {
        let mut buffer = String::new();
        std::io::stdin().read_line(&mut buffer).unwrap();
        from_terminal_sender
            .try_send(buffer.trim_end().to_string())
            .unwrap();
    });

    commands.insert_resource(TerminalReceiver(from_terminal_receiver));
}

fn start_connection(mut client: ResMut<QuinnetClient>) {
    client
        .open_connection(ClientConnectionConfiguration {
            addr_config: ClientAddrConfiguration::from_strings(
                &format!("[::1]:{SERVER_PORT}"),
                "[::]:0",
            )
            .unwrap(),
            cert_mode: CertificateVerificationMode::SkipVerification,
            defaultables: Default::default(),
        })
        .unwrap();

    // You can already send message(s) even before being connected, they will be buffered. In this example we will wait for a ConnectionEvent.
}

fn handle_client_events(
    mut connection_events: MessageReader<ConnectionEvent>,
    mut connection_failed_events: MessageReader<ConnectionFailedEvent>,
    mut client: ResMut<QuinnetClient>,
) {
    if !connection_events.is_empty() {
        // We are connected
        let username: String = rand::rng()
            .sample_iter(&Alphanumeric)
            .take(7)
            .map(char::from)
            .collect();

        println!("--- Joining with name: {}", username);
        println!("--- Type 'quit' to disconnect");

        client
            .connection_mut()
            .send_message(ClientMessage::Join { name: username })
            .unwrap();

        connection_events.clear();
    }
    for ev in connection_failed_events.read() {
        println!(
            "Failed to connect: {:?}, make sure the chat-server is running.",
            ev.err
        );
    }
}

fn main() {
    App::new()
        .add_plugins((
            ScheduleRunnerPlugin::default(),
            LogPlugin::default(),
            QuinnetClientPlugin::default(),
        ))
        .insert_resource(Users::default())
        .add_systems(Startup, (start_terminal_listener, start_connection))
        .add_systems(
            Update,
            (
                handle_client_events,
                (handle_terminal_messages, handle_server_messages).run_if(client_connected),
                exit_after_disconnect,
                shutdown_timeout_fallback.run_if(resource_exists::<ShuttingDown>),
            ),
        )
        .run();
}
