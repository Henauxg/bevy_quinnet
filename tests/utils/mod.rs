use std::{
    fs,
    net::{Ipv6Addr, UdpSocket},
    path::Path,
    thread::sleep,
    time::{Duration, Instant},
};

use bevy::{
    app::ScheduleRunnerPlugin,
    ecs::message::MessageReader,
    prelude::{App, Res, ResMut, Resource, Startup, Update},
};
use bevy_quinnet::{
    client::{
        self,
        certificate::{
            CertConnectionAbortEvent, CertInteractionEvent, CertTrustUpdateEvent,
            CertVerificationInfo, CertVerificationStatus, CertVerifierAction,
            CertVerifierBehaviour, CertificateVerificationMode, KnownHosts, TrustOnFirstUseConfig,
        },
        connection::ClientAddrConfiguration,
        ClientConnectionConfiguration, QuinnetClient, QuinnetClientPlugin,
    },
    server::{
        self, certificate::CertificateRetrievalMode, EndpointAddrConfiguration, QuinnetServer,
        QuinnetServerPlugin, ServerEndpointConfiguration,
    },
    shared::{
        channels::{ChannelConfig, ChannelId},
        ClientId,
    },
};

// --- test resources ---

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

// --- constants ---

pub const TEST_MESSAGE_PAYLOAD: &[u8] = &[0x1, 0x2, 0x3, 0x4];

pub const SERVER_IP: Ipv6Addr = Ipv6Addr::LOCALHOST;
pub const LOCAL_BIND_IP: Ipv6Addr = Ipv6Addr::UNSPECIFIED;

pub const DEFAULT_TEST_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(10);

// --- polling ---

/// Poll until `f()` returns `Some(T)` or `timeout` is reached.
pub fn poll_until<T>(
    label: &'static str,
    timeout: Duration,
    poll_interval: Duration,
    mut f: impl FnMut() -> Option<T>,
) -> T {
    let start = Instant::now();
    let deadline = start + timeout;
    loop {
        if let Some(value) = f() {
            return value;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for {label} (timeout: {timeout:?}, elapsed: {:?})",
                start.elapsed()
            );
        }
        sleep(poll_interval);
    }
}

/// Update the given apps each iteration, then run `condition` on immutable app refs.
pub fn poll_apps_until<T>(
    label: &'static str,
    mut client_app: Option<&mut App>,
    mut server_app: Option<&mut App>,
    timeout: Duration,
    poll_interval: Duration,
    mut condition: impl FnMut(Option<&App>, Option<&App>) -> Option<T>,
) -> T {
    let start = Instant::now();
    let deadline = start + timeout;
    loop {
        if let Some(client) = client_app.as_deref_mut() {
            client.update();
        }
        if let Some(server) = server_app.as_deref_mut() {
            server.update();
        }
        if let Some(value) = condition(client_app.as_deref(), server_app.as_deref()) {
            return value;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for {label} (timeout: {timeout:?}, elapsed: {:?})",
                start.elapsed()
            );
        }
        sleep(poll_interval);
    }
}

/// Poll until the given UDP port can be bound locally (e.g. after `stop_endpoint`).
pub fn wait_for_udp_port_available(port: u16, server_app: Option<&mut App>) {
    poll_apps_until(
        "UDP port available",
        None,
        server_app,
        DEFAULT_TEST_TIMEOUT,
        DEFAULT_POLL_INTERVAL,
        |_, _| {
            if UdpSocket::bind((LOCAL_BIND_IP, port)).is_ok() {
                Some(())
            } else {
                None
            }
        },
    );
}

// --- bevy systems ---

