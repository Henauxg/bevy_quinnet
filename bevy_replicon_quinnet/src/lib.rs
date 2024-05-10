/*!
Provides integration for [`bevy_replicon`](https://docs.rs/bevy_replicon) for [`bevy_quinnet`](https://docs.rs/bevy_quinnet).
*/

use bevy::{app::PluginGroupBuilder, prelude::*};
use bevy_quinnet::{
    client::{Client, QuinnetClientPlugin},
    server::{QuinnetServerPlugin, Server},
    shared::{
        channels::{ChannelType, ChannelsConfiguration},
        QuinnetSyncUpdate,
    },
};
use bevy_replicon::prelude::*;

pub struct RepliconQuinnetServerPlugin;

impl Plugin for RepliconQuinnetServerPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(QuinnetServerPlugin::default())
            .configure_sets(
                PreUpdate,
                ServerSet::ReceivePackets.after(QuinnetSyncUpdate),
            )
            .add_systems(
                PreUpdate,
                (
                    (
                        Self::set_running.run_if(bevy_quinnet::server::server_listening),
                        Self::set_stopped.run_if(not(bevy_quinnet::server::server_listening)),
                        Self::receive_packets.run_if(bevy_quinnet::server::server_listening),
                    )
                        .chain()
                        .in_set(ServerSet::ReceivePackets),
                    Self::forward_server_events.in_set(ServerSet::SendEvents),
                ),
            )
            .add_systems(
                PostUpdate,
                Self::send_packets
                    .in_set(ServerSet::SendPackets)
                    .run_if(bevy_quinnet::server::server_listening),
            );
    }
}

impl RepliconQuinnetServerPlugin {
    fn set_running(mut server: ResMut<RepliconServer>) {
        server.set_running(true);
    }

    fn set_stopped(mut server: ResMut<RepliconServer>) {
        server.set_running(false);
    }

    fn forward_server_events(
        mut conn_events: EventReader<bevy_quinnet::server::ConnectionEvent>,
        mut conn_lost_events: EventReader<bevy_quinnet::server::ConnectionLostEvent>,
        mut server_events: EventWriter<ServerEvent>,
    ) {
        for event in conn_events.read() {
            server_events.send(ServerEvent::ClientConnected {
                client_id: ClientId::new(event.id),
            });
        }
        for event in conn_lost_events.read() {
            server_events.send(ServerEvent::ClientDisconnected {
                client_id: ClientId::new(event.id),
                reason: "".to_string(),
            });
        }
    }

    fn receive_packets(
        connected_clients: Res<ConnectedClients>,
        mut quinnet_server: ResMut<Server>,
        mut replicon_server: ResMut<RepliconServer>,
    ) {
        let Some(endpoint) = quinnet_server.get_endpoint_mut() else {
            return;
        };
        for client_id in connected_clients.iter_client_ids() {
            while let Some((channel_id, message)) =
                endpoint.try_receive_payload_from(client_id.get())
            {
                replicon_server.insert_received(client_id, channel_id, message);
            }
        }
    }

    fn send_packets(
        mut quinnet_server: ResMut<Server>,
        mut replicon_server: ResMut<RepliconServer>,
    ) {
        let Some(endpoint) = quinnet_server.get_endpoint_mut() else {
            return;
        };
        for (client_id, channel_id, message) in replicon_server.drain_sent() {
            endpoint.try_send_payload_on(client_id.get(), channel_id, message);
        }
    }
}

pub struct RepliconQuinnetClientPlugin;

impl Plugin for RepliconQuinnetClientPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(QuinnetClientPlugin::default())
            .configure_sets(
                PreUpdate,
                ClientSet::ReceivePackets.after(QuinnetSyncUpdate),
            )
            .add_systems(
                PreUpdate,
                (
                    Self::set_connecting.run_if(bevy_quinnet::client::client_connecting),
                    Self::set_disconnected.run_if(bevy_quinnet::client::client_just_disconnected),
                    Self::set_connected.run_if(bevy_quinnet::client::client_just_connected),
                    Self::receive_packets.run_if(bevy_quinnet::client::client_connected),
                )
                    .chain()
                    .in_set(ClientSet::ReceivePackets),
            )
            .add_systems(
                PostUpdate,
                Self::send_packets
                    .in_set(ClientSet::SendPackets)
                    .run_if(bevy_quinnet::client::client_connected),
            );
    }
}

impl RepliconQuinnetClientPlugin {
    fn set_disconnected(mut client: ResMut<RepliconClient>) {
        client.set_status(RepliconClientStatus::Disconnected);
    }

    fn set_connecting(mut client: ResMut<RepliconClient>) {
        client.set_status(RepliconClientStatus::Connecting);
    }

    fn set_connected(mut client: ResMut<RepliconClient>, quinnet_client: Res<Client>) {
        let client_id = match quinnet_client.connection().client_id() {
            Some(id) => Some(ClientId::new(id)),
            None => None,
        };

        client.set_status(RepliconClientStatus::Connected { client_id });
    }

    fn receive_packets(
        mut quinnet_client: ResMut<Client>,
        mut replicon_client: ResMut<RepliconClient>,
    ) {
        let Some(connection) = quinnet_client.get_connection_mut() else {
            return;
        };

        while let Some((channel_id, message)) = connection.try_receive_payload() {
            replicon_client.insert_received(channel_id, message);
        }
    }

    fn send_packets(quinnet_client: ResMut<Client>, mut replicon_client: ResMut<RepliconClient>) {
        let Some(connection) = quinnet_client.get_connection() else {
            return;
        };
        for (channel_id, message) in replicon_client.drain_sent() {
            connection.try_send_payload_on(channel_id, message);
        }
    }
}

pub struct RepliconQuinnetPlugins;

impl PluginGroup for RepliconQuinnetPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(RepliconQuinnetClientPlugin)
            .add(RepliconQuinnetServerPlugin)
    }
}

pub trait ChannelsConfigurationExt {
    /// Returns server channel configs that can be used to create [`ConnectionConfig`](renet::ConnectionConfig).
    fn get_server_configs(&self) -> ChannelsConfiguration;

    /// Same as [`RenetChannelsExt::get_server_configs`], but for clients.
    fn get_client_configs(&self) -> ChannelsConfiguration;
}
impl ChannelsConfigurationExt for RepliconChannels {
    fn get_server_configs(&self) -> ChannelsConfiguration {
        create_configs(self.server_channels(), self.default_max_bytes)
    }

    fn get_client_configs(&self) -> ChannelsConfiguration {
        create_configs(self.client_channels(), self.default_max_bytes)
    }
}

/// Converts replicon channels into quinnet channel configs.
fn create_configs(
    channels: &[RepliconChannel],
    _default_max_bytes: usize,
) -> ChannelsConfiguration {
    let mut quinnet_channels = ChannelsConfiguration::new();
    for channel in channels.iter() {
        quinnet_channels.add(match channel.kind {
            ChannelKind::Unreliable => ChannelType::Unreliable,
            ChannelKind::Unordered => ChannelType::UnorderedReliable,
            ChannelKind::Ordered => ChannelType::OrderedReliable,
        });
    }
    quinnet_channels
}
