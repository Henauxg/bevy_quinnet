use std::collections::HashMap;

use bevy::{
    prelude::{
        default, AssetServer, Audio, BuildChildren, Bundle, Button, ButtonBundle, Camera2dBundle,
        Changed, Color, Commands, Component, DespawnRecursiveExt, Entity, EventReader, EventWriter,
        Input, KeyCode, Local, NextState, PlaybackSettings, Query, Res, ResMut, Resource,
        TextBundle, Transform, Vec2, Vec3, With, Without,
    },
    sprite::{Sprite, SpriteBundle},
    text::{Text, TextSection, TextStyle},
    ui::{
        AlignItems, BackgroundColor, Interaction, JustifyContent, PositionType, Size, Style,
        UiRect, Val,
    },
};
use bevy_quinnet::{
    client::{
        certificate::CertificateVerificationMode, connection::ConnectionConfiguration, Client,
    },
    shared::ClientId,
};

use crate::{
    protocol::{ClientMessage, PaddleInput, ServerMessage},
    BrickId, CollisionEvent, CollisionSound, GameState, Score, Velocity, WallLocation, BALL_SIZE,
    BALL_SPEED, BRICK_SIZE, GAP_BETWEEN_BRICKS, LOCAL_BIND_IP, PADDLE_SIZE, SERVER_HOST,
    SERVER_PORT, TIME_STEP,
};

const SCOREBOARD_FONT_SIZE: f32 = 40.0;
const SCOREBOARD_TEXT_PADDING: Val = Val::Px(5.0);

pub(crate) const BACKGROUND_COLOR: Color = Color::rgb(0.9, 0.9, 0.9);
const PADDLE_COLOR: Color = Color::rgb(0.3, 0.3, 0.7);
const OPPONENT_PADDLE_COLOR: Color = Color::rgb(1.0, 0.5, 0.5);
const BALL_COLOR: Color = Color::rgb(0.35, 0.35, 0.6);
const OPPONENT_BALL_COLOR: Color = Color::rgb(0.9, 0.6, 0.6);
const BRICK_COLOR: Color = Color::rgb(0.5, 0.5, 1.0);
const WALL_COLOR: Color = Color::rgb(0.8, 0.8, 0.8);
const TEXT_COLOR: Color = Color::rgb(0.5, 0.5, 1.0);
const SCORE_COLOR: Color = Color::rgb(1.0, 0.5, 0.5);
const NORMAL_BUTTON_COLOR: Color = Color::rgb(0.15, 0.15, 0.15);
const HOVERED_BUTTON_COLOR: Color = Color::rgb(0.25, 0.25, 0.25);
const PRESSED_BUTTON_COLOR: Color = Color::rgb(0.35, 0.75, 0.35);
const BUTTON_TEXT_COLOR: Color = Color::rgb(0.9, 0.9, 0.9);

const BOLD_FONT: &str = "fonts/FiraSans-Bold.ttf";
const NORMAL_FONT: &str = "fonts/FiraMono-Medium.ttf";
const COLLISION_SOUND_EFFECT: &str = "sounds/breakout_collision.ogg";

#[derive(Resource, Debug, Clone, Default)]
pub(crate) struct ClientData {
    self_id: ClientId,
}

#[derive(Resource, Default)]
pub(crate) struct NetworkMapping {
    // Network entity id to local entity id
    map: HashMap<Entity, Entity>,
}
#[derive(Resource, Default)]
pub struct BricksMapping {
    map: HashMap<BrickId, Entity>,
}

// This resource tracks the game's score
#[derive(Resource)]
pub(crate) struct Scoreboard {
    pub(crate) score: i32,
}

#[derive(Component)]
pub(crate) struct Paddle;

#[derive(Component)]
pub(crate) struct Ball;

#[derive(Component)]
pub(crate) struct Brick(BrickId);

/// The buttons in the main menu.
#[derive(Clone, Copy, Component)]
pub(crate) enum MenuItem {
    Host,
    Join,
}

