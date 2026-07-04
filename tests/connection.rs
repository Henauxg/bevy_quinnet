use bevy_quinnet::{client::QuinnetClient, server::QuinnetServer};

// https://github.com/rust-lang/rust/issues/46379
pub use utils::*;

mod utils;

///////////////////////////////////////////////////////////
///                                                     ///
///                        Test                         ///
///                                                     ///
///////////////////////////////////////////////////////////

#[test]
fn connection_with_two_apps() {
    let (mut client_app, mut server_app, port) = start_test_pair();
    assert_ne!(port, 0, "Server should bind to an OS-assigned port");

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

    let client_id = wait_for_client_connected(&mut client_app, &mut server_app);

    assert_eq!(
        server_app
            .world()
            .resource::<ServerTestData>()
            .connection_events_received,
        1
    );

    assert!(
        client_app
            .world()
            .resource::<QuinnetClient>()
            .is_connected(),
        "The default connection should be connected to the server"
    );
    assert_eq!(
        client_app
            .world()
            .resource::<ClientTestData>()
            .connection_events_received,
        1
    );

    client_app
        .world_mut()
        .resource_mut::<QuinnetClient>()
        .connection_mut()
        .send_payload(TEST_MESSAGE_PAYLOAD)
        .unwrap();

    let default_server_channel_id = get_default_server_channel(&server_app);

    let client_payload =
        wait_for_client_message(client_id, default_server_channel_id, &mut server_app);
    assert_eq!(client_payload.iter().as_slice(), TEST_MESSAGE_PAYLOAD);

    server_app
        .world_mut()
        .resource_mut::<QuinnetServer>()
        .endpoint_mut()
        .broadcast_payload(TEST_MESSAGE_PAYLOAD)
        .unwrap();

    let server_message = wait_for_server_message(&mut client_app, default_server_channel_id);
    assert_eq!(server_message.iter().as_slice(), TEST_MESSAGE_PAYLOAD);
}

///////////////////////////////////////////////////////////
///                                                     ///
///                        Test                         ///
///                                                     ///
///////////////////////////////////////////////////////////

#[test]
fn reconnection() {
    let (mut client_app, mut server_app, _port) = start_test_pair();

    let client_id_1 = wait_for_client_connected(&mut client_app, &mut server_app);

    assert_eq!(
        server_app
            .world()
            .resource::<ServerTestData>()
            .connection_events_received,
        1
    );

    assert_eq!(
        client_app
            .world()
            .resource::<ClientTestData>()
            .connection_events_received,
        1
    );

    client_app
        .world_mut()
        .resource_mut::<QuinnetClient>()
        .connection_mut()
        .disconnect()
        .unwrap();

    let last_disconnected_client_id = wait_for_all_clients_disconnected(&mut server_app);
    assert_eq!(last_disconnected_client_id, client_id_1);

    client_app
        .world_mut()
        .resource_mut::<QuinnetClient>()
        .connection_mut()
        .reconnect()
        .unwrap();

    let client_id_2 = wait_for_client_connected(&mut client_app, &mut server_app);

    assert_ne!(
        client_id_1, client_id_2,
        "The two connections should be assigned a different client id"
    );

    assert_eq!(
        server_app
            .world()
            .resource::<ServerTestData>()
            .connection_events_received,
        2
    );

    assert_eq!(
        client_app
            .world()
            .resource::<ClientTestData>()
            .connection_events_received,
        2
    );
}
