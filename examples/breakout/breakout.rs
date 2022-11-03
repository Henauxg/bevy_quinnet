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
use client::{
    handle_menu_buttons, handle_server_messages, move_paddle, play_collision_sound, setup_breakout,
    setup_main_menu, start_connection, teardown_main_menu, update_scoreboard, NetworkMapping,
    BACKGROUND_COLOR,
};
use server::{handle_client_messages, handle_server_events, start_listening, update_paddles};

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

const BALL_SIZE: Vec3 = Vec3::new(30.0, 30.0, 0.0);
const BALL_SPEED: f32 = 400.0;

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
        .insert_resource(client::ClientData::default())
        .insert_resource(NetworkMapping::default())
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
                .with_system(handle_server_messages),
        )
        // or just Joining
        .add_system_set(SystemSet::on_enter(GameState::JoiningLobby).with_system(start_connection))
        .add_system_set(
            SystemSet::on_update(GameState::JoiningLobby).with_system(handle_server_messages),
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
            SystemSet::on_update(GameState::Running).with_system(handle_server_messages),
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
                .with_system(update_paddles.before(check_for_collisions))
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

struct CollisionSound(Handle<AudioSource>);

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

fn apply_velocity(mut query: Query<(&mut Transform, &Velocity)>) {
    for (mut transform, velocity) in &mut query {
        transform.translation.x += velocity.x * TIME_STEP;
        transform.translation.y += velocity.y * TIME_STEP;
    }
}

fn check_for_collisions(
    mut commands: Commands,
    mut scoreboard: ResMut<Scoreboard>,
    mut ball_query: Query<(&mut Velocity, &Transform), With<Ball>>,
    collider_query: Query<(Entity, &Transform, Option<&Brick>), With<Collider>>,
    mut collision_events: EventWriter<CollisionEvent>,
) {
    for (mut ball_velocity, ball_transform) in ball_query.iter_mut() {
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
}