// This bundle is a collection of the components that define a "wall" in our game
#[derive(Bundle)]
struct WallBundle {
    #[bundle]
    sprite_bundle: SpriteBundle,
}
pub(crate) fn start_connection(mut client: ResMut<Client>) {
    client
        .open_connection(
            ConnectionConfiguration::from_ips(
                SERVER_HOST.parse().unwrap(),
                SERVER_PORT,
                LOCAL_BIND_IP,
                0,
            ),
            CertificateVerificationMode::SkipVerification,
        )
        .unwrap();
}

fn spawn_paddle(commands: &mut Commands, position: &Vec3, owned: bool) -> Entity {
    commands
        .spawn((
            SpriteBundle {
                transform: Transform {
                    translation: *position,
                    scale: PADDLE_SIZE,
                    ..default()
                },
                sprite: Sprite {
                    color: if owned {
                        PADDLE_COLOR
                    } else {
                        OPPONENT_PADDLE_COLOR
                    },
                    ..default()
                },
                ..default()
            },
            Paddle,
        ))
        .id()
}

fn spawn_ball(commands: &mut Commands, pos: &Vec3, direction: &Vec2, owned: bool) -> Entity {
    commands
        .spawn((
            Ball,
            SpriteBundle {
                transform: Transform {
                    scale: BALL_SIZE,
                    translation: *pos,
                    ..default()
                },
                sprite: Sprite {
                    color: ball_color_from_bool(owned),
                    ..default()
                },
                ..default()
            },
            Velocity(direction.normalize() * BALL_SPEED),
        ))
        .id()
}

pub(crate) fn spawn_bricks(
    commands: &mut Commands,
    bricks: &mut ResMut<BricksMapping>,
    offset: Vec2,
    rows: usize,
    columns: usize,
) {
    let mut brick_id = 0;
    for row in 0..rows {
        for column in 0..columns {
            let brick_position = Vec2::new(
                offset.x + column as f32 * (BRICK_SIZE.x + GAP_BETWEEN_BRICKS),
                offset.y + row as f32 * (BRICK_SIZE.y + GAP_BETWEEN_BRICKS),
            );

            let brick = commands
                .spawn((
                    Brick(brick_id),
                    SpriteBundle {
                        sprite: Sprite {
                            color: BRICK_COLOR,
                            ..default()
                        },
                        transform: Transform {
                            translation: brick_position.extend(0.0),
                            scale: Vec3::new(BRICK_SIZE.x, BRICK_SIZE.y, 1.0),
                            ..default()
                        },
                        ..default()
                    },
                ))
                .id();
            bricks.map.insert(brick_id, brick);
            brick_id += 1;
        }
    }
}

