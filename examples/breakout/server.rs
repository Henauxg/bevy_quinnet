use std::collections::HashMap;

use bevy::{
    prelude::{
        default, Commands, Component, Entity, EventReader, Query, ResMut, Transform, Vec2, Vec3,
    },
    transform::TransformBundle,
};
use bevy_quinnet::{
    server::{CertificateRetrievalMode, ConnectionEvent, Server, ServerConfigurationData},
    ClientId,
};

use crate::{
    protocol::{ClientMessage, PaddleInput, ServerMessage},
    Ball, Collider, Velocity, BALL_SIZE, BALL_SPEED, BOTTOM_WALL, GAP_BETWEEN_PADDLE_AND_FLOOR,
    LEFT_WALL, PADDLE_PADDING, PADDLE_SIZE, PADDLE_SPEED, RIGHT_WALL, TIME_STEP, TOP_WALL,
    WALL_THICKNESS,
};

const GAP_BETWEEN_PADDLE_AND_BALL: f32 = 35.;

// We set the z-value of the ball to 1 so it renders on top in the case of overlapping sprites.
const BALLS_STARTING_POSITION: [Vec3; 2] = [
    Vec3::new(
        0.0,
        BOTTOM_WALL + GAP_BETWEEN_PADDLE_AND_FLOOR + GAP_BETWEEN_PADDLE_AND_BALL,
        1.0,
    ),
    Vec3::new(
        0.0,
        TOP_WALL - GAP_BETWEEN_PADDLE_AND_FLOOR - GAP_BETWEEN_PADDLE_AND_BALL,
        1.0,
    ),
];
const INITIAL_BALLS_DIRECTION: [Vec2; 2] = [Vec2::new(0.5, -0.5), Vec2::new(-0.5, 0.5)];

const PADDLES_STARTING_POSITION: [Vec3; 2] = [
    Vec3::new(0.0, BOTTOM_WALL + GAP_BETWEEN_PADDLE_AND_FLOOR, 0.0),
    Vec3::new(0.0, TOP_WALL - GAP_BETWEEN_PADDLE_AND_FLOOR, 0.0),
];

#[derive(Debug, Clone, Default)]
pub(crate) struct Player {
    input: PaddleInput,
    score: u64,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Players {
    map: HashMap<ClientId, Player>,
}

#[derive(Component)]
pub(crate) struct Paddle {
    player_id: ClientId,
}

pub(crate) fn start_listening(mut server: ResMut<Server>) {
    server
        .start(
            ServerConfigurationData::new("127.0.0.1".to_string(), 6000, "0.0.0.0".to_string()),
            CertificateRetrievalMode::GenerateSelfSigned,
        )
        .unwrap();
}

pub(crate) fn handle_client_messages(mut server: ResMut<Server>, mut players: ResMut<Players>) {
    while let Ok(Some((message, client_id))) = server.receive_message::<ClientMessage>() {
        match message {
            ClientMessage::PaddleInput { input } => {
                if let Some(player) = players.map.get_mut(&client_id) {
                    player.input = input;
                }
            }
        }
    }
}

pub(crate) fn handle_server_events(
    mut commands: Commands,
    mut connection_events: EventReader<ConnectionEvent>,
    mut server: ResMut<Server>,
    mut players: ResMut<Players>,
) {
    // The server signals us about new connections
    for client in connection_events.iter() {
        // Refuse connection once we already have two players
        if players.map.len() >= 2 {
            server.disconnect_client(client.id)
        } else {
            players.map.insert(
                client.id,
                Player {
                    score: 0,
                    input: PaddleInput::None,
                    // paddle: None,
                },
            );
            if players.map.len() == 2 {
                start_game(&mut commands, &mut server, &players);
            }
        }
    }
}

pub(crate) fn update_paddles(
    mut server: ResMut<Server>,
    players: ResMut<Players>,
    mut paddles: Query<(&mut Transform, &Paddle, Entity)>,
) {
    for (mut paddle_transform, paddle, paddle_entity) in paddles.iter_mut() {
        if let Some(player) = players.map.get(&paddle.player_id) {
            if player.input != PaddleInput::None {
                let mut direction = 0.0;
                match player.input {
                    PaddleInput::Left => direction -= 1.0,
                    PaddleInput::Right => direction = 1.0,
                    _ => {}
                }
                // Calculate the new horizontal paddle position based on player input
                let new_paddle_position =
                    paddle_transform.translation.x + direction * PADDLE_SPEED * TIME_STEP;

                // Update the paddle position,
                // making sure it doesn't cause the paddle to leave the arena
                let left_bound =
                    LEFT_WALL + WALL_THICKNESS / 2.0 + PADDLE_SIZE.x / 2.0 + PADDLE_PADDING;
                let right_bound =
                    RIGHT_WALL - WALL_THICKNESS / 2.0 - PADDLE_SIZE.x / 2.0 - PADDLE_PADDING;

                paddle_transform.translation.x = new_paddle_position.clamp(left_bound, right_bound);

                server
                    .send_group_message(
                        players.map.keys().into_iter(),
                        ServerMessage::PaddlePosition {
                            entity: paddle_entity,
                            position: paddle_transform.translation,
                        },
                    )
                    .unwrap();
            }
        }
    }
}

fn start_game(commands: &mut Commands, server: &mut ResMut<Server>, players: &ResMut<Players>) {
    // Paddles
    for (index, (client_id, _)) in players.map.iter().enumerate() {
        let paddle = spawn_paddle(commands, *client_id, &PADDLES_STARTING_POSITION[index]);
        server
            .send_group_message(
                players.map.keys().into_iter(),
                ServerMessage::SpawnPaddle {
                    client_id: *client_id,
                    entity: paddle,
                    position: PADDLES_STARTING_POSITION[index],
                },
            )
            .unwrap();
    }

    // Balls
    for (position, direction) in BALLS_STARTING_POSITION
        .iter()
        .zip(INITIAL_BALLS_DIRECTION.iter())
    {
        let ball = spawn_ball(commands, position, direction);
        server
            .send_group_message(
                players.map.keys().into_iter(),
                ServerMessage::SpawnBall {
                    entity: ball,
                    position: *position,
                    direction: *direction,
                },
            )
            .unwrap();
    }

    server
        .send_group_message(players.map.keys().into_iter(), ServerMessage::StartGame {})
        .unwrap();
}

fn spawn_paddle(commands: &mut Commands, client_id: ClientId, pos: &Vec3) -> Entity {
    commands
        .spawn()
        .insert(Paddle {
            player_id: client_id,
        })
        .insert_bundle(TransformBundle {
            local: Transform {
                translation: *pos,
                scale: PADDLE_SIZE,
                ..default()
            },
            ..default()
        })
        .insert(Collider)
        .id()
}

fn spawn_ball(commands: &mut Commands, pos: &Vec3, direction: &Vec2) -> Entity {
    commands
        .spawn()
        .insert(Ball)
        .insert_bundle(TransformBundle {
            local: Transform {
                scale: BALL_SIZE,
                translation: *pos,
                ..default()
            },
            ..default()
        })
        .insert(Velocity(direction.normalize() * BALL_SPEED))
        .id()
}
