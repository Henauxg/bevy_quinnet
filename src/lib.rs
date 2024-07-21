#![warn(missing_docs)]

//! A Client/Server game networking plugin using QUIC, for the Bevy game engine.
//! See the repository at <https://github.com/Henauxg/bevy_quinnet>

/// Client features
#[cfg(feature = "client")]
pub mod client;
/// Server features
#[cfg(feature = "server")]
pub mod server;
/// Shared features between client & server
pub mod shared;
