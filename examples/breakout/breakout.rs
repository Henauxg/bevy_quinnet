//! A simplified implementation of the classic game "Breakout".
//! => Original example by Bevy, modified for Bevy Quinnet to add a 2 players versus mode.

use bevy::{
    ecs::schedule::ShouldRun,
    prelude::*,
    sprite::collide_aabb::{collide, Collision},
    time::FixedTimestep,
};
use bevy_quinnet::{
    client::QuinnetClientPlugin,
    server::{QuinnetServerPlugin, Server},
};
use client::{handle_client_events, handle_server_messages, start_connection};
use server::{handle_client_messages, handle_server_events, start_listening};

mod client;
mod protocol;
mod server;

// Defines the amount of time that should elapse between each physics step.
const TIME_STEP: f32 = 1.0 / 60.0;

// These constants are defined in `Transform` units.
// Using the default 2D camera they correspond 1:1 with screen pixels.
const PADDLE_SIZE: Vec3 = Vec3::new(120.0, 20.0, 0.0);
const GAP_BETWEEN_PADDLE_AND_FLOOR: f32 = 60.0;
const PADDLE_SPEED: f32 = 500.0;
// How close can the paddle get to the wall
const PADDLE_PADDING: f32 = 10.0;

// We set the z-value of the ball to 1 so it renders on top in the case of overlapping sprites.
const BALL_STARTING_POSITION: Vec3 = Vec3::new(0.0, -50.0, 1.0);
const BALL_SIZE: Vec3 = Vec3::new(30.0, 30.0, 0.0);
const BALL_SPEED: f32 = 400.0;
const INITIAL_BALL_DIRECTION: Vec2 = Vec2::new(0.5, -0.5);

const WALL_THICKNESS: f32 = 10.0;
// x coordinates
const LEFT_WALL: f32 = -450.;
const RIGHT_WALL: f32 = 450.;
// y coordinates
const BOTTOM_WALL: f32 = -300.;
const TOP_WALL: f32 = 300.;

const BRICK_SIZE: Vec2 = Vec2::new(100., 30.);
// These values are exact
const GAP_BETWEEN_PADDLE_AND_BRICKS: f32 = 270.0;
const GAP_BETWEEN_BRICKS: f32 = 5.0;
// These values are lower bounds, as the number of bricks is computed
const GAP_BETWEEN_BRICKS_AND_CEILING: f32 = 20.0;
const GAP_BETWEEN_BRICKS_AND_SIDES: f32 = 20.0;

const SCOREBOARD_FONT_SIZE: f32 = 40.0;
const SCOREBOARD_TEXT_PADDING: Val = Val::Px(5.0);

const BACKGROUND_COLOR: Color = Color::rgb(0.9, 0.9, 0.9);
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

