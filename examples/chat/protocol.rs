use std::collections::HashMap;

use bevy_quinnet::shared::{
    channels::{ChannelConfig, ChannelId, ChannelsConfiguration},
    ClientId,
};
use serde::{Deserialize, Serialize};

// Messages from clients
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    Join { name: String },
    Disconnect {},
    ChatMessage { message: String },
}

// Messages from the server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    ClientConnected {
        client_id: ClientId,
        username: String,
    },
    ClientDisconnected {
        client_id: ClientId,
    },
    ChatMessage {
        client_id: ClientId,
        message: String,
    },
    InitClient {
        client_id: ClientId,
        usernames: HashMap<ClientId, String>,
    },
}

#[allow(dead_code)]
#[derive(Debug)]
#[repr(u8)]
pub enum NetworkChannels {
    Chat,
}
impl Into<ChannelId> for NetworkChannels {
    fn into(self) -> ChannelId {
        self as ChannelId
    }
}
impl NetworkChannels {
    #[allow(dead_code)]
    pub fn channels_configuration() -> ChannelsConfiguration {
        ChannelsConfiguration::from_configs(vec![ChannelConfig::default_ordered_reliable()])
            .unwrap()
    }
}
