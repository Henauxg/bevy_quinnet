// pub const DEFAULT_MESSAGE_QUEUE_SIZE: usize = 150;
// pub const DEFAULT_KILL_MESSAGE_QUEUE_SIZE: usize = 10;
// pub const DEFAULT_KEEP_ALIVE_INTERVAL_S: u64 = 4;

pub mod client;
pub mod server;
pub mod shared;

///////////////////////////////////////////////////////////
///                                                     ///
///                      Tests                          ///
///                                                     ///
///////////////////////////////////////////////////////////

#[cfg(test)]
mod tests {
    use std::{thread::sleep, time::Duration};

    use crate::{
        client::{
            self, certificate::CertificateVerificationMode, Client, ConnectionConfiguration,
            QuinnetClientPlugin,
        },
        server::{
            self, certificate::CertificateRetrievalMode, QuinnetServerPlugin, Server,
            ServerConfigurationData,
        },
        shared::ClientId,
    };
    use bevy::{
        app::ScheduleRunnerPlugin,
        prelude::{App, EventReader, ResMut, Resource},
    };
    use serde::{Deserialize, Serialize};

    const SERVER_HOST: &str = "127.0.0.1";
    const SERVER_PORT: u16 = 6000;

    #[derive(Resource, Debug, Clone, Default)]
    struct ClientTestData {
        connection_events_received: u64,
    }

    #[derive(Resource, Debug, Clone, Default)]
    struct ServerTestData {
        connection_events_received: u64,
        last_connected_client_id: ClientId,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub enum SharedMessage {
        ChatMessage(String),
    }

    #[test]
    fn connection_with_two_apps() {
        let mut client_app = build_client_app();
        let mut server_app = build_server_app();

        // Startup
        client_app.update();
        server_app.update();

        {
            let client = client_app.world.resource::<Client>();
            assert!(
                client.get_connection().is_some(),
                "The default connection should exist"
            );
            let server = server_app.world.resource::<Server>();
            assert!(server.is_listening(), "The server should be listening");
        }

        // Let the async runtime connection connect.
        sleep(Duration::from_secs_f32(0.1));

        // Connection event propagation
        client_app.update();
        server_app.update();

        let sent_client_message = SharedMessage::ChatMessage("Test message content".to_string());
        {
            let server_test_data = server_app.world.resource::<ServerTestData>();
            assert_eq!(server_test_data.connection_events_received, 1);

            let client = client_app.world.resource::<Client>();
            let client_test_data = client_app.world.resource::<ClientTestData>();
            assert!(
                client.connection().is_connected(),
                "The default connection should be connected to the server"
            );
            assert_eq!(client_test_data.connection_events_received, 1);

            client
                .connection()
                .send_message(sent_client_message.clone())
                .unwrap();
        }

        // Client->Server Message
        sleep(Duration::from_secs_f32(0.1));
        server_app.update();

        {
            let (client_message, client_id) = server_app
                .world
                .resource_mut::<Server>()
                .receive_message::<SharedMessage>()
                .expect("Failed to receive client message")
                .expect("There should be a client message");
            let server_test_data = server_app.world.resource::<ServerTestData>();
            assert_eq!(client_id, server_test_data.last_connected_client_id);
            assert_eq!(client_message, sent_client_message);
        }

        let sent_server_message = SharedMessage::ChatMessage("Server response".to_string());
        {
            let server = server_app.world.resource::<Server>();
            server
                .broadcast_message(sent_server_message.clone())
                .unwrap();
        }

        // Server->Client Message
        sleep(Duration::from_secs_f32(0.1));
        client_app.update();

        {
            let mut client = client_app.world.resource_mut::<Client>();
            let server_message = client
                .connection_mut()
                .receive_message::<SharedMessage>()
                .expect("Failed to receive server message")
                .expect("There should be a server message");
            assert_eq!(server_message, sent_server_message);
        }
    }

    fn build_client_app() -> App {
        let mut client_app = App::new();
        client_app
            .add_plugin(ScheduleRunnerPlugin::default())
            .add_plugin(QuinnetClientPlugin::default())
            .insert_resource(ClientTestData::default())
            .add_startup_system(start_simple_connection)
            .add_system(handle_client_events);
        client_app
    }

    fn build_server_app() -> App {
        let mut server_app = App::new();
        server_app
            .add_plugin(ScheduleRunnerPlugin::default())
            .add_plugin(QuinnetServerPlugin::default())
            .insert_resource(ServerTestData::default())
            .add_startup_system(start_listening)
            .add_system(handle_server_events);
        server_app
    }

    fn default_client_configuration() -> ConnectionConfiguration {
        ConnectionConfiguration::new(
            SERVER_HOST.to_string(),
            SERVER_PORT,
            "0.0.0.0".to_string(),
            0,
        )
    }

    fn start_simple_connection(mut client: ResMut<Client>) {
        client.open_connection(
            default_client_configuration(),
            CertificateVerificationMode::SkipVerification,
        );
    }

    fn start_listening(mut server: ResMut<Server>) {
        server
            .start(
                ServerConfigurationData::new(
                    SERVER_HOST.to_string(),
                    SERVER_PORT,
                    "0.0.0.0".to_string(),
                ),
                CertificateRetrievalMode::GenerateSelfSigned,
            )
            .unwrap();
    }

    fn handle_client_events(
        mut connection_events: EventReader<client::ConnectionEvent>,
        mut test_data: ResMut<ClientTestData>,
    ) {
        for _connected_event in connection_events.iter() {
            test_data.connection_events_received += 1;
        }
    }

    fn handle_server_events(
        mut connection_events: EventReader<server::ConnectionEvent>,
        mut test_data: ResMut<ServerTestData>,
    ) {
        for connected_event in connection_events.iter() {
            test_data.connection_events_received += 1;
            test_data.last_connected_client_id = connected_event.id;
        }
    }
}
