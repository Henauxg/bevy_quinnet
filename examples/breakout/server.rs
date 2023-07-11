use std::collections::HashMap;

use bevy::{
    prelude::{
        default, Bundle, Commands, Component, Entity, EventReader, Query, Res, ResMut, Resource,
        Transform, Vec2, Vec3, With,
    },
    sprite::collide_aabb::{collide, Collision},
    transform::TransformBundle,
};
use bevy_quinnet::{
    server::{certificate::CertificateRetrievalMode, ConnectionEvent, Server, ServerConfiguration},
    shared::{channel::ChannelId, ClientId},
};

use crate::{
    protocol::{ClientMessage, PaddleInput, ServerMessage},
    BrickId, Velocity, WallLocation, BALL_SIZE, BALL_SPEED, BOTTOM_WALL, BRICK_SIZE,
    GAP_BETWEEN_BRICKS, GAP_BETWEEN_BRICKS_AND_SIDES, GAP_BETWEEN_PADDLE_AND_BRICKS,
    GAP_BETWEEN_PADDLE_AND_FLOOR, LEFT_WALL, LOCAL_BIND_IP, PADDLE_PADDING, PADDLE_SIZE,
    PADDLE_SPEED, RIGHT_WALL, SERVER_HOST, SERVER_PORT, TIME_STEP, TOP_WALL, WALL_THICKNESS,
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
}

#[derive(Resource, Debug, Clone, Default)]
pub(crate) struct Players {
    map: HashMap<ClientId, Player>,
}

#[derive(Component)]
pub(crate) struct Paddle {
    player_id: ClientId,
}

#[derive(Component)]
pub(crate) struct Brick(BrickId);

#[derive(Component)]
pub(crate) struct Collider;

#[derive(Component)]
pub(crate) struct Ball {
    last_hit_by: ClientId,
}

#[derive(Bundle)]
struct WallBundle {
    transform_bundle: TransformBundle,
    collider: Collider,
}

pub(crate) fn start_listening(mut server: ResMut<Server>) {
    server
        .start_endpoint(
            ServerConfiguration::from_ip(LOCAL_BIND_IP, SERVER_PORT),
            CertificateRetrievalMode::GenerateSelfSigned {
                server_hostname: SERVER_HOST.to_string(),
            },
        )
        .unwrap();
}

pub(crate) fn handle_client_messages(mut server: ResMut<Server>, mut players: ResMut<Players>) {
    let endpoint = server.endpoint_mut();
    for client_id in endpoint.clients() {
        while let Some(message) = endpoint.try_receive_message_from::<ClientMessage>(client_id) {
            match message {
                ClientMessage::PaddleInput { input } => {
                    if let Some(player) = players.map.get_mut(&client_id) {
                        player.input = input;
                    }
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
            server.endpoint_mut().disconnect_client(client.id).unwrap();
        } else {
            players.map.insert(
                client.id,
                Player {
                    input: PaddleInput::None,
                },
            );
            if players.map.len() == 2 {
                start_game(&mut commands, &mut server, &players);
            }
        }
    }
}

pub(crate) fn update_paddles(
    server: Res<Server>,
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

                server.endpoint().try_send_group_message_on(
                    players.map.keys().into_iter(),
                    ChannelId::Unreliable,
                    ServerMessage::PaddleMoved {
                        entity: paddle_entity,
                        position: paddle_transform.translation,
                    },
                );
            }
        }
    }
}

