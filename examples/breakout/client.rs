use std::collections::HashMap;

use bevy::{
    prelude::{
        default, AssetServer, Audio, BuildChildren, Bundle, Button, ButtonBundle, Camera2dBundle,
        Changed, Color, Commands, Component, DespawnRecursiveExt, Entity, EventReader, Input,
        KeyCode, Local, Query, Res, ResMut, State, TextBundle, Transform, Vec2, Vec3, With,
    },
    sprite::{Sprite, SpriteBundle},
    text::{Text, TextSection, TextStyle},
    ui::{
        AlignItems, Interaction, JustifyContent, PositionType, Size, Style, UiColor, UiRect, Val,
    },
};
use bevy_quinnet::{
    client::{CertificateVerificationMode, Client, ClientConfigurationData},
    ClientId,
};

use crate::{
    protocol::{ClientMessage, PaddleInput, ServerMessage},
    Ball, Brick, Collider, CollisionEvent, CollisionSound, GameState, Score, Scoreboard, Velocity,
    WallLocation, BALL_SIZE, BALL_SPEED, BOTTOM_WALL, BRICK_SIZE, GAP_BETWEEN_BRICKS,
    GAP_BETWEEN_BRICKS_AND_CEILING, GAP_BETWEEN_BRICKS_AND_SIDES, GAP_BETWEEN_PADDLE_AND_BRICKS,
    GAP_BETWEEN_PADDLE_AND_FLOOR, LEFT_WALL, PADDLE_SIZE, RIGHT_WALL, TOP_WALL,
};

const SCOREBOARD_FONT_SIZE: f32 = 40.0;
const SCOREBOARD_TEXT_PADDING: Val = Val::Px(5.0);

pub(crate) const BACKGROUND_COLOR: Color = Color::rgb(0.9, 0.9, 0.9);
const PADDLE_COLOR: Color = Color::rgb(0.3, 0.3, 0.7);
const BALL_COLOR: Color = Color::rgb(1.0, 0.5, 0.5);
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

#[derive(Debug, Clone, Default)]
pub(crate) struct ClientData {
    self_id: ClientId,
}

#[derive(Default)]
pub(crate) struct NetworkMapping {
    // Network entity id to local entity id
    map: HashMap<Entity, Entity>,
}

#[derive(Component)]
pub(crate) struct Paddle;

/// The buttons in the main menu.
#[derive(Clone, Copy, Component)]
pub(crate) enum MenuItem {
    Host,
    Join,
}

// This bundle is a collection of the components that define a "wall" in our game
#[derive(Bundle)]
struct WallBundle {
    // You can nest bundles inside of other bundles like this
    // Allowing you to compose their functionality
    #[bundle]
    sprite_bundle: SpriteBundle,
    collider: Collider,
}

pub(crate) fn start_connection(client: ResMut<Client>) {
    client
        .connect(
            ClientConfigurationData::new("127.0.0.1".to_string(), 6000, "0.0.0.0".to_string(), 0),
            CertificateVerificationMode::SkipVerification,
        )
        .unwrap();
}

fn spawn_paddle(commands: &mut Commands, position: &Vec3) -> Entity {
    commands
        .spawn()
        // .insert(Paddle)
        .insert_bundle(SpriteBundle {
            transform: Transform {
                translation: *position,
                scale: PADDLE_SIZE,
                ..default()
            },
            sprite: Sprite {
                color: PADDLE_COLOR,
                ..default()
            },
            ..default()
        })
        .insert(Collider)
        .insert(Paddle)
        .id()
}

fn spawn_ball(commands: &mut Commands, pos: &Vec3, direction: &Vec2) -> Entity {
    commands
        .spawn()
        .insert(Ball)
        .insert_bundle(SpriteBundle {
            transform: Transform {
                scale: BALL_SIZE,
                translation: *pos,
                ..default()
            },
            sprite: Sprite {
                color: BALL_COLOR,
                ..default()
            },
            ..default()
        })
        .insert(Velocity(direction.normalize() * BALL_SPEED))
        .id()
}

