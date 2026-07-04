use std::collections::HashMap;

use bevy::{
    asset::Assets,
    audio::{AudioPlayer, Volume},
    ecs::system::Single,
    input::ButtonInput,
    prelude::*,
    sprite::Sprite,
    sprite_render::{ColorMaterial, MeshMaterial2d},
    state::state::NextState,
    text::{FontSize, TextColor, TextFont, TextSpan},
    ui::{
        AlignItems, BackgroundColor, FlexDirection, Interaction, JustifyContent, Node,
        PositionType, Val,
    },
};
use bevy_quinnet::{
    client::{
        certificate::CertificateVerificationMode, connection::ClientAddrConfiguration,
        ClientConnectionConfiguration, ClientConnectionConfigurationDefaultables, QuinnetClient,
    },
    shared::ClientId,
};

use crate::{
    protocol::{
        ClientChannel, ClientMessage, PaddleInput, ServerChannel, ServerEvent, ServerSetupMessage,
        ServerUpdate,
    },
    BallCollided, BrickId, CollisionSound, GameState, Velocity, WallLocation, BALL_SIZE,
    BALL_SPEED, BRICK_SIZE, GAP_BETWEEN_BRICKS, LOCAL_BIND_IP, PADDLE_SIZE, SERVER_HOST,
    SERVER_PORT, TIME_STEP,
};

const SCOREBOARD_FONT_SIZE: FontSize = FontSize::Px(40.0);
const SCOREBOARD_TEXT_PADDING: Val = Val::Px(5.0);
const BUTTON_FONT_SIZE: FontSize = FontSize::Px(40.0);

pub(crate) const BACKGROUND_COLOR: Color = Color::srgb(0.9, 0.9, 0.9);
const PADDLE_COLOR: Color = Color::srgb(0.3, 0.3, 0.7);
const OPPONENT_PADDLE_COLOR: Color = Color::srgb(1.0, 0.5, 0.5);
const BALL_COLOR: Color = Color::srgb(0.35, 0.35, 0.6);
const OPPONENT_BALL_COLOR: Color = Color::srgb(0.9, 0.6, 0.6);
const BRICK_COLOR: Color = Color::srgb(0.5, 0.5, 1.0);
const WALL_COLOR: Color = Color::srgb(0.8, 0.8, 0.8);
const TEXT_COLOR: Color = Color::srgb(0.5, 0.5, 1.0);
const SCORE_COLOR: Color = Color::srgb(1.0, 0.5, 0.5);
const NORMAL_BUTTON_COLOR: Color = Color::srgb(0.15, 0.15, 0.15);
const HOVERED_BUTTON_COLOR: Color = Color::srgb(0.25, 0.25, 0.25);
const PRESSED_BUTTON_COLOR: Color = Color::srgb(0.35, 0.75, 0.35);
const BUTTON_TEXT_COLOR: Color = Color::srgb(0.9, 0.9, 0.9);

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
pub(crate) struct Brick;

/// Marks the main menu UI root (despawning it removes the button tree).
#[derive(Component)]
pub(crate) struct MainMenu;

/// Marks the scoreboard UI root entity.
#[derive(Component)]
pub(crate) struct ScoreboardUi;

/// The buttons in the main menu.
#[derive(Clone, Copy, Component)]
pub(crate) enum MenuItem {
    Host,
    Join,
}

#[derive(Component, Default)]
struct Collider;

#[derive(Component)]
#[require(Sprite, Transform, Collider)]
struct Wall;

impl Wall {
    fn new(location: WallLocation) -> (Wall, Sprite, Transform) {
        (
            Wall,
            Sprite::from_color(WALL_COLOR, Vec2::ONE),
            Transform {
                // We need to convert our Vec2 into a Vec3, by giving it a z-coordinate
                // This is used to determine the order of our sprites
                translation: location.position().extend(0.0),
                // The z-scale of 2D objects must always be 1.0,
                // or their ordering will be affected in surprising ways.
                // See https://github.com/bevyengine/bevy/issues/4149
                scale: location.size().extend(1.0),
                ..default()
            },
        )
    }
}

pub(crate) fn start_connection(mut client: ResMut<QuinnetClient>) {
    client
        .open_connection(ClientConnectionConfiguration {
            addr_config: ClientAddrConfiguration::from_ips(
                SERVER_HOST,
                SERVER_PORT,
                LOCAL_BIND_IP,
                0,
            ),
            cert_mode: CertificateVerificationMode::SkipVerification,
            defaultables: ClientConnectionConfigurationDefaultables {
                send_channels_cfg: ClientChannel::channels_configuration(),
                ..Default::default()
            },
        })
        .unwrap();
}

