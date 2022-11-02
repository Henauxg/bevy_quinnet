use bevy_quinnet::ClientId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Input {}

// Messages from clients
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    // Join {},
    // Disconnect {},
    PaddleInput {},
}

// Messages from the server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    // ClientConnected { client_id: ClientId },
    // ClientDisconnected { client_id: ClientId },
    InitClient { client_id: ClientId },
    GameStart {},
    BrickDestroyed {},
    BallPosition {},
    PaddlePosition {},
}