#[derive(Clone, Eq, PartialEq, Debug, Hash)]
enum GameState {
    MainMenu,
    HostingLobby,
    JoiningLobby,
    Running,
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugin(QuinnetServerPlugin::default())
        .add_plugin(QuinnetClientPlugin::default())
        .add_event::<CollisionEvent>()
        .add_state(GameState::MainMenu)
        .insert_resource(Scoreboard { score: 0 })
        .insert_resource(ClearColor(BACKGROUND_COLOR))
        // Resources
        .insert_resource(server::Players::default()) //TODO Move ?
        .insert_resource(client::Users::default())
        // Main menu
        .add_system_set(SystemSet::on_enter(GameState::MainMenu).with_system(setup_main_menu))
        .add_system_set(SystemSet::on_update(GameState::MainMenu).with_system(handle_menu_buttons))
        .add_system_set(SystemSet::on_exit(GameState::MainMenu).with_system(teardown_main_menu))
        // Hosting
        .add_system_set(
            SystemSet::on_enter(GameState::HostingLobby)
                .with_system(start_listening)
                .with_system(start_connection),
        )
        .add_system_set(
            SystemSet::on_update(GameState::HostingLobby)
                .with_system(handle_client_messages)
                .with_system(handle_server_events)
                .with_system(handle_server_messages)
                .with_system(handle_client_events),
        )
        // or just Joining
        .add_system_set(SystemSet::on_enter(GameState::JoiningLobby).with_system(start_connection))
        .add_system_set(
            SystemSet::on_update(GameState::JoiningLobby)
                .with_system(handle_server_messages)
                .with_system(handle_client_events),
        )
        // Running the game
        .add_system_set(SystemSet::on_enter(GameState::Running).with_system(setup_breakout))
        .add_system_set(
            SystemSet::new()
                // https://github.com/bevyengine/bevy/issues/1839
                .with_run_criteria(FixedTimestep::step(TIME_STEP as f64).chain(
                    |In(input): In<ShouldRun>, state: Res<State<GameState>>| match state.current() {
                        GameState::Running => input,
                        _ => ShouldRun::No,
                    },
                ))
                .with_system(check_for_collisions)
                .with_system(move_paddle.before(check_for_collisions))
                .with_system(apply_velocity.before(check_for_collisions))
                .with_system(play_collision_sound.after(check_for_collisions))
                .with_system(update_scoreboard),
        )
        // Every app is a client
        .add_system_set(
            SystemSet::on_update(GameState::Running)
                .with_system(handle_server_messages)
                .with_system(handle_client_events),
        )
        // But hosting apps are also a server
        .add_system_set(
            SystemSet::new()
                // https://github.com/bevyengine/bevy/issues/1839
                .with_run_criteria(run_if_host.chain(
                    |In(input): In<ShouldRun>, state: Res<State<GameState>>| match state.current() {
                        GameState::Running => input,
                        _ => ShouldRun::No,
                    },
                ))
                // .with_system(check_for_collisions)
                // .with_system(move_paddle.before(check_for_collisions))
                // .with_system(apply_velocity.before(check_for_collisions))
                // .with_system(play_collision_sound.after(check_for_collisions))
                // .with_system(update_scoreboard)
                .with_system(handle_client_messages)
                .with_system(handle_server_events),
        )
        .add_system(bevy::window::close_on_esc)
        .run();
}

#[derive(Component)]
struct Paddle;

#[derive(Component)]
struct Ball;

#[derive(Component, Deref, DerefMut)]
struct Velocity(Vec2);

#[derive(Component)]
struct Collider;

#[derive(Default)]
struct CollisionEvent;

#[derive(Component)]
struct Brick;

#[derive(Component)]
struct Score;

/// The buttons in the main menu.
#[derive(Clone, Copy, Component)]
pub enum MenuItem {
    Host,
    Join,
}

struct CollisionSound(Handle<AudioSource>);

// This bundle is a collection of the components that define a "wall" in our game
#[derive(Bundle)]
struct WallBundle {
    // You can nest bundles inside of other bundles like this
    // Allowing you to compose their functionality
    #[bundle]
    sprite_bundle: SpriteBundle,
    collider: Collider,
}

/// Which side of the arena is this wall located on?
enum WallLocation {
    Left,
    Right,
    Bottom,
    Top,
}

impl WallLocation {
    fn position(&self) -> Vec2 {
        match self {
            WallLocation::Left => Vec2::new(LEFT_WALL, 0.),
            WallLocation::Right => Vec2::new(RIGHT_WALL, 0.),
            WallLocation::Bottom => Vec2::new(0., BOTTOM_WALL),
            WallLocation::Top => Vec2::new(0., TOP_WALL),
        }
    }

