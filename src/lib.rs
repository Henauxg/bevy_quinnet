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
            self,
            certificate::{
                CertConnectionAbortEvent, CertInteractionEvent, CertTrustUpdateEvent,
                CertVerificationInfo, CertVerificationStatus, CertVerifierAction,
                CertificateVerificationMode,
            },
            Client, ConnectionConfiguration, QuinnetClientPlugin,
        },
        server::{
            self,
            certificate::{CertificateRetrievalMode, CertificateRetrievedEvent},
            QuinnetServerPlugin, Server, ServerConfigurationData,
        },
        shared::{CertificateFingerprint, ClientId},
    };
    use bevy::{
        app::ScheduleRunnerPlugin,
        prelude::{App, EventReader, ResMut, Resource},
    };
    use serde::{Deserialize, Serialize};

    const SERVER_HOST: &str = "127.0.0.1";
    const SERVER_PORT: u16 = 6000;
    const TEST_CERT_FILE: &str = "assets/tests/test_cert.pem";
    const TEST_KEY_FILE: &str = "assets/tests/test_key.pem";
    const TEST_CERT_FINGERPRINT_B64: &str = "sieQJ9J6DIrQP37HAlUFk2hYhLZDY9G5OZQpqzkWlKo=";

    #[derive(Resource, Debug, Clone, Default)]
    struct ClientTestData {
        connection_events_received: u64,

        cert_trust_update_events_received: u64,
        last_trusted_cert_info: Option<CertVerificationInfo>,

        cert_interactions_received: u64,
        last_cert_interactions_status: Option<CertVerificationStatus>,
        last_cert_interactions_info: Option<CertVerificationInfo>,

        cert_verif_connection_abort_events_received: u64,
        last_abort_cert_status: Option<CertVerificationStatus>,
        last_abort_cert_info: Option<CertVerificationInfo>,
    }

    #[derive(Resource, Debug, Clone, Default)]
    struct ServerTestData {
        connection_events_received: u64,
        last_connected_client_id: Option<ClientId>,
        cert_events_received: u64,
        last_cert_fingeprint: Option<CertificateFingerprint>,
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
            assert_eq!(
                client_id,
                server_test_data
                    .last_connected_client_id
                    .expect("A client should have connected")
            );
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

    #[test]
    fn trust_on_first_use() {
        // TOFU With default parameters
        // Server listens with a cert loaded from a file
        // Client connects with empty cert store
        // -> The server's certificate is treatead as Unknown by the client, which stores it and continues the connection
        // Clients disconnects
        // Client reconnects with the updated cert store
        // -> The server's certificate is treatead as Trusted by the client, which continues the connection
        // Clients disconnects
        // Server reboots, and generates a new self-signed certificate
        // Client reconnects with its cert store
        // -> The server's certificate is treatead as Untrusted by the client, which requests a client action
        // We receive the client action request and ask to abort the connection

        let mut client_app = App::new();
        client_app
            .add_plugin(ScheduleRunnerPlugin::default())
            .add_plugin(QuinnetClientPlugin::default())
            .insert_resource(ClientTestData::default())
            .add_system(handle_client_events);

        let mut server_app = App::new();
        server_app
            .add_plugin(ScheduleRunnerPlugin::default())
            .add_plugin(QuinnetServerPlugin::default())
            .insert_resource(ServerTestData::default())
            .add_system(handle_server_events);

        // Startup
        client_app.update();
        server_app.update();

        // Server listens with a cert loaded from a file
        {
            let mut server = server_app.world.resource_mut::<Server>();
            server
                .start(
                    ServerConfigurationData::new(
                        SERVER_HOST.to_string(),
                        SERVER_PORT,
                        "0.0.0.0".to_string(),
                    ),
                    CertificateRetrievalMode::LoadFromFile {
                        cert_file: TEST_CERT_FILE.to_string(),
                        key_file: TEST_KEY_FILE.to_string(),
                    },
                )
                .unwrap();
        }

        // Client connects with empty cert store
        {
            let mut client = client_app.world.resource_mut::<Client>();
            client.open_connection(
                default_client_configuration(),
                CertificateVerificationMode::TrustOnFirstUse(
                    client::certificate::TrustOnFirstUseConfig {
                        ..Default::default()
                    },
                ),
            );
        }

        // Let the async runtime connection connect.
        sleep(Duration::from_secs_f32(0.1));

        // Connection & event propagation
        server_app.update();
        client_app.update();

        {
            let mut server_test_data = server_app.world.resource_mut::<ServerTestData>();
            assert_eq!(
                TEST_CERT_FINGERPRINT_B64.to_string(),
                server_test_data
                    .last_cert_fingeprint
                    .as_mut()
                    .expect("A certificate should have been loaded")
                    .to_base64()
            );
            assert_eq!(server_test_data.cert_events_received, 1);
        }

        // The server's certificate is treatead as Unknown by the client, which stores it and continues the connection
        {
            let mut client_test_data = client_app.world.resource_mut::<ClientTestData>();
            assert_eq!(client_test_data.cert_trust_update_events_received, 1);
            let cert_info = client_test_data
                .last_trusted_cert_info
                .as_mut()
                .expect("A certificate trust update should have happened");
            assert_eq!(
                cert_info.fingerprint.to_base64(),
                TEST_CERT_FINGERPRINT_B64.to_string()
            );
            assert!(cert_info.known_fingerprint.is_none());
            assert_eq!(cert_info.server_name.to_string(), SERVER_HOST.to_string());

            let mut client = client_app.world.resource_mut::<Client>();
            assert!(
                client.connection().is_connected(),
                "The default connection should be connected to the server"
            );

            // Clients disconnects
            // Client reconnects with the updated cert store
            client
                .close_all_connections()
                .expect("Failed to close connections on the client");

            client.open_connection(
                default_client_configuration(),
                CertificateVerificationMode::TrustOnFirstUse(
                    client::certificate::TrustOnFirstUseConfig {
                        ..Default::default()
                    },
                ),
            );
        }

        // Let the async runtime connection connect.
        sleep(Duration::from_secs_f32(0.1));

        // Connection & event propagation
        server_app.update();
        client_app.update();

        {
            let client_test_data = client_app.world.resource::<ClientTestData>();
            assert_eq!(client_test_data.cert_trust_update_events_received, 1);

            let mut client = client_app.world.resource_mut::<Client>();
            assert!(
                client.connection().is_connected(),
                "The default connection should be connected to the server"
            );

            // Clients disconnects
            client
                .close_all_connections()
                .expect("Failed to close connections on the client");
        }

        // Server reboots, and generates a new self-signed certificate
        // TODO Close server endpoint here

        {
            let mut server = server_app.world.resource_mut::<Server>();
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

        // Client reconnects with its cert store
        {
            let mut client = client_app.world.resource_mut::<Client>();
            assert!(
                client.connection().is_connected() == false,
                "The default connection should be disconnected from the server"
            );
            client.open_connection(
                default_client_configuration(),
                CertificateVerificationMode::TrustOnFirstUse(
                    client::certificate::TrustOnFirstUseConfig {
                        ..Default::default()
                    },
                ),
            );
        }

        // Let the async runtime connection connect.
        sleep(Duration::from_secs_f32(0.1));

        // Connection & event propagation
        server_app.update();
        client_app.update();

        let mut server_test_data = server_app.world.resource_mut::<ServerTestData>();
        assert_eq!(server_test_data.cert_events_received, 1);
        let generated_cert_fingerprint = server_test_data
            .last_cert_fingeprint
            .as_mut()
            .expect("The server should have generated a self-signed certificate");

        // The server's certificate is treatead as Untrusted by the client, which requests a client action
        // We received the client action request and asked to abort the connection
        {
            let mut client_test_data = client_app.world.resource_mut::<ClientTestData>();
            assert_eq!(client_test_data.cert_interactions_received, 1);
            assert_eq!(
                client_test_data.cert_verif_connection_abort_events_received,
                1
            );

            // Verify the cert info in the certificate interaction event
            let interaction_cert_info = client_test_data
                .last_cert_interactions_info
                .as_mut()
                .expect("A certificate interaction event should have happened during certificate verification");
            // Verify that the cert we received is the one generated by the server
            assert_eq!(
                interaction_cert_info.fingerprint.to_base64(),
                generated_cert_fingerprint.to_base64()
            );
            // Verify the known fingerprint
            assert_eq!(
                interaction_cert_info
                    .known_fingerprint
                    .as_mut()
                    .expect("There should be a known fingerprint in the store")
                    .to_base64(),
                TEST_CERT_FINGERPRINT_B64.to_string()
            );
            assert_eq!(
                interaction_cert_info.server_name.to_string(),
                SERVER_HOST.to_string()
            );
            assert_eq!(
                client_test_data.last_cert_interactions_status,
                Some(CertVerificationStatus::UntrustedCertificate)
            );

            // Verify the cert info in the connection abort event
            assert_eq!(
                client_test_data.last_abort_cert_info,
                client_test_data.last_cert_interactions_info
            );
            assert_eq!(
                client_test_data.last_abort_cert_status,
                Some(CertVerificationStatus::UntrustedCertificate)
            );

            let client = client_app.world.resource::<Client>();
            assert!(
                client.connection().is_connected() == false,
                "The default connection should not be connected to the server"
            );
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
        mut cert_trust_update_events: EventReader<CertTrustUpdateEvent>,
        mut cert_interaction_events: EventReader<CertInteractionEvent>,
        mut cert_connection_abort_events: EventReader<CertConnectionAbortEvent>,
        mut test_data: ResMut<ClientTestData>,
    ) {
        for _connected_event in connection_events.iter() {
            test_data.connection_events_received += 1;
        }
        for trust_update in cert_trust_update_events.iter() {
            test_data.cert_trust_update_events_received += 1;
            test_data.last_trusted_cert_info = Some(trust_update.cert_info.clone());
        }
        for cert_interaction in cert_interaction_events.iter() {
            test_data.cert_interactions_received += 1;
            test_data.last_cert_interactions_status = Some(cert_interaction.status.clone());
            test_data.last_cert_interactions_info = Some(cert_interaction.info.clone());

            match cert_interaction.status {
                CertVerificationStatus::UnknownCertificate => todo!(),
                CertVerificationStatus::UntrustedCertificate => {
                    cert_interaction
                        .apply_cert_verifier_action(CertVerifierAction::AbortConnection)
                        .expect("Failed to apply vert verification action");
                }
                CertVerificationStatus::TrustedCertificate => todo!(),
            }
        }
        for connection_abort in cert_connection_abort_events.iter() {
            test_data.cert_verif_connection_abort_events_received += 1;
            test_data.last_abort_cert_status = Some(connection_abort.status.clone());
            test_data.last_abort_cert_info = Some(connection_abort.cert_info.clone());
        }
    }

    fn handle_server_events(
        mut connection_events: EventReader<server::ConnectionEvent>,
        mut cert_events: EventReader<CertificateRetrievedEvent>,
        mut test_data: ResMut<ServerTestData>,
    ) {
        for connected_event in connection_events.iter() {
            test_data.connection_events_received += 1;
            test_data.last_connected_client_id = Some(connected_event.id);
        }
        for cert_event in cert_events.iter() {
            test_data.cert_events_received += 1;
            test_data.last_cert_fingeprint = Some(cert_event.fingerprint.clone());
        }
    }
}
