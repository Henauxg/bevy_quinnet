use std::{
    net::{Ipv6Addr, SocketAddr},
    thread::sleep,
    time::Duration,
};

use bevy::{
    app::ScheduleRunnerPlugin,
    prelude::{App, EventReader, Res, ResMut, Resource, Startup, Update},
};
use bevy_quinnet::{
    client::{
        self,
        certificate::{
            CertConnectionAbortEvent, CertInteractionEvent, CertTrustUpdateEvent,
            CertVerificationInfo, CertVerificationStatus, CertVerifierAction,
            CertificateVerificationMode,
        },
        connection::ClientConfiguration,
        QuinnetClient, QuinnetClientPlugin,
    },
    server::{
        self, certificate::CertificateRetrievalMode, QuinnetServer, QuinnetServerPlugin,
        ServerEndpointConfiguration,
    },
    shared::{
        channels::{ChannelConfig, ChannelId, ChannelsConfiguration},
        connection::ConnectionConfig,
        ClientId,
    },
};

#[derive(Resource, Debug, Clone, Default)]
pub struct ClientTestData {
    pub connection_events_received: u64,

    pub cert_trust_update_events_received: u64,
    pub last_trusted_cert_info: Option<CertVerificationInfo>,

    pub cert_interactions_received: u64,
    pub last_cert_interactions_status: Option<CertVerificationStatus>,
    pub last_cert_interactions_info: Option<CertVerificationInfo>,

    pub cert_verif_connection_abort_events_received: u64,
    pub last_abort_cert_status: Option<CertVerificationStatus>,
    pub last_abort_cert_info: Option<CertVerificationInfo>,
}

#[derive(Resource, Debug, Clone, Default)]
pub struct ServerTestData {
    pub connection_events_received: u64,
    pub last_connected_client_id: Option<ClientId>,
    pub connection_lost_events_received: u64,
    pub last_disconnected_client_id: Option<ClientId>,
}

#[derive(Resource, Debug, Clone, Default)]
pub struct Port(u16);

pub const TEST_MESSAGE_PAYLOAD: &[u8] = &[0x1, 0x2, 0x3, 0x4];

pub const SERVER_IP: Ipv6Addr = Ipv6Addr::LOCALHOST;
pub const LOCAL_BIND_IP: Ipv6Addr = Ipv6Addr::UNSPECIFIED;

pub fn build_client_app() -> App {
    let mut client_app = App::new();
    client_app
        .add_plugins((
            ScheduleRunnerPlugin::default(),
            QuinnetClientPlugin::default(),
        ))
        .insert_resource(ClientTestData::default())
        .add_systems(Startup, start_simple_connection)
        .add_systems(Update, handle_client_events);
    client_app
}

pub fn build_server_app() -> App {
    let mut server_app = App::new();
    server_app
        .add_plugins((
            ScheduleRunnerPlugin::default(),
            QuinnetServerPlugin::default(),
        ))
        .insert_resource(ServerTestData::default())
        .add_systems(Startup, start_listening)
        .add_systems(Update, handle_server_events);
    server_app
}

pub fn default_client_configuration(port: u16) -> ClientConfiguration {
    ClientConfiguration::from_ips(SERVER_IP, port, LOCAL_BIND_IP, 0)
}

pub fn start_simple_connection(mut client: ResMut<QuinnetClient>, port: Res<Port>) {
    client
        .open_connection(
            default_client_configuration(port.0),
            ConnectionConfig::default(),
            CertificateVerificationMode::SkipVerification,
            ChannelsConfiguration::default(),
        )
        .unwrap();
}

pub fn start_listening(mut server: ResMut<QuinnetServer>, port: Res<Port>) {
    server
        .start_endpoint(
            ServerEndpointConfiguration {
                local_bind_addr: SocketAddr::new(LOCAL_BIND_IP.into(), port.0),
                // During tests, we disable the clearing of stale payloads on the server since we check received messages after the whole Update schedule.
                clear_stale_received_payloads: false,
                connections_config: ConnectionConfig::default(),
            },
            CertificateRetrievalMode::GenerateSelfSigned {
                server_hostname: SERVER_IP.to_string(),
            },
            ChannelsConfiguration::default(),
        )
        .unwrap();
}