    fn size(&self) -> Vec2 {
        let arena_height = TOP_WALL - BOTTOM_WALL;
        let arena_width = RIGHT_WALL - LEFT_WALL;
        // Make sure we haven't messed up our constants
        assert!(arena_height > 0.0);
        assert!(arena_width > 0.0);

        match self {
            WallLocation::Left | WallLocation::Right => {
                Vec2::new(WALL_THICKNESS, arena_height + WALL_THICKNESS)
            }
            WallLocation::Bottom | WallLocation::Top => {
                Vec2::new(arena_width + WALL_THICKNESS, WALL_THICKNESS)
            }
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

// This resource tracks the game's score
struct Scoreboard {
    score: usize,
}

fn run_if_host(server: Res<Server>) -> ShouldRun {
    match server.is_listening() {
        true => ShouldRun::No,
        false => ShouldRun::Yes,
    }
}

fn setup_main_menu(mut commands: Commands, asset_server: Res<AssetServer>) {
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

fn handle_menu_buttons(
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

fn teardown_main_menu(mut commands: Commands, query: Query<Entity, With<Button>>) {
    for entity in query.iter() {
        commands.entity(entity).despawn_recursive();
    }
}

// Add the game's entities to our world
fn setup_breakout(mut commands: Commands, asset_server: Res<AssetServer>) {
    // Sound
    let ball_collision_sound = asset_server.load(COLLISION_SOUND_EFFECT);
    commands.insert_resource(CollisionSound(ball_collision_sound));

    // Paddle
    let paddle_y = BOTTOM_WALL + GAP_BETWEEN_PADDLE_AND_FLOOR;

    commands
        .spawn()
        .insert(Paddle)
        .insert_bundle(SpriteBundle {
            transform: Transform {
                translation: Vec3::new(0.0, paddle_y, 0.0),
                scale: PADDLE_SIZE,
                ..default()
            },
            sprite: Sprite {
                color: PADDLE_COLOR,
                ..default()
            },
            ..default()
        })
        .insert(Collider);

    // Ball
    commands
        .spawn()
        .insert(Ball)
        .insert_bundle(SpriteBundle {
            transform: Transform {
                scale: BALL_SIZE,
                translation: BALL_STARTING_POSITION,
                ..default()
            },
            sprite: Sprite {
                color: BALL_COLOR,
                ..default()
            },
            ..default()
        })
        .insert(Velocity(INITIAL_BALL_DIRECTION.normalize() * BALL_SPEED));

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
    let bottom_edge_of_bricks = paddle_y + GAP_BETWEEN_PADDLE_AND_BRICKS;
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

fn move_paddle(
    keyboard_input: Res<Input<KeyCode>>,
    mut query: Query<&mut Transform, With<Paddle>>,
) {
    let mut paddle_transform = query.single_mut();
    let mut direction = 0.0;

    if keyboard_input.pressed(KeyCode::Left) {
        direction -= 1.0;
    }

    if keyboard_input.pressed(KeyCode::Right) {
        direction += 1.0;
    }

    // Calculate the new horizontal paddle position based on player input
    let new_paddle_position = paddle_transform.translation.x + direction * PADDLE_SPEED * TIME_STEP;

    // Update the paddle position,
    // making sure it doesn't cause the paddle to leave the arena
    let left_bound = LEFT_WALL + WALL_THICKNESS / 2.0 + PADDLE_SIZE.x / 2.0 + PADDLE_PADDING;
    let right_bound = RIGHT_WALL - WALL_THICKNESS / 2.0 - PADDLE_SIZE.x / 2.0 - PADDLE_PADDING;

    paddle_transform.translation.x = new_paddle_position.clamp(left_bound, right_bound);
}

fn apply_velocity(mut query: Query<(&mut Transform, &Velocity)>) {
    for (mut transform, velocity) in &mut query {
        transform.translation.x += velocity.x * TIME_STEP;
        transform.translation.y += velocity.y * TIME_STEP;
    }
}

fn update_scoreboard(scoreboard: Res<Scoreboard>, mut query: Query<&mut Text, With<Score>>) {
    let mut text = query.single_mut();
    text.sections[1].value = scoreboard.score.to_string();
}

fn check_for_collisions(
    mut commands: Commands,
    mut scoreboard: ResMut<Scoreboard>,
    mut ball_query: Query<(&mut Velocity, &Transform), With<Ball>>,
    collider_query: Query<(Entity, &Transform, Option<&Brick>), With<Collider>>,
    mut collision_events: EventWriter<CollisionEvent>,
) {
    let (mut ball_velocity, ball_transform) = ball_query.single_mut();
    let ball_size = ball_transform.scale.truncate();

    // check collision with walls
    for (collider_entity, transform, maybe_brick) in &collider_query {
        let collision = collide(
            ball_transform.translation,
            ball_size,
            transform.translation,
            transform.scale.truncate(),
        );
        if let Some(collision) = collision {
            // Sends a collision event so that other systems can react to the collision
            collision_events.send_default();

            // Bricks should be despawned and increment the scoreboard on collision
            if maybe_brick.is_some() {
                scoreboard.score += 1;
                commands.entity(collider_entity).despawn();
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
        }
    }
}

fn play_collision_sound(
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
