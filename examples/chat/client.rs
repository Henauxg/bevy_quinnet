use std::{
    collections::HashMap,
    thread::{self, sleep},
    time::Duration,
};

use bevy::{
    app::{AppExit, ScheduleRunnerPlugin},
    log::LogPlugin,
    prelude::{
        info, warn, App, Commands, Deref, DerefMut, EventReader, EventWriter, PostUpdate, Res,
        ResMut, Resource, Startup, Update,
    },
};
use bevy_quinnet::{
    client::{
        certificate::CertificateVerificationMode,
        connection::{ConnectionConfiguration, ConnectionEvent},
        Client, QuinnetClientPlugin,
    },
    shared::ClientId,
};
use rand::{distributions::Alphanumeric, Rng};
use tokio::sync::mpsc;

use protocol::{ClientMessage, ServerMessage};

mod protocol;

#[derive(Resource, Debug, Clone, Default)]
struct Users {
    self_id: ClientId,
    names: HashMap<ClientId, String>,
}

#[derive(Resource, Deref, DerefMut)]
struct TerminalReceiver(mpsc::Receiver<String>);

pub fn on_app_exit(app_exit_events: EventReader<AppExit>, client: Res<Client>) {
    if !app_exit_events.is_empty() {
        client
            .connection()
            .send_message(ClientMessage::Disconnect {})
            .unwrap();
        // TODO Clean: event to let the async client send his last messages.
        sleep(Duration::from_secs_f32(0.1));
    }
}

fn handle_server_messages(mut users: ResMut<Users>, mut client: ResMut<Client>) {
    while let Some(message) = client
        .connection_mut()
        .try_receive_message::<ServerMessage>()
    {
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
    mut app_exit_events: EventWriter<AppExit>,
    client: Res<Client>,
) {
    while let Ok(message) = terminal_messages.try_recv() {
        if message == "quit" {
            app_exit_events.send(AppExit);
        } else {
            client
                .connection()
                .try_send_message(ClientMessage::ChatMessage { message: message });
        }
    }
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

fn start_connection(mut client: ResMut<Client>) {
    client
        .open_connection(
            ConnectionConfiguration::from_strings("127.0.0.1:6000", "0.0.0.0:0").unwrap(),
            CertificateVerificationMode::SkipVerification,
        )
        .unwrap();

    // You can already send message(s) even before being connected, they will be buffered. In this example we will wait for a ConnectionEvent.
}

fn handle_client_events(
    mut connection_events: EventReader<ConnectionEvent>,
    client: ResMut<Client>,
) {
    if !connection_events.is_empty() {
        // We are connected
        let username: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(7)
            .map(char::from)
            .collect();

        println!("--- Joining with name: {}", username);
        println!("--- Type 'quit' to disconnect");

        client
            .connection()
            .send_message(ClientMessage::Join { name: username })
            .unwrap();

        connection_events.clear();
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
                handle_terminal_messages,
                handle_server_messages,
                handle_client_events,
            ),
        )
        // CoreSet::PostUpdate so that AppExit events generated in the previous stage are available
        .add_systems(PostUpdate, on_app_exit)
        .run();
}