pub(crate) fn handle_server_messages(
    mut commands: Commands,
    mut client: ResMut<Client>,
    mut client_data: ResMut<ClientData>,
    mut entity_mapping: ResMut<NetworkMapping>,
    mut game_state: ResMut<State<GameState>>,
    mut paddles: Query<&mut Transform, With<Paddle>>,
) {
    while let Ok(Some(message)) = client.receive_message::<ServerMessage>() {
        match message {
            ServerMessage::InitClient { client_id } => {
                client_data.self_id = client_id;
            }
            ServerMessage::SpawnPaddle {
                client_id,
                entity,
                position,
            } => {
                let paddle = spawn_paddle(&mut commands, &position);
                entity_mapping.map.insert(entity, paddle);
            }
            ServerMessage::SpawnBall {
                entity,
                position,
                direction,
            } => {
                let ball = spawn_ball(&mut commands, &position, &direction);
                entity_mapping.map.insert(entity, ball);
            }
            ServerMessage::StartGame {} => game_state.set(GameState::Running).unwrap(),
            ServerMessage::BrickDestroyed { client_id } => todo!(),
            ServerMessage::BallPosition { entity, position } => todo!(),
            ServerMessage::PaddlePosition { entity, position } => {
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
    client: ResMut<Client>,
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
            .send_message(ClientMessage::PaddleInput {
                input: paddle_input.clone(),
            })
            .unwrap();
        local.current_input = paddle_input;
    }
}

pub(crate) fn update_scoreboard(
    scoreboard: Res<Scoreboard>,
    mut query: Query<&mut Text, With<Score>>,
) {
    let mut text = query.single_mut();
    text.sections[1].value = scoreboard.score.to_string();
}

pub(crate) fn play_collision_sound(
    collision_events: EventReader<CollisionEvent>,
    audio: Res<Audio>,
    sound: Res<CollisionSound>,
) {
    // Play a sound once per frame if a collision occurred.
    if !collision_events.is_empty() {
        // This prevents events staying active on the next frame.
        collision_events.clear();
        audio.play(sound.0.clone());
    }
}

pub(crate) fn setup_main_menu(mut commands: Commands, asset_server: Res<AssetServer>) {
    // Camera
    commands.spawn_bundle(Camera2dBundle::default());

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
        .spawn_bundle(ButtonBundle {
            style: button_style.clone(),
            color: NORMAL_BUTTON_COLOR.into(),
            ..default()
        })
        .insert(MenuItem::Host)
        .with_children(|parent| {
            parent.spawn_bundle(TextBundle::from_section("Host", text_style.clone()));
        });
    commands
        .spawn_bundle(ButtonBundle {
            style: button_style,
            color: NORMAL_BUTTON_COLOR.into(),
            ..default()
        })
        .insert(MenuItem::Join)
        .with_children(|parent| {
            parent.spawn_bundle(TextBundle::from_section("Join", text_style));
        });
}

pub(crate) fn handle_menu_buttons(
    mut interaction_query: Query<
        (&Interaction, &mut UiColor, &MenuItem),
        (Changed<Interaction>, With<Button>),
    >,
    mut game_state: ResMut<State<GameState>>,
) {
    for (interaction, mut color, item) in &mut interaction_query {
        match *interaction {
            Interaction::Clicked => {
                *color = PRESSED_BUTTON_COLOR.into();
                match item {
                    MenuItem::Host => game_state.set(GameState::HostingLobby).unwrap(),
                    MenuItem::Join => game_state.set(GameState::JoiningLobby).unwrap(),
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

// Add the game's entities to our world
pub(crate) fn setup_breakout(mut commands: Commands, asset_server: Res<AssetServer>) {
    // Sound
    let ball_collision_sound = asset_server.load(COLLISION_SOUND_EFFECT);
    commands.insert_resource(CollisionSound(ball_collision_sound));

    // Scoreboard
    commands
        .spawn()
        .insert_bundle(
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
        )
        .insert(Score);

    // Walls
    commands.spawn_bundle(WallBundle::new(WallLocation::Left));
    commands.spawn_bundle(WallBundle::new(WallLocation::Right));
    commands.spawn_bundle(WallBundle::new(WallLocation::Bottom));
    commands.spawn_bundle(WallBundle::new(WallLocation::Top));

    // Bricks
    // Negative scales result in flipped sprites / meshes,
    // which is definitely not what we want here
    assert!(BRICK_SIZE.x > 0.0);
    assert!(BRICK_SIZE.y > 0.0);

    let total_width_of_bricks = (RIGHT_WALL - LEFT_WALL) - 2. * GAP_BETWEEN_BRICKS_AND_SIDES;
    let bottom_edge_of_bricks =
        BOTTOM_WALL + GAP_BETWEEN_PADDLE_AND_FLOOR + GAP_BETWEEN_PADDLE_AND_BRICKS;
    let total_height_of_bricks = TOP_WALL - bottom_edge_of_bricks - GAP_BETWEEN_BRICKS_AND_CEILING;

    assert!(total_width_of_bricks > 0.0);
    assert!(total_height_of_bricks > 0.0);

    // Given the space available, compute how many rows and columns of bricks we can fit
    let n_columns = (total_width_of_bricks / (BRICK_SIZE.x + GAP_BETWEEN_BRICKS)).floor() as usize;
    let n_rows = (total_height_of_bricks / (BRICK_SIZE.y + GAP_BETWEEN_BRICKS)).floor() as usize;
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
    let offset_y = bottom_edge_of_bricks + BRICK_SIZE.y / 2.;

    for row in 0..n_rows {
        for column in 0..n_columns {
            let brick_position = Vec2::new(
                offset_x + column as f32 * (BRICK_SIZE.x + GAP_BETWEEN_BRICKS),
                offset_y + row as f32 * (BRICK_SIZE.y + GAP_BETWEEN_BRICKS),
            );

            // brick
            commands
                .spawn()
                .insert(Brick)
                .insert_bundle(SpriteBundle {
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
                })
                .insert(Collider);
        }
    }
}

impl WallBundle {
    // This "builder method" allows us to reuse logic across our wall entities,
    // making our code easier to read and less prone to bugs when we change the logic
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
            collider: Collider,
        }
    }
}
