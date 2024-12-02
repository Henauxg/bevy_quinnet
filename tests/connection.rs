use std::{thread::sleep, time::Duration};

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
    let port = 6000; // TODO Use port 0 and retrieve the port used by the server.

    let mut client_app = start_simple_client_app(port);
    let mut server_app = start_simple_server_app(port);

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

    let sent_client_message = SharedMessage::TestMessage("Test message content".to_string());
    client_app
        .world_mut()
        .resource_mut::<QuinnetClient>()
        .connection_mut()
        .send_message(sent_client_message.clone())
        .unwrap();

    // Client->Server Message
    sleep(Duration::from_secs_f32(0.1));
    server_app.update();

    let (_channel_id, client_message) = server_app
        .world_mut()
        .resource_mut::<QuinnetServer>()
        .endpoint_mut()
        .receive_message_from::<SharedMessage>(client_id)
        .expect("Failed to receive client message")
        .expect("There should be a client message");
    assert_eq!(client_message, sent_client_message);

    let sent_server_message = SharedMessage::TestMessage("Server response".to_string());

    server_app
        .world_mut()
        .resource_mut::<QuinnetServer>()
        .endpoint_mut()
        .broadcast_message(sent_server_message.clone())
        .unwrap();

    // Server->Client Message
    sleep(Duration::from_secs_f32(0.1));
    client_app.update();

    let (_channel_id, server_message) = client_app
        .world_mut()
        .resource_mut::<QuinnetClient>()
        .connection_mut()
        .receive_message::<SharedMessage>()
        .expect("Failed to receive server message")
        .expect("There should be a server message");
    assert_eq!(server_message, sent_server_message);
}

///////////////////////////////////////////////////////////
///                                                     ///
///                        Test                         ///
///                                                     ///
///////////////////////////////////////////////////////////

#[test]
fn reconnection() {
    let port = 6005; // TODO Use port 0 and retrieve the port used by the server.

    let mut client_app = start_simple_client_app(port);
    let mut server_app = start_simple_server_app(port);

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
