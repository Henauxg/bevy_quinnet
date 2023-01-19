use std::{thread::sleep, time::Duration};

use bevy_quinnet::{client::Client, server::Server};

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

    {
        let client = client_app.world.resource::<Client>();
        assert!(
            client.get_connection().is_some(),
            "The default connection should exist"
        );
        let server = server_app.world.resource::<Server>();
        assert!(server.is_listening(), "The server should be listening");
    }

    let client_id = wait_for_client_connected(&mut client_app, &mut server_app);

    let sent_client_message = SharedMessage::TestMessage("Test message content".to_string());
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
        let client_message = server_app
            .world
            .resource_mut::<Server>()
            .endpoint_mut()
            .receive_message_from::<SharedMessage>(client_id)
            .expect("Failed to receive client message")
            .expect("There should be a client message");
        assert_eq!(client_message, sent_client_message);
    }

    let sent_server_message = SharedMessage::TestMessage("Server response".to_string());
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
