use bevy::{
    app::ScheduleRunnerPlugin,
    prelude::{App, Update},
};
use bevy_quinnet::{
    client::{
        certificate::{CertVerificationStatus, CertificateVerificationMode},
        ClientConnectionConfiguration, QuinnetClient,
    },
    server::{
        certificate::CertificateRetrievalMode, EndpointAddrConfiguration, QuinnetServer,
        QuinnetServerPlugin, ServerEndpointConfiguration,
    },
};

// https://github.com/rust-lang/rust/issues/46379
pub use utils::*;

mod utils;

///////////////////////////////////////////////////////////
///                                                     ///
///                        Test                         ///
///                                                     ///
///////////////////////////////////////////////////////////

const TEST_CERT_FILE: &str = "tests/fixtures/cert.pem.test";
const TEST_KEY_FILE: &str = "tests/fixtures/key.pem.test";
const TEST_CERT_FINGERPRINT_B64: &str = "sieQJ9J6DIrQP37HAlUFk2hYhLZDY9G5OZQpqzkWlKo=";

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

    let hosts = test_hosts_file("trust_on_first_use");
    clear_hosts_file(&hosts);

    let mut client_app = build_cert_client_app();

    let mut server_app = App::new();
    server_app
        .add_plugins((
            ScheduleRunnerPlugin::default(),
            QuinnetServerPlugin::default(),
        ))
        .insert_resource(ServerTestData::default())
        .add_systems(Update, handle_server_events);

    // Startup
    client_app.update();
    server_app.update();

    // Server listens with a cert loaded from a file
    {
        let mut server = server_app.world_mut().resource_mut::<QuinnetServer>();
        let server_cert = server
            .start_endpoint(ServerEndpointConfiguration {
                addr_config: EndpointAddrConfiguration::from_ip(LOCAL_BIND_IP, 0),
                cert_mode: CertificateRetrievalMode::LoadFromFile {
                    cert_file: TEST_CERT_FILE.to_string(),
                    key_file: TEST_KEY_FILE.to_string(),
                },
                defaultables: Default::default(),
            })
            .unwrap();
        assert_eq!(
            TEST_CERT_FINGERPRINT_B64.to_string(),
            server_cert.fingerprint.to_base64(),
            "The loaded cert fingerprint should match the known test fingerprint"
        );
    }

    let port = server_listen_port(&server_app);

    // Client connects with empty cert store
    {
        let mut client = client_app.world_mut().resource_mut::<QuinnetClient>();
        client
            .open_connection(ClientConnectionConfiguration {
                addr_config: default_client_addr_configuration(port),
                cert_mode: CertificateVerificationMode::TrustOnFirstUse(tofu_with_hosts(
                    &hosts,
                )),
                // cert_mode: CertificateVerificationMode::SkipVerification,
                defaultables: Default::default(),
            })
            .unwrap();
    }

    assert!(
        client_app
            .world()
            .resource::<QuinnetClient>()
            .get_connection()
            .is_some(),
        "The default connection should exist"
    );
    let server = server_app.world().resource::<QuinnetServer>();
    assert!(server.is_listening(), "The server should be listening");

    // Let the async runtime connection connect.
    wait_for_client_connected(&mut client_app, &mut server_app);

    // The server's certificate is treatead as Unknown by the client, which stores it and continues the connection
    {
        let mut client_test_data = client_app.world_mut().resource_mut::<ClientTestData>();
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
            SERVER_IP.to_string(),
            "The server name should match the one we configured"
        );
        assert_eq!(
            client_test_data.cert_interactions_received, 0,
            "Unknown cert uses immediate TrustAndStore, not client interaction"
        );

        let mut client = client_app.world_mut().resource_mut::<QuinnetClient>();
        assert!(
            client.is_connected(),
            "The default connection should be connected to the server"
        );

        // Clients disconnects
        // Client reconnects with the updated cert store
        client.close_all_connections();

        client
            .open_connection(ClientConnectionConfiguration {
                addr_config: default_client_addr_configuration(port),
                cert_mode: CertificateVerificationMode::TrustOnFirstUse(tofu_with_hosts(
                    &hosts,
                )),
                defaultables: Default::default(),
            })
            .unwrap();
    }

    // Let the async runtime connection connect.
    wait_for_client_connected(&mut client_app, &mut server_app);

    {
        assert!(
            client_app
                .world_mut()
                .resource_mut::<QuinnetClient>()
                .is_connected(),
            "The default connection should be connected to the server"
        );

        let client_test_data = client_app.world().resource::<ClientTestData>();
        assert_eq!(client_test_data.cert_trust_update_events_received, 1, "The client should still have only 1 certificate trust update event after his reconnection");
        assert_eq!(
            client_test_data.cert_interactions_received, 0,
            "Trusted cert uses immediate TrustOnce, not client interaction"
        );

        // Clients disconnects
        client_app
            .world_mut()
            .resource_mut::<QuinnetClient>()
            .close_all_connections();
    }

    // Server reboots, and generates a new self-signed certificate
    server_app
        .world_mut()
        .resource_mut::<QuinnetServer>()
        .stop_endpoint()
        .unwrap();

    // Let the endpoint fully stop and the port be released.
    wait_for_udp_port_available(port, Some(&mut server_app));

    let server_cert = server_app
        .world_mut()
        .resource_mut::<QuinnetServer>()
        .start_endpoint(ServerEndpointConfiguration {
            addr_config: EndpointAddrConfiguration::from_ip(LOCAL_BIND_IP, port),
            cert_mode: CertificateRetrievalMode::GenerateSelfSigned {
                server_hostname: SERVER_IP.to_string(),
            },
            defaultables: Default::default(),
        })
        .unwrap();

    // Client reconnects with its cert store containing the previously store certificate fingerprint
    {
        let mut client = client_app.world_mut().resource_mut::<QuinnetClient>();
        client
            .open_connection(ClientConnectionConfiguration {
                addr_config: default_client_addr_configuration(port),
                cert_mode: CertificateVerificationMode::TrustOnFirstUse(tofu_with_hosts(
                    &hosts,
                )),
                defaultables: Default::default(),
            })
            .unwrap();
    }

    // Let the async runtime process the certificate action and connection abort.
    poll_apps_until(
        "cert abort event",
        Some(&mut client_app),
        Some(&mut server_app),
        DEFAULT_TEST_TIMEOUT,
        DEFAULT_POLL_INTERVAL,
        |client, _| {
            let client = client?;
            if client
                .world()
                .resource::<ClientTestData>()
                .cert_verif_connection_abort_events_received
                > 0
            {
                Some(())
            } else {
                None
            }
        },
    );

    // The server's certificate is treatead as Untrusted by the client, which requests a client action
    // We received the client action request and asked to abort the connection
    {
        let mut client_test_data = client_app.world_mut().resource_mut::<ClientTestData>();
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
            .expect(
            "A certificate interaction event should have happened during certificate verification",
        );
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
            SERVER_IP.to_string(),
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

        let client = client_app.world().resource::<QuinnetClient>();
        assert!(
            client.is_connected() == false,
            "The default connection should not be connected to the server"
        );
    }

    // Leave the workspace clean
    let hosts = test_hosts_file("trust_on_first_use");
    clear_hosts_file(&hosts);
}