pub(crate) fn check_for_collisions(
    mut commands: Commands,
    server: ResMut<Server>,
    mut ball_query: Query<(&mut Velocity, &Transform, Entity, &mut Ball)>,
    collider_query: Query<(Entity, &Transform, Option<&Brick>, Option<&Paddle>), With<Collider>>,
) {
    for (mut ball_velocity, ball_transform, ball_entity, mut ball) in ball_query.iter_mut() {
        let ball_size = ball_transform.scale.truncate();

        // check collision with walls
        for (collider_entity, transform, maybe_brick, maybe_paddle) in &collider_query {
            let collision = collide(
                ball_transform.translation,
                ball_size,
                transform.translation,
                transform.scale.truncate(),
            );
            if let Some(collision) = collision {
                // When a ball hit a paddle, mark this ball as belonging to this client
                if let Some(paddle) = maybe_paddle {
                    ball.last_hit_by = paddle.player_id;
                }

                let endpoint = server.endpoint();
                // Bricks should be despawned on collision
                if let Some(brick) = maybe_brick {
                    commands.entity(collider_entity).despawn();

                    endpoint.try_broadcast_message_on(
                        ChannelId::UnorderedReliable,
                        ServerMessage::BrickDestroyed {
                            by_client_id: ball.last_hit_by,
                            brick_id: brick.0,
                        },
                    );
                }

                // reflect the ball when it collides
                let mut reflect_x = false;
                let mut reflect_y = false;

                // only reflect if the ball's velocity is going in the opposite direction of the
                // collision
                match collision {
                    Collision::Left => reflect_x = ball_velocity.x > 0.0,
                    Collision::Right => reflect_x = ball_velocity.x < 0.0,
                    Collision::Top => reflect_y = ball_velocity.y < 0.0,
                    Collision::Bottom => reflect_y = ball_velocity.y > 0.0,
                    Collision::Inside => { /* do nothing */ }
                }

                // reflect velocity on the x-axis if we hit something on the x-axis
                if reflect_x {
                    ball_velocity.x = -ball_velocity.x;
                }

                // reflect velocity on the y-axis if we hit something on the y-axis
                if reflect_y {
                    ball_velocity.y = -ball_velocity.y;
                }

                endpoint.try_broadcast_message_on(
                    ChannelId::UnorderedReliable,
                    ServerMessage::BallCollided {
                        owner_client_id: ball.last_hit_by,
                        entity: ball_entity,
                        position: ball_transform.translation,
                        velocity: ball_velocity.0,
                    },
                );
            }
        }
    }
}

pub(crate) fn apply_velocity(mut query: Query<(&mut Transform, &Velocity), With<Ball>>) {
    for (mut transform, velocity) in &mut query {
        transform.translation.x += velocity.x * TIME_STEP;
        transform.translation.y += velocity.y * TIME_STEP;
    }
}

