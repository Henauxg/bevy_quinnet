/*!
Provides integration for [`bevy_replicon`](https://docs.rs/bevy_replicon) for [`bevy_quinnet`](https://docs.rs/bevy_quinnet).
*/

use bevy::{app::PluginGroupBuilder, prelude::*};
use bevy_quinnet::shared::channels::{ChannelType, ChannelsConfiguration};
use bevy_replicon::prelude::*;

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "server")]
pub mod server;

#[cfg(feature = "client")]
use client::RepliconQuinnetClientPlugin;
#[cfg(feature = "server")]
use server::RepliconQuinnetServerPlugin;

pub struct RepliconQuinnetPlugins;

impl PluginGroup for RepliconQuinnetPlugins {
    fn build(self) -> PluginGroupBuilder {
        let mut group = PluginGroupBuilder::start::<Self>();

        #[cfg(feature = "server")]
        {
            group = group.add(RepliconQuinnetServerPlugin);
        }

        #[cfg(feature = "client")]
        {
            group = group.add(RepliconQuinnetClientPlugin);
        }

        group
    }
}

pub trait ChannelsConfigurationExt {
    /// Returns server channel configs that can be used to start an endpoint on the [`bevy_quinnet::server::QuinnetServer`].
    fn get_server_configs(&self) -> ChannelsConfiguration;

    /// Same as [`ChannelsConfigurationExt::get_server_configs`], but for clients.
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