fn spawn_paddle(commands: &mut Commands, position: &Vec3, owned: bool) -> Entity {
    commands
        .spawn((
            Sprite::from_color(
                if owned {
                    PADDLE_COLOR
                } else {
                    OPPONENT_PADDLE_COLOR
                },
                Vec2::ONE,
            ),
            Transform {
                translation: *position,
                scale: PADDLE_SIZE,
                ..default()
            },
            Paddle,
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
                    Sprite {
                        color: BRICK_COLOR,
                        ..default()
                    },
                    Transform {
                        translation: brick_position.extend(0.0),
                        scale: Vec3::new(BRICK_SIZE.x, BRICK_SIZE.y, 1.0),
                        ..default()
                    },
                    Brick,
                ))
                .id();
            bricks.map.insert(brick_id, brick);
            brick_id += 1;
        }
    }
}

pub(crate) fn handle_server_setup_messages(
    mut commands: Commands,
    mut client: ResMut<QuinnetClient>,
    mut client_data: ResMut<ClientData>,
    mut entity_mapping: ResMut<NetworkMapping>,
    mut next_state: ResMut<NextState<GameState>>,
    mut bricks: ResMut<BricksMapping>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    while let Some(message) = client
        .connection_mut()
        .try_receive_message_on(ServerChannel::GameSetup)
    {
        match message {
            ServerSetupMessage::InitClient { client_id } => {
                client_data.self_id = client_id;
            }
            ServerSetupMessage::SpawnPaddle {
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
            ServerSetupMessage::SpawnBall {
                owner_client_id,
                entity,
                position,
                direction,
            } => {
                let ball = commands
                    .spawn((
                        Mesh2d(meshes.add(Circle::default())),
                        MeshMaterial2d(materials.add(player_color_from_bool(
                            owner_client_id == client_data.self_id,
                        ))),
                        Transform::from_translation(position).with_scale(BALL_SIZE),
                        Ball,
                        Velocity(direction.normalize() * BALL_SPEED),
                    ))
                    .id();
                entity_mapping.map.insert(entity, ball);
            }
            ServerSetupMessage::SpawnBricks {
                offset,
                rows,
                columns,
            } => spawn_bricks(&mut commands, &mut bricks, offset, rows, columns),
            ServerSetupMessage::StartGame => next_state.set(GameState::Running),
        }
    }
}

pub(crate) fn handle_server_gameplay_events(
    mut commands: Commands,
    mut client: ResMut<QuinnetClient>,
    client_data: ResMut<ClientData>,
    entity_mapping: ResMut<NetworkMapping>,
    mut balls: Query<
        (
            &mut Transform,
            &mut Velocity,
            &mut MeshMaterial2d<ColorMaterial>,
        ),
        (With<Ball>, Without<Paddle>),
    >,
    bricks: ResMut<BricksMapping>,
    mut scoreboard: ResMut<Scoreboard>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    while let Some(message) = client
        .connection_mut()
        .try_receive_message_on(ServerChannel::GameEvents)
    {
        match message {
            ServerEvent::BrickDestroyed {
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
            ServerEvent::BallCollided {
                owner_client_id,
                entity,
                position,
                velocity,
            } => {
                if let Some(local_ball) = entity_mapping.map.get(&entity) {
                    if let Ok((mut ball_transform, mut ball_velocity, mut ball_mat)) =
                        balls.get_mut(*local_ball)
                    {
                        ball_transform.translation = position;
                        ball_velocity.0 = velocity;
                        ball_mat.0 = materials.add(player_color_from_bool(
                            owner_client_id == client_data.self_id,
                        ));
                    }
                }
                commands.trigger(BallCollided);
            }
        }
    }
}

pub(crate) fn handle_server_updates(
    mut client: ResMut<QuinnetClient>,
    entity_mapping: ResMut<NetworkMapping>,
    mut paddles: Query<&mut Transform, With<Paddle>>,
) {
    while let Some(message) = client
        .connection_mut()
        .try_receive_message_on(ServerChannel::PaddleUpdates)
    {
        match message {
            ServerUpdate::PaddleMoved { entity, position } => {
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
    mut client: ResMut<QuinnetClient>,
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut local: Local<PaddleState>,
) {
    let mut paddle_input = PaddleInput::None;

    if keyboard_input.pressed(KeyCode::ArrowLeft) {
        paddle_input = PaddleInput::Left;
    }

    if keyboard_input.pressed(KeyCode::ArrowRight) {
        paddle_input = PaddleInput::Right;
    }

    if local.current_input != paddle_input {
        client.connection_mut().try_send_message_on(
            ClientChannel::PaddleCommands,
            ClientMessage::PaddleInput {
                input: paddle_input.clone(),
            },
        );
        local.current_input = paddle_input;
    }
}

pub(crate) fn update_scoreboard(
    scoreboard: Res<Scoreboard>,
    score_root: Single<Entity, (With<ScoreboardUi>, With<Text>)>,
    mut writer: TextUiWriter,
) {
    *writer.text(*score_root, 1) = scoreboard.score.to_string();
    writer.color(*score_root, 1).0 = player_color_from_bool(scoreboard.score >= 0);
}

pub(crate) fn play_collision_sound(
    _collided: On<BallCollided>,
    mut commands: Commands,
    sound: Res<CollisionSound>,
) {
    commands.spawn((
        AudioPlayer(sound.clone()),
        // auto-despawn the entity when playback finishes
        PlaybackSettings::DESPAWN.with_volume(Volume::Linear(0.03)),
    ));
}

pub(crate) fn setup_main_menu(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.spawn(Camera2d);

    let button_style = Node {
        width: Val::Px(150.0),
        height: Val::Px(65.0),
        justify_content: JustifyContent::Center,
        align_items: AlignItems::Center,
        ..default()
    };

    let text_font = TextFont {
        font: asset_server.load(BOLD_FONT).into(),
        font_size: BUTTON_FONT_SIZE,
        ..Default::default()
    };
    let text_color = TextColor(BUTTON_TEXT_COLOR);
    commands.spawn((
        Node {
            width: Val::Vw(100.0),
            height: Val::Vh(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(16.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(Color::NONE),
        MainMenu,
        children![
            (
                button_style.clone(),
                Button,
                BackgroundColor(NORMAL_BUTTON_COLOR),
                MenuItem::Host,
                children![(Text::new("Host"), text_font.clone(), text_color)],
            ),
            (
                button_style,
                Button,
                BackgroundColor(NORMAL_BUTTON_COLOR),
                MenuItem::Join,
                children![(Text::new("Join"), text_font, text_color)],
            ),
        ],
    ));
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
            Interaction::Pressed => {
                *color = PRESSED_BUTTON_COLOR.into();
                match item {
                    MenuItem::Host => next_state.set(GameState::HostingLobby),
                    MenuItem::Join => next_state.set(GameState::JoiningLobby),
                }
            }
            Interaction::Hovered => *color = HOVERED_BUTTON_COLOR.into(),
            Interaction::None => *color = NORMAL_BUTTON_COLOR.into(),
        }
    }
}

pub(crate) fn teardown_main_menu(mut commands: Commands, menu: Single<Entity, With<MainMenu>>) {
    commands.entity(*menu).despawn();
}

pub(crate) fn setup_breakout(mut commands: Commands, asset_server: Res<AssetServer>) {
    // Sound
    let ball_collision_sound = asset_server.load(COLLISION_SOUND_EFFECT);
    commands.insert_resource(CollisionSound(ball_collision_sound));

    // Scoreboard
    commands.spawn((
        Text::new("Score: "),
        TextFont {
            font: asset_server.load(BOLD_FONT).into(),
            font_size: SCOREBOARD_FONT_SIZE,
            ..default()
        },
        TextColor(TEXT_COLOR),
        ScoreboardUi,
        Node {
            position_type: PositionType::Absolute,
            top: SCOREBOARD_TEXT_PADDING,
            left: SCOREBOARD_TEXT_PADDING,
            ..default()
        },
        children![(
            TextSpan::default(),
            TextFont {
                font: asset_server.load(NORMAL_FONT).into(),
                font_size: SCOREBOARD_FONT_SIZE,
                ..default()
            },
            TextColor(SCORE_COLOR),
        )],
    ));

    // Walls
    commands.spawn(Wall::new(WallLocation::Left));
    commands.spawn(Wall::new(WallLocation::Right));
    commands.spawn(Wall::new(WallLocation::Bottom));
    commands.spawn(Wall::new(WallLocation::Top));
}

pub(crate) fn apply_velocity(mut query: Query<(&mut Transform, &Velocity), With<Ball>>) {
    for (mut transform, velocity) in &mut query {
        transform.translation.x += velocity.x * TIME_STEP;
        transform.translation.y += velocity.y * TIME_STEP;
    }
}

fn player_color_from_bool(owned: bool) -> Color {
    if owned {
        BALL_COLOR
    } else {
        OPPONENT_BALL_COLOR
    }
}
