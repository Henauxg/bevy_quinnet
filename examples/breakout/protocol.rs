use bevy::prelude::{Entity, Vec2, Vec3};
use bevy_quinnet::shared::{
    channels::{ChannelConfig, ChannelId, SendChannelsConfiguration},
    ClientId,
};
use serde::{Deserialize, Serialize};
use strum::{EnumIter, IntoEnumIterator};

use crate::BrickId;

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum PaddleInput {
    #[default]
    None,
    Left,
    Right,
}

// Messages from clients
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum ClientMessage {
    PaddleInput { input: PaddleInput },
}

// Game setup messages from the server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum ServerSetupMessage {
    InitClient {
        client_id: ClientId,
    },
    SpawnPaddle {
        owner_client_id: ClientId,
        entity: Entity,
        position: Vec3,
    },
    SpawnBall {
        owner_client_id: ClientId,
        entity: Entity,
        position: Vec3,
        direction: Vec2,
    },
    SpawnBricks {
        offset: Vec2,
        rows: usize,
        columns: usize,
    },
    StartGame,
}

// Game events from the server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum ServerEvent {
    BrickDestroyed {
        by_client_id: ClientId,
        brick_id: BrickId,
    },
    BallCollided {
        owner_client_id: ClientId,
        entity: Entity,
        position: Vec3,
        velocity: Vec2,
    },
}

// Game updates from the server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum ServerUpdate {
    PaddleMoved { entity: Entity, position: Vec3 },
}

#[derive(Debug, Clone, Copy, EnumIter)]
#[repr(u8)]
pub enum ClientChannel {
    PaddleCommands,
}
impl Into<ChannelId> for ClientChannel {
    fn into(self) -> ChannelId {
        self as ChannelId
    }
}
impl ClientChannel {
    pub fn to_channel_config(self) -> ChannelConfig {
        match self {
            ClientChannel::PaddleCommands => ChannelConfig::default_ordered_reliable(),
        }
    }
    pub fn channels_configuration() -> SendChannelsConfiguration {
        SendChannelsConfiguration::from_configs(
            ClientChannel::iter()
                .map(ClientChannel::to_channel_config)
                .collect(),
        )
        .unwrap()
    }
}

#[derive(Debug, Clone, Copy, EnumIter)]
#[repr(u8)]
pub enum ServerChannel {
    GameSetup,
    GameEvents,
    PaddleUpdates,
}

impl Into<ChannelId> for ServerChannel {
    fn into(self) -> ChannelId {
        self as ChannelId
    }
}

impl ServerChannel {
    pub fn to_channel_config(self) -> ChannelConfig {
        match self {
            ServerChannel::GameSetup => ChannelConfig::default_ordered_reliable(),
            ServerChannel::GameEvents => ChannelConfig::default_ordered_reliable(),
            ServerChannel::PaddleUpdates => ChannelConfig::default_unreliable(),
        }
    }

    pub fn channels_configuration() -> SendChannelsConfiguration {
        SendChannelsConfiguration::from_configs(
            ServerChannel::iter()
                .map(ServerChannel::to_channel_config)
                .collect(),
        )
        .unwrap()
    }
}