pub fn handle_client_events(
    mut connection_events: MessageReader<client::connection::ConnectionEvent>,
    mut cert_trust_update_events: MessageReader<CertTrustUpdateEvent>,
    mut cert_interaction_events: MessageReader<CertInteractionEvent>,
    mut cert_connection_abort_events: MessageReader<CertConnectionAbortEvent>,
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
            CertVerificationStatus::UnknownCertificate => cert_interaction
                .apply_cert_verifier_action(CertVerifierAction::TrustAndStore)
                .expect("Failed to apply cert verification action"),
            CertVerificationStatus::UntrustedCertificate => cert_interaction
                .apply_cert_verifier_action(CertVerifierAction::AbortConnection)
                .expect("Failed to apply cert verification action"),
            CertVerificationStatus::TrustedCertificate => cert_interaction
                .apply_cert_verifier_action(CertVerifierAction::TrustOnce)
                .expect("Failed to apply cert verification action"),
        }
    }
    for connection_abort in cert_connection_abort_events.read() {
        test_data.cert_verif_connection_abort_events_received += 1;
        test_data.last_abort_cert_status = Some(connection_abort.status.clone());
        test_data.last_abort_cert_info = Some(connection_abort.cert_info.clone());
    }
}

pub fn handle_server_events(
    mut connection_events: MessageReader<server::ConnectionEvent>,
    mut connection_lost_events: MessageReader<server::ConnectionLostEvent>,
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

fn start_simple_connection(mut client: ResMut<QuinnetClient>, port: Res<Port>) {
    client
        .open_connection(ClientConnectionConfiguration {
            addr_config: default_client_addr_configuration(port.0),
            cert_mode: CertificateVerificationMode::SkipVerification,
            defaultables: Default::default(),
        })
        .unwrap();
}

fn start_listening(mut server: ResMut<QuinnetServer>) {
    server
        .start_endpoint(ServerEndpointConfiguration {
            addr_config: EndpointAddrConfiguration::from_ip(LOCAL_BIND_IP, 0),
            cert_mode: CertificateRetrievalMode::GenerateSelfSigned {
                server_hostname: SERVER_IP.to_string(),
            },
            defaultables: Default::default(),
        })
        .unwrap();
}

// --- app setup ---

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

pub fn build_cert_client_app() -> App {
    let mut client_app = App::new();
    client_app
        .add_plugins((
            ScheduleRunnerPlugin::default(),
            QuinnetClientPlugin::default(),
        ))
        .insert_resource(ClientTestData::default())
        .add_systems(Update, handle_client_events);
    client_app.update();
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

pub fn start_simple_server_app() -> App {
    let mut server_app = build_server_app();
    server_app.update();
    server_app
}

pub fn start_simple_client_app(port: u16) -> App {
    let mut client_app = build_client_app();
    client_app.insert_resource(Port(port));
    client_app.update();
    client_app
}

pub fn start_test_pair() -> (App, App, u16) {
    let server_app = start_simple_server_app();
    let port = server_listen_port(&server_app);
    let client_app = start_simple_client_app(port);
    (client_app, server_app, port)
}

// --- connection ---

pub fn default_client_addr_configuration(port: u16) -> ClientAddrConfiguration {
    ClientAddrConfiguration::from_ips(SERVER_IP, port, LOCAL_BIND_IP, 0)
}

pub fn server_listen_port(server_app: &App) -> u16 {
    server_app
        .world()
        .resource::<QuinnetServer>()
        .endpoint()
        .local_addr()
        .port()
}

pub fn wait_for_client_connected(client_app: &mut App, server_app: &mut App) -> ClientId {
    poll_apps_until(
        "client connected",
        Some(client_app),
        Some(server_app),
        DEFAULT_TEST_TIMEOUT,
        DEFAULT_POLL_INTERVAL,
        |client, server| {
            let client = client?;
            let server = server?;
            if client.world().resource::<QuinnetClient>().is_connected() {
                server
                    .world()
                    .resource::<ServerTestData>()
                    .last_connected_client_id
            } else {
                None
            }
        },
    )
}

pub fn wait_for_all_clients_disconnected(server_app: &mut App) -> ClientId {
    poll_apps_until(
        "all clients disconnected",
        None,
        Some(server_app),
        DEFAULT_TEST_TIMEOUT,
        DEFAULT_POLL_INTERVAL,
        |_, server| {
            let server = server?;
            if server
                .world()
                .resource::<QuinnetServer>()
                .endpoint()
                .clients()
                .is_empty()
            {
                server
                    .world()
                    .resource::<ServerTestData>()
                    .last_disconnected_client_id
            } else {
                None
            }
        },
    )
}

// --- certificates ---

pub fn test_hosts_file(name: &str) -> String {
    format!("target/test-known-hosts/{name}")
}

pub fn clear_hosts_file(path: &str) {
    if Path::new(path).exists() {
        fs::remove_file(path).expect("Failed to remove known hosts file");
    }
}

pub fn tofu_with_hosts(hosts_file: &str) -> TrustOnFirstUseConfig {
    TrustOnFirstUseConfig {
        known_hosts: KnownHosts::HostsFile(hosts_file.to_string()),
        ..Default::default()
    }
}

pub fn tofu_config_requesting_client_action(
    status: CertVerificationStatus,
    hosts_file: &str,
) -> TrustOnFirstUseConfig {
    let mut config = tofu_with_hosts(hosts_file);
    config
        .verifier_behaviour
        .insert(status, CertVerifierBehaviour::RequestClientAction);
    config
}

pub fn open_tofu_connection(client_app: &mut App, port: u16, config: TrustOnFirstUseConfig) {
    client_app
        .world_mut()
        .resource_mut::<QuinnetClient>()
        .open_connection(ClientConnectionConfiguration {
            addr_config: default_client_addr_configuration(port),
            cert_mode: CertificateVerificationMode::TrustOnFirstUse(config),
            defaultables: Default::default(),
        })
        .unwrap();
}

pub fn wait_for_cert_interaction(
    client_app: &mut App,
    server_app: &mut App,
    expected: CertVerificationStatus,
) {
    poll_apps_until(
        "cert interaction event",
        Some(client_app),
        Some(server_app),
        DEFAULT_TEST_TIMEOUT,
        DEFAULT_POLL_INTERVAL,
        |client, _| {
            let client = client?;
            let data = client.world().resource::<ClientTestData>();
            (data.last_cert_interactions_status.as_ref() == Some(&expected)).then_some(())
        },
    );
}

// --- channels ---

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

// --- messaging ---

pub fn wait_for_client_message(
    client_id: ClientId,
    channel_id: ChannelId,
    server_app: &mut App,
) -> bytes::Bytes {
    poll_until(
        "client message",
        DEFAULT_TEST_TIMEOUT,
        DEFAULT_POLL_INTERVAL,
        || {
            server_app.update();
            match server_app
                .world_mut()
                .resource_mut::<QuinnetServer>()
                .endpoint_mut()
                .receive_payload(client_id, channel_id)
            {
                Ok(Some(payload)) => Some(payload),
                Ok(None) => None,
                Err(err) => panic!("Error when receiving payload from client: {:?}", err),
            }
        },
    )
}

pub fn wait_for_server_message(client_app: &mut App, channel_id: ChannelId) -> bytes::Bytes {
    poll_until(
        "server message",
        DEFAULT_TEST_TIMEOUT,
        DEFAULT_POLL_INTERVAL,
        || {
            client_app.update();
            match client_app
                .world_mut()
                .resource_mut::<QuinnetClient>()
                .connection_mut()
                .receive_payload(channel_id)
            {
                Ok(Some(payload)) => Some(payload),
                Ok(None) => None,
                Err(err) => panic!("Error when receiving payload from server: {:?}", err),
            }
        },
    )
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
    channel_id: ChannelId,
    server_app: &mut App,
    client_app: &mut App,
    msg_counter: &mut u64,
) {
    *msg_counter += 1;
    let server_message_payload = bytes::Bytes::from(vec![client_id as u8, *msg_counter as u8]);

    let mut server = server_app.world_mut().resource_mut::<QuinnetServer>();
    server
        .endpoint_mut()
        .send_payload_on(client_id, channel_id, server_message_payload.clone())
        .unwrap();

    let client_received = wait_for_server_message(client_app, channel_id);
    assert_eq!(server_message_payload, client_received);
}
