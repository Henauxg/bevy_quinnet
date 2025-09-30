//! A simplified implementation of the classic game "Breakout".
//! => Original example by Bevy, modified for Bevy Quinnet to add a 2 players versus mode.

use std::net::Ipv6Addr;

use bevy::prelude::*;
use bevy_quinnet::{
    client::QuinnetClientPlugin,
    server::{QuinnetServer, QuinnetServerPlugin},
};
use client::BACKGROUND_COLOR;

mod client;
mod protocol;
mod server;

const SERVER_HOST: Ipv6Addr = Ipv6Addr::LOCALHOST;
const LOCAL_BIND_IP: Ipv6Addr = Ipv6Addr::UNSPECIFIED;
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

const BALL_DIAMETER: f32 = 30.;
const BALL_SIZE: Vec3 = Vec3::new(BALL_DIAMETER, BALL_DIAMETER, 0.0);
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

#[derive(Default, Clone, Eq, PartialEq, Debug, Hash, States)]
enum GameState {
    #[default]
    MainMenu,
    HostingLobby,
    JoiningLobby,
    Running,
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, SystemSet)]
pub enum GameSystems {
    HostSystems,
    ClientSystems,
}

#[derive(Component, Deref, DerefMut)]
struct Velocity(Vec2);

#[derive(Default, Event)]
struct CollisionEvent;

#[derive(Component)]
struct Score;

#[derive(Resource, Deref)]
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

fn server_is_listening(server: Res<QuinnetServer>) -> bool {
    server.is_listening()
}

fn main() {
    let mut app = App::new();
    app.add_plugins((
        DefaultPlugins,
        QuinnetServerPlugin::default(),
        QuinnetClientPlugin::default(),
    ));
    app.add_event::<CollisionEvent>();
    app.init_state::<GameState>();
    app.insert_resource(ClearColor(BACKGROUND_COLOR))
        .insert_resource(client::Scoreboard { score: 0 })
        .insert_resource(client::ClientData::default())
        .insert_resource(client::NetworkMapping::default())
        .insert_resource(client::BricksMapping::default());

    // ------ Main menu ------
    app.add_systems(Update, close_on_esc)
        .add_systems(OnEnter(GameState::MainMenu), client::setup_main_menu)
        .add_systems(
            Update,
            client::handle_menu_buttons.run_if(in_state(GameState::MainMenu)),
        )
        .add_systems(OnExit(GameState::MainMenu), client::teardown_main_menu);

    // Hosting as both server & client
    app.add_systems(
        OnEnter(GameState::HostingLobby),
        (server::start_listening, client::start_connection),
    )
    .add_systems(
        Update,
        (
            server::handle_client_messages,
            server::handle_server_events,
            client::handle_server_setup_messages,
        )
            .run_if(in_state(GameState::HostingLobby)),
    );

    // Or just Joining as a client
    app.add_systems(OnEnter(GameState::JoiningLobby), client::start_connection)
        .add_systems(
            Update,
            client::handle_server_setup_messages.run_if(in_state(GameState::JoiningLobby)),
        );

    // ------ Running the game ------

    // Every app is a client
    app.add_systems(OnEnter(GameState::Running), client::setup_breakout);

    app.edit_schedule(FixedUpdate, |schedule| {
        schedule.configure_sets(GameSystems::ClientSystems.run_if(in_state(GameState::Running)));
        schedule.add_systems(
            (
                (
                    client::handle_server_gameplay_events,
                    client::handle_server_updates,
                    client::apply_velocity,
                )
                    .chain(),
                client::move_paddle,
                client::update_scoreboard,
                client::play_collision_sound,
            )
                .in_set(GameSystems::ClientSystems),
        );
    });

    // But hosting apps are also a server
    app.edit_schedule(FixedUpdate, |schedule| {
        schedule.configure_sets(
            GameSystems::HostSystems
                .run_if(in_state(GameState::Running))
                .run_if(server_is_listening),
        );
        schedule.add_systems(
            ((
                server::handle_client_messages,
                (server::update_paddles, server::apply_velocity),
                server::check_for_collisions,
            )
                .chain(),)
                .in_set(GameSystems::HostSystems),
        );
    });

    app.run();
}

pub fn close_on_esc(
    mut commands: Commands,
    focused_windows: Query<(Entity, &Window)>,
    input: Res<ButtonInput<KeyCode>>,
) {
    for (window, focus) in focused_windows.iter() {
        if !focus.focused {
            continue;
        }

        if input.just_pressed(KeyCode::Escape) {
            commands.entity(window).despawn();
        }
    }
}
