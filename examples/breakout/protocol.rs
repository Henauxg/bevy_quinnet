use bevy::prelude::{Entity, Vec2, Vec3};
use bevy_quinnet::ClientId;
use serde::{Deserialize, Serialize};

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

// Messages from the server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum ServerMessage {
    InitClient {
        client_id: ClientId,
    },
    SpawnPaddle {
        client_id: ClientId,
        entity: Entity,
        position: Vec3,
    },
    SpawnBall {
        entity: Entity,
        position: Vec3,
        direction: Vec2,
    },
    StartGame {},
    BrickDestroyed {
        client_id: ClientId,
    },
    BallCollided {
        entity: Entity,
        position: Vec3,
        velocity: Vec2,
    },
    PaddleMoved {
        entity: Entity,
        position: Vec3,
    },
}