pub(crate) fn handle_server_messages(
    mut commands: Commands,
    mut client: ResMut<Client>,
    mut client_data: ResMut<ClientData>,
    mut entity_mapping: ResMut<NetworkMapping>,
    mut next_state: ResMut<NextState<GameState>>,
    mut paddles: Query<&mut Transform, With<Paddle>>,
    mut balls: Query<(&mut Transform, &mut Velocity, &mut Sprite), (With<Ball>, Without<Paddle>)>,
    mut bricks: ResMut<BricksMapping>,
    mut scoreboard: ResMut<Scoreboard>,
    mut collision_events: EventWriter<CollisionEvent>,
) {
    while let Some(message) = client
        .connection_mut()
        .try_receive_message::<ServerMessage>()
    {
        match message {
            ServerMessage::InitClient { client_id } => {
                client_data.self_id = client_id;
            }
            ServerMessage::SpawnPaddle {
                owner_client_id,
                entity,
                position,
            } => {
                let paddle = spawn_paddle(
                    &mut commands,
                    &position,
                    owner_client_id == client_data.self_id,
                );
                entity_mapping.map.insert(entity, paddle);
            }
            ServerMessage::SpawnBall {
                owner_client_id,
                entity,
                position,
                direction,
            } => {
                let ball = spawn_ball(
                    &mut commands,
                    &position,
                    &direction,
                    owner_client_id == client_data.self_id,
                );
                entity_mapping.map.insert(entity, ball);
            }
            ServerMessage::SpawnBricks {
                offset,
                rows,
                columns,
            } => spawn_bricks(&mut commands, &mut bricks, offset, rows, columns),
            ServerMessage::StartGame {} => next_state.set(GameState::Running),
            ServerMessage::BrickDestroyed {
                by_client_id,
                brick_id,
            } => {
                if by_client_id == client_data.self_id {
                    scoreboard.score += 1;
                } else {
                    scoreboard.score -= 1;
                }
                if let Some(brick_entity) = bricks.map.get(&brick_id) {
                    commands.entity(*brick_entity).despawn();
                }
            }
            ServerMessage::BallCollided {
                owner_client_id,
                entity,
                position,
                velocity,
            } => {
                if let Some(local_ball) = entity_mapping.map.get(&entity) {
                    if let Ok((mut ball_transform, mut ball_velocity, mut ball_sprite)) =
                        balls.get_mut(*local_ball)
                    {
                        ball_transform.translation = position;
                        ball_velocity.0 = velocity;
                        ball_sprite.color =
                            ball_color_from_bool(owner_client_id == client_data.self_id);
                    }
                }
                // Sends a collision event so that other systems can react to the collision
                collision_events.send_default();
            }
            ServerMessage::PaddleMoved { entity, position } => {
                if let Some(local_paddle) = entity_mapping.map.get(&entity) {
                    if let Ok(mut paddle_transform) = paddles.get_mut(*local_paddle) {
                        paddle_transform.translation = position;
                    }
                }
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct PaddleState {
    current_input: PaddleInput,
}

pub(crate) fn move_paddle(
    client: Res<Client>,
    keyboard_input: Res<Input<KeyCode>>,
    mut local: Local<PaddleState>,
) {
    let mut paddle_input = PaddleInput::None;

    if keyboard_input.pressed(KeyCode::Left) {
        paddle_input = PaddleInput::Left;
    }

    if keyboard_input.pressed(KeyCode::Right) {
        paddle_input = PaddleInput::Right;
    }

    if local.current_input != paddle_input {
        client
            .connection()
            .try_send_message(ClientMessage::PaddleInput {
                input: paddle_input.clone(),
            });
        local.current_input = paddle_input;
    }
}

pub(crate) fn update_scoreboard(
    scoreboard: Res<Scoreboard>,
    mut query: Query<&mut Text, With<Score>>,
) {
    let mut text = query.single_mut();
    text.sections[1].value = scoreboard.score.to_string();
    text.sections[1].style.color = ball_color_from_bool(scoreboard.score >= 0);
}

pub(crate) fn play_collision_sound(
    mut collision_events: EventReader<CollisionEvent>,
    audio: Res<Audio>,
    sound: Res<CollisionSound>,
) {
    // Play a sound once per frame if a collision occurred.
    if !collision_events.is_empty() {
        // This prevents events staying active on the next frame.
        collision_events.clear();
        audio.play_with_settings(
            sound.0.clone(),
            PlaybackSettings {
                volume: 0.1,
                ..Default::default()
            },
        );
    }
}

pub(crate) fn setup_main_menu(mut commands: Commands, asset_server: Res<AssetServer>) {
    // Camera
    commands.spawn(Camera2dBundle::default());

    let button_style = Style {
        size: Size::new(Val::Px(150.0), Val::Px(65.0)),
        // center button
        margin: UiRect::all(Val::Auto),
        // horizontally center child text
        justify_content: JustifyContent::Center,
        // vertically center child text
        align_items: AlignItems::Center,
        ..default()
    };
    let text_style = TextStyle {
        font: asset_server.load(BOLD_FONT),
        font_size: 40.0,
        color: BUTTON_TEXT_COLOR,
    };
    commands
        .spawn((
            ButtonBundle {
                style: button_style.clone(),
                background_color: NORMAL_BUTTON_COLOR.into(),
                ..default()
            },
            MenuItem::Host,
        ))
        .with_children(|parent| {
            parent.spawn(TextBundle::from_section("Host", text_style.clone()));
        });
    commands
        .spawn((
            ButtonBundle {
                style: button_style,
                background_color: NORMAL_BUTTON_COLOR.into(),
                ..default()
            },
            MenuItem::Join,
        ))
        .with_children(|parent| {
            parent.spawn(TextBundle::from_section("Join", text_style));
        });
}

pub(crate) fn handle_menu_buttons(
    mut interaction_query: Query<
        (&Interaction, &mut BackgroundColor, &MenuItem),
        (Changed<Interaction>, With<Button>),
    >,
    mut next_state: ResMut<NextState<GameState>>,
) {
    for (interaction, mut color, item) in &mut interaction_query {
        match *interaction {
            Interaction::Clicked => {
                *color = PRESSED_BUTTON_COLOR.into();
                match item {
                    MenuItem::Host => next_state.set(GameState::HostingLobby),
                    MenuItem::Join => next_state.set(GameState::JoiningLobby),
                }
            }
            Interaction::Hovered => {
                *color = HOVERED_BUTTON_COLOR.into();
            }
            Interaction::None => {
                *color = NORMAL_BUTTON_COLOR.into();
            }
        }
    }
}

pub(crate) fn teardown_main_menu(mut commands: Commands, query: Query<Entity, With<Button>>) {
    for entity in query.iter() {
        commands.entity(entity).despawn_recursive();
    }
}

pub(crate) fn setup_breakout(mut commands: Commands, asset_server: Res<AssetServer>) {
    // Sound
    let ball_collision_sound = asset_server.load(COLLISION_SOUND_EFFECT);
    commands.insert_resource(CollisionSound(ball_collision_sound));

    // Scoreboard
    commands.spawn((
        TextBundle::from_sections([
            TextSection::new(
                "Score: ",
                TextStyle {
                    font: asset_server.load(BOLD_FONT),
                    font_size: SCOREBOARD_FONT_SIZE,
                    color: TEXT_COLOR,
                },
            ),
            TextSection::from_style(TextStyle {
                font: asset_server.load(NORMAL_FONT),
                font_size: SCOREBOARD_FONT_SIZE,
                color: SCORE_COLOR,
            }),
        ])
        .with_style(Style {
            position_type: PositionType::Absolute,
            position: UiRect {
                top: SCOREBOARD_TEXT_PADDING,
                left: SCOREBOARD_TEXT_PADDING,
                ..default()
            },
            ..default()
        }),
        Score,
    ));

    // Walls
    commands.spawn(WallBundle::new(WallLocation::Left));
    commands.spawn(WallBundle::new(WallLocation::Right));
    commands.spawn(WallBundle::new(WallLocation::Bottom));
    commands.spawn(WallBundle::new(WallLocation::Top));
}

pub(crate) fn apply_velocity(mut query: Query<(&mut Transform, &Velocity), With<Ball>>) {
    for (mut transform, velocity) in &mut query {
        transform.translation.x += velocity.x * TIME_STEP;
        transform.translation.y += velocity.y * TIME_STEP;
    }
}

fn ball_color_from_bool(owned: bool) -> Color {
    if owned {
        BALL_COLOR
    } else {
        OPPONENT_BALL_COLOR
    }
}

impl WallBundle {
    fn new(location: WallLocation) -> WallBundle {
        WallBundle {
            sprite_bundle: SpriteBundle {
                transform: Transform {
                    // We need to convert our Vec2 into a Vec3, by giving it a z-coordinate
                    // This is used to determine the order of our sprites
                    translation: location.position().extend(0.0),
                    // The z-scale of 2D objects must always be 1.0,
                    // or their ordering will be affected in surprising ways.
                    // See https://github.com/bevyengine/bevy/issues/4149
                    scale: location.size().extend(1.0),
                    ..default()
                },
                sprite: Sprite {
                    color: WALL_COLOR,
                    ..default()
                },
                ..default()
            },
        }
    }
}