fn start_game(commands: &mut Commands, server: &mut ResMut<Server>, players: &ResMut<Players>) {
    let endpoint = server.endpoint_mut();
    // Assign ids
    for client_id in players.map.keys().into_iter() {
        endpoint
            .send_message(
                *client_id,
                ServerMessage::InitClient {
                    client_id: *client_id,
                },
            )
            .unwrap();
    }

    // Spawn paddles
    for (position, client_id) in PADDLES_STARTING_POSITION
        .iter()
        .zip(players.map.keys().into_iter())
    {
        let paddle = spawn_paddle(commands, *client_id, &position);
        endpoint
            .send_group_message(
                players.map.keys().into_iter(),
                ServerMessage::SpawnPaddle {
                    owner_client_id: *client_id,
                    entity: paddle,
                    position: *position,
                },
            )
            .unwrap();
    }

    // Spawn balls
    for ((position, direction), client_id) in BALLS_STARTING_POSITION
        .iter()
        .zip(INITIAL_BALLS_DIRECTION.iter())
        .zip(players.map.keys().into_iter())
    {
        let ball = spawn_ball(commands, *client_id, position, direction);
        endpoint
            .send_group_message(
                players.map.keys().into_iter(),
                ServerMessage::SpawnBall {
                    owner_client_id: *client_id,
                    entity: ball,
                    position: *position,
                    direction: *direction,
                },
            )
            .unwrap();
    }

    // Spawn walls
    commands.spawn(WallBundle::new(WallLocation::Left));
    commands.spawn(WallBundle::new(WallLocation::Right));
    commands.spawn(WallBundle::new(WallLocation::Bottom));
    commands.spawn(WallBundle::new(WallLocation::Top));

    // Spawn bricks
    // Negative scales result in flipped sprites / meshes,
    // which is definitely not what we want here
    assert!(BRICK_SIZE.x > 0.0);
    assert!(BRICK_SIZE.y > 0.0);

    let total_width_of_bricks = (RIGHT_WALL - LEFT_WALL) - 2. * GAP_BETWEEN_BRICKS_AND_SIDES;
    let bottom_edge_of_bricks =
        BOTTOM_WALL + GAP_BETWEEN_PADDLE_AND_FLOOR + GAP_BETWEEN_PADDLE_AND_BRICKS;
    let available_height_for_bricks = TOP_WALL
        - bottom_edge_of_bricks
        - (GAP_BETWEEN_PADDLE_AND_FLOOR + GAP_BETWEEN_PADDLE_AND_BRICKS);

    assert!(total_width_of_bricks > 0.0);
    assert!(available_height_for_bricks > 0.0);

    // Given the space available, compute how many rows and columns of bricks we can fit
    let n_columns = (total_width_of_bricks / (BRICK_SIZE.x + GAP_BETWEEN_BRICKS)).floor() as usize;
    let n_rows =
        (available_height_for_bricks / (BRICK_SIZE.y + GAP_BETWEEN_BRICKS)).floor() as usize;
    let height_occupied_by_bricks =
        n_rows as f32 * (BRICK_SIZE.y + GAP_BETWEEN_BRICKS) - GAP_BETWEEN_BRICKS;
    let n_vertical_gaps = n_columns - 1;

    // Because we need to round the number of columns,
    // the space on the top and sides of the bricks only captures a lower bound, not an exact value
    let center_of_bricks = (LEFT_WALL + RIGHT_WALL) / 2.0;
    let left_edge_of_bricks = center_of_bricks
        // Space taken up by the bricks
        - (n_columns as f32 / 2.0 * BRICK_SIZE.x)
        // Space taken up by the gaps
        - n_vertical_gaps as f32 / 2.0 * GAP_BETWEEN_BRICKS;

    // In Bevy, the `translation` of an entity describes the center point,
    // not its bottom-left corner
    let offset_x = left_edge_of_bricks + BRICK_SIZE.x / 2.;
    let offset_y = bottom_edge_of_bricks
        + BRICK_SIZE.y / 2.
        + (available_height_for_bricks - height_occupied_by_bricks) / 2.; // Offset so that both players are at an equal distance of the bricks

    let mut brick_id = 0;
    for row in 0..n_rows {
        for column in 0..n_columns {
            let brick_position = Vec2::new(
                offset_x + column as f32 * (BRICK_SIZE.x + GAP_BETWEEN_BRICKS),
                offset_y + row as f32 * (BRICK_SIZE.y + GAP_BETWEEN_BRICKS),
            );

            // brick
            commands.spawn((
                Brick(brick_id),
                TransformBundle {
                    local: Transform {
                        translation: brick_position.extend(0.0),
                        scale: Vec3::new(BRICK_SIZE.x, BRICK_SIZE.y, 1.0),
                        ..default()
                    },
                    ..default()
                },
                Collider,
            ));
            brick_id += 1;
        }
    }
    endpoint
        .send_group_message(
            players.map.keys().into_iter(),
            ServerMessage::SpawnBricks {
                offset: Vec2 {
                    x: offset_x,
                    y: offset_y,
                },
                rows: n_rows,
                columns: n_columns,
            },
        )
        .unwrap();

    endpoint
        .send_group_message(players.map.keys().into_iter(), ServerMessage::StartGame {})
        .unwrap();
}

fn spawn_paddle(commands: &mut Commands, client_id: ClientId, pos: &Vec3) -> Entity {
    commands
        .spawn((
            Paddle {
                player_id: client_id,
            },
            TransformBundle {
                local: Transform {
                    translation: *pos,
                    scale: PADDLE_SIZE,
                    ..default()
                },
                ..default()
            },
            Collider,
        ))
        .id()
}

fn spawn_ball(
    commands: &mut Commands,
    client_id: ClientId,
    pos: &Vec3,
    direction: &Vec2,
) -> Entity {
    commands
        .spawn((
            Ball {
                last_hit_by: client_id,
            },
            TransformBundle {
                local: Transform {
                    scale: BALL_SIZE,
                    translation: *pos,
                    ..default()
                },
                ..default()
            },
            Velocity(direction.normalize() * BALL_SPEED),
        ))
        .id()
}

impl WallBundle {
    fn new(location: WallLocation) -> WallBundle {
        WallBundle {
            transform_bundle: TransformBundle {
                local: Transform {
                    translation: location.position().extend(0.0),
                    // The z-scale of 2D objects must always be 1.0,
                    // or their ordering will be affected in surprising ways.
                    // See https://github.com/bevyengine/bevy/issues/4149
                    scale: location.size().extend(1.0),
                    ..default()
                },
                ..default()
            },
            collider: Collider,
        }
    }
}
