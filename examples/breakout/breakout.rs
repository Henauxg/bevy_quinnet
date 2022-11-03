//! A simplified implementation of the classic game "Breakout".
//! => Original example by Bevy, modified for Bevy Quinnet to add a 2 players versus mode.

use bevy::{ecs::schedule::ShouldRun, prelude::*, time::FixedTimestep};
use bevy_quinnet::{
    client::QuinnetClientPlugin,
    server::{QuinnetServerPlugin, Server},
};
use client::BACKGROUND_COLOR;

mod client;
mod protocol;
mod server;

const SERVER_HOST: &str = "127.0.0.1";
const SERVER_PORT: u16 = 6000;

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
const GAP_BETWEEN_PADDLE_AND_BRICKS: f32 = 140.0;
const GAP_BETWEEN_BRICKS: f32 = 5.0;
// These values are lower bounds, as the number of bricks is computed
const GAP_BETWEEN_BRICKS_AND_SIDES: f32 = 20.0;

#[derive(Clone, Eq, PartialEq, Debug, Hash)]
enum GameState {
    MainMenu,
    HostingLobby,
    JoiningLobby,
    Running,
}

#[derive(Component, Deref, DerefMut)]
struct Velocity(Vec2);

#[derive(Default)]
struct CollisionEvent;

#[derive(Component)]
struct Score;

struct CollisionSound(Handle<AudioSource>);

pub type BrickId = u64;

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

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugin(QuinnetServerPlugin::default())
        .add_plugin(QuinnetClientPlugin::default())
        .add_event::<CollisionEvent>()
        .add_state(GameState::MainMenu)
        // Resources
        .insert_resource(Scoreboard { score: 0 })
        .insert_resource(ClearColor(BACKGROUND_COLOR))
        .insert_resource(server::Players::default())
        .insert_resource(client::ClientData::default())
        .insert_resource(client::NetworkMapping::default())
        .insert_resource(client::BricksMapping::default())
        // Main menu
        .add_system_set(
            SystemSet::on_enter(GameState::MainMenu).with_system(client::setup_main_menu),
        )
        .add_system_set(
            SystemSet::on_update(GameState::MainMenu).with_system(client::handle_menu_buttons),
        )
        .add_system_set(
            SystemSet::on_exit(GameState::MainMenu).with_system(client::teardown_main_menu),
        )
        // Hosting a server on a client
        .add_system_set(
            SystemSet::on_enter(GameState::HostingLobby)
                .with_system(server::start_listening)
                .with_system(client::start_connection),
        )
        .add_system_set(
            SystemSet::on_update(GameState::HostingLobby)
                .with_system(server::handle_client_messages)
                .with_system(server::handle_server_events)
                .with_system(client::handle_server_messages),
        )
        // or just Joining as a client
        .add_system_set(
            SystemSet::on_enter(GameState::JoiningLobby).with_system(client::start_connection),
        )
        .add_system_set(
            SystemSet::on_update(GameState::JoiningLobby)
                .with_system(client::handle_server_messages),
        )
        // Running the game.
        // Every app is a client
        .add_system_set(SystemSet::on_enter(GameState::Running).with_system(client::setup_breakout))
        .add_system_set(
            SystemSet::new()
                // https://github.com/bevyengine/bevy/issues/1839
                // Run on a fixed Timestep,on all clients, in GameState::Running
                .with_run_criteria(FixedTimestep::step(TIME_STEP as f64).chain(
                    |In(input): In<ShouldRun>, state: Res<State<GameState>>| match state.current() {
                        GameState::Running => input,
                        _ => ShouldRun::No,
                    },
                ))
                .with_system(client::handle_server_messages.before(client::apply_velocity))
                .with_system(client::apply_velocity)
                .with_system(client::move_paddle)
                .with_system(client::update_scoreboard)
                .with_system(client::play_collision_sound.after(client::handle_server_messages)),
        )
        // But hosting apps are also a server
        .add_system_set(
            SystemSet::new()
                // https://github.com/bevyengine/bevy/issues/1839
                // Run on a fixed Timestep, only for the hosting client, in GameState::Running
                .with_run_criteria(FixedTimestep::step(TIME_STEP as f64).chain(
                    |In(input): In<ShouldRun>,
                     state: Res<State<GameState>>,
                     server: Res<Server>| match state.current() {
                        GameState::Running => match server.is_listening() {
                            true => input,
                            false => ShouldRun::No,
                        },
                        _ => ShouldRun::No,
                    },
                ))
                .with_system(server::handle_client_messages.before(server::update_paddles))
                .with_system(server::update_paddles.before(server::check_for_collisions))
                .with_system(server::apply_velocity.before(server::check_for_collisions))
                .with_system(server::check_for_collisions),
            // .with_system(play_collision_sound.after(check_for_collisions))
            // .with_system(update_scoreboard)
        )
        .add_system(bevy::window::close_on_esc)
        .run();
}
