use std::{
    net::{IpAddr, Ipv4Addr},
    thread::sleep,
    time::Duration,
};

use bevy::prelude::*;
use bevy_quinnet::{
    client::{
        certificate::CertificateVerificationMode, connection::ConnectionConfiguration,
        QuinnetClient,
    },
    server::{certificate::CertificateRetrievalMode, QuinnetServer, ServerConfiguration},
};
use bevy_replicon::prelude::*;
use bevy_replicon_quinnet::{ChannelsConfigurationExt, RepliconQuinnetPlugins};
use serde::{Deserialize, Serialize};

#[test]
fn connect_disconnect() {
    let port = 6000; // TODO Use port 0 and retrieve the port used by the server.
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            RepliconPlugins.set(ServerPlugin {
                tick_policy: TickPolicy::EveryFrame,
                ..Default::default()
            }),
            RepliconQuinnetPlugins,
        ));
    }

    setup(&mut server_app, &mut client_app, port);

    let mut quinnet_client = client_app.world.resource_mut::<QuinnetClient>();
    assert!(quinnet_client.is_connected());
    let default_connection = quinnet_client.get_default_connection().unwrap();
    quinnet_client.close_connection(default_connection).unwrap();

    client_app.update();

    let replicon_client = client_app.world.resource_mut::<RepliconClient>();
    assert!(replicon_client.is_disconnected());

    server_wait_for_disconnect(&mut server_app);

    let quinnet_server = server_app.world.resource::<QuinnetServer>();
    assert_eq!(quinnet_server.endpoint().clients().len(), 0);

    let connected_clients = server_app.world.resource::<ConnectedClients>();
    assert_eq!(connected_clients.len(), 0);
}

#[test]
fn replication() {
    let port = 6001; // TODO Use port 0 and retrieve the port used by the server.
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            RepliconPlugins.set(ServerPlugin {
                tick_policy: TickPolicy::EveryFrame,
                ..Default::default()
            }),
            RepliconQuinnetPlugins,
        ));
    }

    setup(&mut server_app, &mut client_app, port);

    server_app.world.spawn(Replication);

    server_app.update();
    client_wait_for_message(&mut client_app);

    assert_eq!(client_app.world.entities().len(), 1);
}

#[test]
fn server_event() {
    let port = 6002; // TODO Use port 0 and retrieve the port used by the server.
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            RepliconPlugins.set(ServerPlugin {
                tick_policy: TickPolicy::EveryFrame,
                ..Default::default()
            }),
            RepliconQuinnetPlugins,
        ))
        .add_server_event::<DummyEvent>(ChannelKind::Ordered);
    }

    setup(&mut server_app, &mut client_app, port);

    server_app.world.send_event(ToClients {
        mode: SendMode::Broadcast,
        event: DummyEvent,
    });

    server_app.update();
    client_wait_for_message(&mut client_app);

    let dummy_events = client_app.world.resource::<Events<DummyEvent>>();
    assert_eq!(dummy_events.len(), 1);
}

#[test]
fn client_event() {
    let port = 6003; // TODO Use port 0 and retrieve the port used by the server.
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            RepliconPlugins.set(ServerPlugin {
                tick_policy: TickPolicy::EveryFrame,
                ..Default::default()
            }),
            RepliconQuinnetPlugins,
        ))
        .add_client_event::<DummyEvent>(ChannelKind::Ordered);
    }

    setup(&mut server_app, &mut client_app, port);

    client_app.world.send_event(DummyEvent);
    client_app.update();
    server_wait_for_message(&mut server_app);

    let client_events = server_app
        .world
        .resource::<Events<FromClient<DummyEvent>>>();
    assert_eq!(client_events.len(), 1);
}

fn setup(server_app: &mut App, client_app: &mut App, server_port: u16) {
    setup_server(server_app, server_port);
    setup_client(client_app, server_port);
    wait_for_connection(server_app, client_app);
}

fn setup_client(app: &mut App, server_port: u16) {
    let channels_config = app
        .world
        .resource::<RepliconChannels>()
        .get_client_configs();

    let mut client = app.world.resource_mut::<QuinnetClient>();
    client
        .open_connection(
            ConnectionConfiguration::from_ips(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                server_port,
                IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
                0,
            ),
            CertificateVerificationMode::SkipVerification,
            channels_config,
        )
        .unwrap();
}

fn setup_server(app: &mut App, server_port: u16) {
    let channels_config = app
        .world
        .resource::<RepliconChannels>()
        .get_server_configs();

    let mut server = app.world.resource_mut::<QuinnetServer>();
    server
        .start_endpoint(
            ServerConfiguration::from_ip(IpAddr::V4(Ipv4Addr::LOCALHOST), server_port),
            CertificateRetrievalMode::GenerateSelfSigned {
                server_hostname: Ipv4Addr::LOCALHOST.to_string(),
            },
            channels_config,
        )
        .unwrap();
}

fn wait_for_connection(server_app: &mut App, client_app: &mut App) {
    loop {
        client_app.update();
        server_app.update();
        if client_app.world.resource::<QuinnetClient>().is_connected() {
            break;
        }
    }
}

fn client_wait_for_message(client_app: &mut App) {
    loop {
        sleep(Duration::from_secs_f32(0.05));
        client_app.update();
        if client_app
            .world
            .resource::<QuinnetClient>()
            .connection()
            .received_messages_count()
            > 0
        {
            break;
        }
    }
}

fn server_wait_for_message(server_app: &mut App) {
    loop {
        sleep(Duration::from_secs_f32(0.05));
        server_app.update();
        if server_app
            .world
            .resource::<QuinnetServer>()
            .endpoint()
            .endpoint_stats()
            .received_messages_count()
            > 0
        {
            break;
        }
    }
}

fn server_wait_for_disconnect(server_app: &mut App) {
    loop {
        sleep(Duration::from_secs_f32(0.05));
        server_app.update();
        if server_app
            .world
            .resource::<QuinnetServer>()
            .endpoint()
            .endpoint_stats()
            .disconnect_count()
            > 0
        {
            break;
        }
    }
}

#[derive(Deserialize, Event, Serialize)]
struct DummyEvent;
