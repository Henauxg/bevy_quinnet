use bevy::prelude::App;

use bevy_quinnet::{
    client::{ClientPayloadSendError, QuinnetClient},
    server::{QuinnetServer, ServerGroupPayloadSendError},
    shared::channels::ChannelConfig,
};

// https://github.com/rust-lang/rust/issues/46379
pub use utils::*;

mod utils;

///////////////////////////////////////////////////////////
///                                                     ///
///                        Test                         ///
///                                                     ///
///////////////////////////////////////////////////////////

#[test]
fn default_channel() {
    let port = 6001; // TODO Use port 0 and retrieve the port used by the server.
    let mut server_app: App = start_simple_server_app(port);
    let mut client_app: App = start_simple_client_app(port);

    let client_default_channel = get_default_client_channel(&client_app);
    let server_default_channel = get_default_server_channel(&server_app);

    for channel in vec![client_default_channel, server_default_channel] {
        assert!(matches!(channel, 0), "Default channel id should be 0");
    }

    close_client_channel(client_default_channel, &mut client_app);
    close_server_channel(server_default_channel, &mut server_app);

    {
        let mut server = server_app.world_mut().resource_mut::<QuinnetServer>();
        assert_eq!(
            server.endpoint().get_default_channel(),
            None,
            "Default server channel should be None"
        );

        assert!(
            matches!(
                server
                    .endpoint_mut()
                    .broadcast_payload(TEST_MESSAGE_PAYLOAD),
                Err(ServerGroupPayloadSendError::NoDefaultChannel)
            ),
            "Should not be able to send on default channel"
        );
    }
    {
        let mut client = client_app.world_mut().resource_mut::<QuinnetClient>();
        assert_eq!(
            client.connection().get_default_channel(),
            None,
            "Default client channel should be None"
        );

        assert!(
            matches!(
                client.connection_mut().send_payload(TEST_MESSAGE_PAYLOAD),
                Err(ClientPayloadSendError::NoDefaultChannel)
            ),
            "Should not be able to send on default channel"
        );
    }

    let client_channel = open_client_channel(ChannelConfig::default(), &mut client_app);
    let server_channel = open_server_channel(ChannelConfig::default(), &mut server_app);

    let client_id = wait_for_client_connected(&mut client_app, &mut server_app);

    let mut msg_counter = 0;
    send_and_test_client_message(
        client_id,
        client_channel,
        &mut client_app,
        &mut server_app,
        &mut msg_counter,
    );
    send_and_test_server_message(
        client_id,
        server_channel,
        &mut server_app,
        &mut client_app,
        &mut msg_counter,
    );
}

///////////////////////////////////////////////////////////
///                                                     ///
///                        Test                         ///
///                                                     ///
///////////////////////////////////////////////////////////

#[test]
fn multi_instance_channels() {
    let port = 6002; // TODO Use port 0 and retrieve the port used by the server.
    let mut server_app: App = start_simple_server_app(port);

    for channel_type in vec![
        ChannelConfig::default_ordered_reliable(),
        ChannelConfig::default_unordered_reliable(),
        ChannelConfig::default_unreliable(),
    ] {
        let mut client_app_1: App = start_simple_client_app(port);

        let client_id_1 = wait_for_client_connected(&mut client_app_1, &mut server_app);

        let client_1_channel_1 = get_default_client_channel(&client_app_1);
        let client_1_channel_2 = open_client_channel(channel_type, &mut client_app_1);

        let server_channel_1 = get_default_server_channel(&server_app);
        let server_channel_2 = open_server_channel(channel_type, &mut server_app);

        let mut msg_counter = 0;
        for channel in vec![client_1_channel_1, client_1_channel_2] {
            send_and_test_client_message(
                client_id_1,
                channel,
                &mut client_app_1,
                &mut server_app,
                &mut msg_counter,
            );
        }
        for channel in vec![server_channel_1, server_channel_2] {
            send_and_test_server_message(
                client_id_1,
                channel,
                &mut server_app,
                &mut client_app_1,
                &mut msg_counter,
            );
        }

        let mut client_app_2 = start_simple_client_app(port);
        let client_id_2 = wait_for_client_connected(&mut client_app_2, &mut server_app);

        for (client_id, mut client_app) in
            vec![(client_id_1, client_app_1), (client_id_2, client_app_2)]
        {
            send_and_test_server_message(
                client_id,
                server_channel_1,
                &mut server_app,
                &mut client_app,
                &mut msg_counter,
            );
            send_and_test_server_message(
                client_id,
                server_channel_2,
                &mut server_app,
                &mut client_app,
                &mut msg_counter,
            );
        }
    }
}