pub fn handle_client_events(
    mut connection_events: EventReader<client::connection::ConnectionEvent>,
    mut cert_trust_update_events: EventReader<CertTrustUpdateEvent>,
    mut cert_interaction_events: EventReader<CertInteractionEvent>,
    mut cert_connection_abort_events: EventReader<CertConnectionAbortEvent>,
    mut test_data: ResMut<ClientTestData>,
) {
    for _connected_event in connection_events.read() {
        test_data.connection_events_received += 1;
    }
    for trust_update in cert_trust_update_events.read() {
        test_data.cert_trust_update_events_received += 1;
        test_data.last_trusted_cert_info = Some(trust_update.cert_info.clone());
    }
    for cert_interaction in cert_interaction_events.read() {
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
    for connection_abort in cert_connection_abort_events.read() {
        test_data.cert_verif_connection_abort_events_received += 1;
        test_data.last_abort_cert_status = Some(connection_abort.status.clone());
        test_data.last_abort_cert_info = Some(connection_abort.cert_info.clone());
    }
}

pub fn handle_server_events(
    mut connection_events: EventReader<server::ConnectionEvent>,
    mut connection_lost_events: EventReader<server::ConnectionLostEvent>,
    mut test_data: ResMut<ServerTestData>,
) {
    for event in connection_events.read() {
        test_data.connection_events_received += 1;
        test_data.last_connected_client_id = Some(event.id);
    }
    for event in connection_lost_events.read() {
        test_data.connection_lost_events_received += 1;
        test_data.last_disconnected_client_id = Some(event.id);
    }
}

pub fn start_simple_server_app(port: u16) -> App {
    let mut server_app = build_server_app();
    server_app.insert_resource(Port(port));

    // Startup
    server_app.update();
    server_app
}

pub fn start_simple_client_app(port: u16) -> App {
    let mut client_app = build_client_app();
    client_app.insert_resource(Port(port));

    // Startup
    client_app.update();
    client_app
}

pub fn wait_for_client_connected(client_app: &mut App, server_app: &mut App) -> ClientId {
    loop {
        client_app.update();
        server_app.update();
        if client_app
            .world()
            .resource::<QuinnetClient>()
            .is_connected()
        {
            break;
        }
    }
    server_app
        .world()
        .resource::<ServerTestData>()
        .last_connected_client_id
        .expect("A client should have connected")
}

pub fn wait_for_all_clients_disconnected(server_app: &mut App) -> ClientId {
    loop {
        server_app.update();
        if server_app
            .world()
            .resource::<QuinnetServer>()
            .endpoint()
            .clients()
            .len()
            == 0
        {
            break;
        }
    }
    server_app
        .world()
        .resource::<ServerTestData>()
        .last_disconnected_client_id
        .expect("A client should have connected")
}

pub fn get_default_client_channel(app: &App) -> ChannelId {
    let client = app.world().resource::<QuinnetClient>();
    client
        .connection()
        .default_channel()
        .expect("Expected some default channel")
}

pub fn get_default_server_channel(app: &App) -> ChannelId {
    let server = app.world().resource::<QuinnetServer>();
    server
        .endpoint()
        .default_channel()
        .expect("Expected some default channel")
}

pub fn close_client_channel(channel_id: ChannelId, app: &mut App) {
    let mut client = app.world_mut().resource_mut::<QuinnetClient>();
    client
        .connection_mut()
        .close_channel(channel_id)
        .expect("Failed to close channel")
}

pub fn close_server_channel(channel_id: ChannelId, app: &mut App) {
    let mut server = app.world_mut().resource_mut::<QuinnetServer>();
    server
        .endpoint_mut()
        .close_channel(channel_id)
        .expect("Failed to close channel")
}

pub fn open_client_channel(channel_type: ChannelConfig, app: &mut App) -> ChannelId {
    let mut client = app.world_mut().resource_mut::<QuinnetClient>();
    client
        .connection_mut()
        .open_channel(channel_type)
        .expect("Failed to open channel")
}

pub fn open_server_channel(channel_type: ChannelConfig, app: &mut App) -> ChannelId {
    let mut server = app.world_mut().resource_mut::<QuinnetServer>();
    server
        .endpoint_mut()
        .open_channel(channel_type)
        .expect("Failed to open channel")
}

pub fn wait_for_client_message(
    client_id: ClientId,
    channel_id: ChannelId,
    server_app: &mut App,
) -> bytes::Bytes {
    for _ in 0..20 {
        server_app.update();
        match server_app
            .world_mut()
            .resource_mut::<QuinnetServer>()
            .endpoint_mut()
            .receive_payload_from(client_id, channel_id)
        {
            Ok(Some(payload)) => return payload,
            Ok(None) => (),
            Err(err) => panic!("Error when receiving payload from client: {:?}", err),
        }
        sleep(Duration::from_secs_f32(0.05));
    }
    panic!("Did not receive a message from client in time");
}

pub fn wait_for_server_message(client_app: &mut App, channel_id: ChannelId) -> bytes::Bytes {
    for _ in 0..20 {
        client_app.update();
        match client_app
            .world_mut()
            .resource_mut::<QuinnetClient>()
            .connection_mut()
            .receive_payload(channel_id)
        {
            Ok(Some(payload)) => return payload,
            Ok(None) => (),
            Err(err) => panic!("Error when receiving payload from server: {:?}", err),
        }
        sleep(Duration::from_secs_f32(0.05));
    }
    panic!("Did not receive a message from server in time");
}

pub fn send_and_test_client_message(
    client_id: ClientId,
    channel_id: ChannelId,
    client_app: &mut App,
    server_app: &mut App,
    msg_counter: &mut u64,
) {
    *msg_counter += 1;
    let client_message_payload = bytes::Bytes::from(vec![client_id as u8, *msg_counter as u8]);

    let mut client = client_app.world_mut().resource_mut::<QuinnetClient>();
    client
        .connection_mut()
        .send_payload_on(channel_id, client_message_payload.clone())
        .unwrap();

    let server_received = wait_for_client_message(client_id, channel_id, server_app);
    assert_eq!(client_message_payload, server_received);
}

pub fn send_and_test_server_message(
    client_id: ClientId,
    channel: ChannelId,
    server_app: &mut App,
    client_app: &mut App,
    msg_counter: &mut u64,
) {
    *msg_counter += 1;
    let server_message_payload = bytes::Bytes::from(vec![client_id as u8, *msg_counter as u8]);

    let mut server = server_app.world_mut().resource_mut::<QuinnetServer>();
    server
        .endpoint_mut()
        .send_payload_on(client_id, channel, server_message_payload.clone())
        .unwrap();

    let client_received = wait_for_server_message(client_app, channel);
    assert_eq!(server_message_payload, client_received);
}
