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
    use std::{fs, thread::sleep, time::Duration};

    use crate::{
        client::{
            self,
            certificate::{
                CertConnectionAbortEvent, CertInteractionEvent, CertTrustUpdateEvent,
                CertVerificationInfo, CertVerificationStatus, CertVerifierAction,
                CertificateVerificationMode,
            },
            Client, ConnectionConfiguration, QuinnetClientPlugin, DEFAULT_KNOWN_HOSTS_FILE,
        },
        server::{
            self, certificate::CertificateRetrievalMode, QuinnetServerPlugin, Server,
            ServerConfigurationData,
        },
        shared::ClientId,
    };
    use bevy::{
        app::ScheduleRunnerPlugin,
        prelude::{App, EventReader, Res, ResMut, Resource},
    };
    use serde::{Deserialize, Serialize};

    const SERVER_HOST: &str = "127.0.0.1";
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
    }

    #[derive(Resource, Debug, Clone, Default)]
    struct Port(u16);

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub enum SharedMessage {
        ChatMessage(String),
    }

    #[test]
    fn connection_with_two_apps() {
        let port = 6000; // TODO Use port 0 and retrieve the port used by the server.

        let mut client_app = build_client_app();
        client_app.insert_resource(Port(port));
        let mut server_app = build_server_app();
        server_app.insert_resource(Port(port));

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
                .endpoint_mut()
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
                .endpoint()
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

        let port = 6001; // TODO Use port 0 and retrieve the port used by the server.

        fs::remove_file(DEFAULT_KNOWN_HOSTS_FILE)
            .expect("Failed to remove default known hosts file");

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
            let server_cert = server
                .start_endpoint(
                    ServerConfigurationData::new(
                        SERVER_HOST.to_string(),
                        port,
                        "0.0.0.0".to_string(),
                    ),
                    CertificateRetrievalMode::LoadFromFile {
                        cert_file: TEST_CERT_FILE.to_string(),
                        key_file: TEST_KEY_FILE.to_string(),
                    },
                )
                .unwrap();
            assert_eq!(
                TEST_CERT_FINGERPRINT_B64.to_string(),
                server_cert.fingerprint.to_base64(),
                "The loaded cert fingerprint should match the known test fingerprint"
            );
        }

        // Client connects with empty cert store
        {
            let mut client = client_app.world.resource_mut::<Client>();
            client.open_connection(
                default_client_configuration(port),
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

        // The server's certificate is treatead as Unknown by the client, which stores it and continues the connection
        {
            let mut client_test_data = client_app.world.resource_mut::<ClientTestData>();
            assert_eq!(
                client_test_data.cert_trust_update_events_received, 1,
                "The client should have received exactly 1 certificate trust update event"
            );
            let cert_info = client_test_data
                .last_trusted_cert_info
                .as_mut()
                .expect("A certificate trust update should have happened");
            assert_eq!(
                cert_info.fingerprint.to_base64(),
                TEST_CERT_FINGERPRINT_B64.to_string(),
                "The certificate rceived by the client should match the known test certificate"
            );
            assert!(
                cert_info.known_fingerprint.is_none(),
                "The client should not have any previous certificate fingerprint for this server"
            );
            assert_eq!(
                cert_info.server_name.to_string(),
                SERVER_HOST.to_string(),
                "The server name should match the one we configured"
            );

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
                default_client_configuration(port),
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
            assert!(
                client_app
                    .world
                    .resource_mut::<Client>()
                    .connection()
                    .is_connected(),
                "The default connection should be connected to the server"
            );

            let client_test_data = client_app.world.resource::<ClientTestData>();
            assert_eq!(client_test_data.cert_trust_update_events_received, 1, "The client should still have only 1 certificate trust update event after his reconnection");

            // Clients disconnects
            client_app
                .world
                .resource_mut::<Client>()
                .close_all_connections()
                .expect("Failed to close connections on the client");
        }

        // Server reboots, and generates a new self-signed certificate
        server_app
            .world
            .resource_mut::<Server>()
            .stop_endpoint()
            .unwrap();

        // Let the endpoint fully stop.
        sleep(Duration::from_secs_f32(0.1));

        let server_cert = server_app
            .world
            .resource_mut::<Server>()
            .start_endpoint(
                ServerConfigurationData::new(SERVER_HOST.to_string(), port, "0.0.0.0".to_string()),
                CertificateRetrievalMode::GenerateSelfSigned,
            )
            .unwrap();

        // Client reconnects with its cert store containing the previously store certificate fingerprint
        {
            let mut client = client_app.world.resource_mut::<Client>();
            client.open_connection(
                default_client_configuration(port),
                CertificateVerificationMode::TrustOnFirstUse(
                    client::certificate::TrustOnFirstUseConfig {
                        ..Default::default()
                    },
                ),
            );
        }

        // Let the async runtime connection connect.
        sleep(Duration::from_secs_f32(0.1));

        // Connection & event propagation: certificate interaction event
        server_app.update();
        client_app.update();

        // Let the async runtime process the certificate action & connection.
        sleep(Duration::from_secs_f32(0.1));

        // Connectino abort event
        client_app.update();

        // The server's certificate is treatead as Untrusted by the client, which requests a client action
        // We received the client action request and asked to abort the connection
        {
            let mut client_test_data = client_app.world.resource_mut::<ClientTestData>();
            assert_eq!(
                client_test_data.cert_interactions_received, 1,
                "The client should have received exactly 1 certificate interaction event"
            );
            assert_eq!(
                client_test_data.cert_verif_connection_abort_events_received, 1,
                "The client should have received exactly 1 certificate connection abort event"
            );

            // Verify the cert info in the certificate interaction event
            let interaction_cert_info = client_test_data
                .last_cert_interactions_info
                .as_mut()
                .expect("A certificate interaction event should have happened during certificate verification");
            assert_eq!(
                interaction_cert_info.fingerprint.to_base64(),
                server_cert.fingerprint.to_base64(),
                "The fingerprint received by the client should match the one generated by the server"
            );
            // Verify the known fingerprint
            assert_eq!(
                interaction_cert_info
                    .known_fingerprint
                    .as_mut()
                    .expect("There should be a known fingerprint in the store")
                    .to_base64(),
                TEST_CERT_FINGERPRINT_B64.to_string(),
                "The previously known fingeprint for this server should be the test fingerprint"
            );
            assert_eq!(
                interaction_cert_info.server_name.to_string(),
                SERVER_HOST.to_string(),
                "The server name in the certificate interaction event should be the server we want to connect to"
            );
            assert_eq!(
                client_test_data.last_cert_interactions_status,
                Some(CertVerificationStatus::UntrustedCertificate),
                "The certificate verification status in the certificate interaction event should be `Untrusted`"
            );

            // Verify the cert info in the connection abort event
            assert_eq!(
                client_test_data.last_abort_cert_info,
                client_test_data.last_cert_interactions_info,
                "The certificate info in the connection abort event should match those of the certificate interaction event"
            );
            assert_eq!(
                client_test_data.last_abort_cert_status,
                Some(CertVerificationStatus::UntrustedCertificate),
                "The certificate verification status in the connection abort event should be `Untrusted`"
            );

            let client = client_app.world.resource::<Client>();
            assert!(
                client.connection().is_connected() == false,
                "The default connection should not be connected to the server"
            );
        }

        // Leave the workspace clean
        fs::remove_file(DEFAULT_KNOWN_HOSTS_FILE)
            .expect("Failed to remove default known hosts file");
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

    fn default_client_configuration(port: u16) -> ConnectionConfiguration {
        ConnectionConfiguration::new(SERVER_HOST.to_string(), port, "0.0.0.0".to_string(), 0)
    }

    fn start_simple_connection(mut client: ResMut<Client>, port: Res<Port>) {
        client.open_connection(
            default_client_configuration(port.0),
            CertificateVerificationMode::SkipVerification,
        );
    }

    fn start_listening(mut server: ResMut<Server>, port: Res<Port>) {
        server
            .start_endpoint(
                ServerConfigurationData::new(
                    SERVER_HOST.to_string(),
                    port.0,
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
                        .expect("Failed to apply cert verification action");
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
        mut test_data: ResMut<ServerTestData>,
    ) {
        for connected_event in connection_events.iter() {
            test_data.connection_events_received += 1;
            test_data.last_connected_client_id = Some(connected_event.id);
        }
    }
}