#[test]
fn tofu_unknown_certificate_client_action() {
    // Unknown cert with RequestClientAction (not default immediate TrustAndStore)
    // Client connects with empty cert store
    // -> CertInteractionEvent(UnknownCertificate); handler applies TrustAndStore
    // -> Connection succeeds and CertTrustUpdateEvent is raised

    let hosts = test_hosts_file("unknown");
    clear_hosts_file(&hosts);

    let (mut server_app, port) = start_loaded_cert_server(TEST_CERT_FILE, TEST_KEY_FILE);
    let mut client_app = build_cert_client_app();

    open_tofu_connection(
        &mut client_app,
        port,
        tofu_config_requesting_client_action(
            CertVerificationStatus::UnknownCertificate,
            &hosts,
        ),
    );
    wait_for_cert_interaction(
        &mut client_app,
        &mut server_app,
        CertVerificationStatus::UnknownCertificate,
    );
    wait_for_client_connected(&mut client_app, &mut server_app);

    let data = client_app.world().resource::<ClientTestData>();
    assert_eq!(data.cert_interactions_received, 1);
    assert_eq!(data.cert_trust_update_events_received, 1);
    assert_eq!(
        data.last_trusted_cert_info
            .as_ref()
            .unwrap()
            .fingerprint
            .to_base64(),
        TEST_CERT_FINGERPRINT_B64
    );

    clear_hosts_file(&hosts);
}

#[test]
fn tofu_trusted_certificate_client_action() {
    // Trusted cert with RequestClientAction (not default immediate TrustOnce)
    // Client connects once with default TOFU to populate the cert store, then disconnects
    // Client reconnects with TrustedCertificate configured as RequestClientAction
    // -> CertInteractionEvent(TrustedCertificate); handler applies TrustOnce
    // -> Connection succeeds without a new CertTrustUpdateEvent

    let hosts = test_hosts_file("trusted");
    clear_hosts_file(&hosts);

    let (mut server_app, port) = start_loaded_cert_server(TEST_CERT_FILE, TEST_KEY_FILE);
    let mut client_app = build_cert_client_app();

    open_tofu_connection(&mut client_app, port, tofu_with_hosts(&hosts));
    wait_for_client_connected(&mut client_app, &mut server_app);
    client_app
        .world_mut()
        .resource_mut::<QuinnetClient>()
        .close_all_connections();

    open_tofu_connection(
        &mut client_app,
        port,
        tofu_config_requesting_client_action(
            CertVerificationStatus::TrustedCertificate,
            &hosts,
        ),
    );
    wait_for_cert_interaction(
        &mut client_app,
        &mut server_app,
        CertVerificationStatus::TrustedCertificate,
    );
    wait_for_client_connected(&mut client_app, &mut server_app);

    let data = client_app.world().resource::<ClientTestData>();
    assert_eq!(data.cert_interactions_received, 1);
    assert_eq!(data.cert_trust_update_events_received, 1);
    assert!(client_app
        .world()
        .resource::<QuinnetClient>()
        .is_connected());

    clear_hosts_file(&hosts);
}

fn start_loaded_cert_server(cert_file: &str, key_file: &str) -> (App, u16) {
    let mut server_app = App::new();
    server_app
        .add_plugins((
            ScheduleRunnerPlugin::default(),
            QuinnetServerPlugin::default(),
        ))
        .insert_resource(ServerTestData::default())
        .add_systems(Update, handle_server_events);
    server_app.update();
    server_app
        .world_mut()
        .resource_mut::<QuinnetServer>()
        .start_endpoint(ServerEndpointConfiguration {
            addr_config: EndpointAddrConfiguration::from_ip(LOCAL_BIND_IP, 0),
            cert_mode: CertificateRetrievalMode::LoadFromFile {
                cert_file: cert_file.to_string(),
                key_file: key_file.to_string(),
            },
            defaultables: Default::default(),
        })
        .unwrap();
    let port = server_listen_port(&server_app);
    (server_app, port)
}
