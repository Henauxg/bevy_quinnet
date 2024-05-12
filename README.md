<div align="center">

[![Bevy tracking](https://img.shields.io/badge/Bevy%20tracking-released%20version-lightblue)](https://github.com/bevyengine/bevy/blob/main/docs/plugins_guidelines.md#main-branch-tracking)
[![crates.io](https://img.shields.io/crates/v/bevy_quinnet)](https://crates.io/crates/bevy_quinnet)
[![bevy_quinnet on doc.rs](https://docs.rs/bevy_quinnet/badge.svg)](https://docs.rs/bevy_quinnet)

# Bevy Quinnet

A Client/Server game networking plugin using [QUIC](https://www.chromium.org/quic/), for the [`Bevy`](https://github.com/bevyengine/bevy) engine.

</div>

- [Bevy Quinnet](#bevy-quinnet)
  - [QUIC as a game networking protocol](#quic-as-a-game-networking-protocol)
  - [Features](#features)
  - [Roadmap](#roadmap)
  - [Quickstart](#quickstart)
    - [Client](#client)
    - [Server](#server)
  - [Channels](#channels)
  - [Certificates and server authentication](#certificates-and-server-authentication)
  - [Replicon integration](#replicon-integration)
  - [Examples](#examples)
  - [Compatible Bevy versions](#compatible-bevy-versions)
  - [Misc](#misc)
    - [Cargo features](#cargo-features)
    - [Logs](#logs)
    - [Limitations](#limitations)
  - [Credits](#credits)
  - [License](#license)

## QUIC as a game networking protocol

QUIC was really attractive to me as a game networking protocol because most of the hard-work is done by the protocol specification and the implementation (here [Quinn](https://github.com/quinn-rs/quinn)). No need to reinvent the wheel once again on error-prones subjects such as a UDP reliability wrapper, encryption & authentication mechanisms, congestion-control, and so on.

Most of the features proposed by the big networking libs are supported by default through QUIC. As an example, here is the list of features presented in [GameNetworkingSockets](https://github.com/ValveSoftware/GameNetworkingSockets):

* *Connection-oriented API (like TCP)*: -> by default
* *... but message-oriented (like UDP), not stream-oriented*: -> by default (*)
* *Supports both reliable and unreliable message types*: ->by default
* *Messages can be larger than underlying MTU. The protocol performs fragmentation, reassembly, and retransmission for reliable messages*: -> by default (frag & reassembly is not done by the protocol for unreliable packets)
* *A reliability layer [...]. It is based on the "ack vector" model from DCCP (RFC 4340, section 11.4) and Google QUIC and discussed in the context of games by Glenn Fiedler [...]*: -> by default.
* *Encryption. [...] The details for shared key derivation and per-packet IV are based on the design used by Google's QUIC protocol*: -> by default
* *Tools for simulating packet latency/loss, and detailed stats measurement*: -> Not by default
* *Head-of-line blocking control and bandwidth sharing of multiple message streams on the same connection.*: -> by default
* *IPv6 support*: -> by default
* *Peer-to-peer networking (NAT traversal with ICE + signaling + symmetric connect mode)*: -> Not by default
* *Cross platform*: -> by default, where UDP is available

-> Roughly 9 points out of 11 by default.

(*) Kinda, when sharing a QUIC stream, reliable messages need to be framed.

## Features

Quinnet has basic features, I made it mostly to satisfy my own needs for my own game projects.

It currently features:

- A Client plugin which can:
    - Connect/disconnect to/from one or more server
    - Send & receive unreliable and ordered/unordered reliable messages
- A Server plugin which can:
    - Accept client connections & disconnect them
    - Send & receive unreliable and ordered/unordered reliable messages
- Both client & server accept custom protocol structs/enums defined by the user as the message format.
- Communications are encrypted, and the client can [authenticate the server](#certificates-and-server-authentication).

Although Quinn and parts of Quinnet are asynchronous, the APIs exposed by Quinnet for the client and server are synchronous. This makes the surface API easy to work with and adapted to a Bevy usage.
The implementation uses [tokio channels](https://tokio.rs/tokio/tutorial/channels) internally to communicate with the networking async tasks.

##  Roadmap

This is a bird-eye view of the features/tasks that will probably be worked on next (in no particular order):

- [ ] Feature: Implement `unreliable` messages larger than the path MTU from client & server
- [ ] Performance: feed multiples messages before flushing ordered reliable channels
- [ ] Clean: Rework the error handling in the async back-end
- [ ] Clean: Rework the error handling on collections to not fail at the first error

## Quickstart

### Client

- Add the `QuinnetClientPlugin` to the bevy app:

```rust
 App::new()
        // ...
        .add_plugins(QuinnetClientPlugin::default())
        // ...
        .run();
```

- You can then use the `Client` resource to connect, send & receive messages:

```rust
fn start_connection(client: ResMut<QuinnetClient>) {
    client
        .open_connection(
            ClientEndpointConfiguration::from_ips(
                IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                6000,
                IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
                0,
            ),
            CertificateVerificationMode::SkipVerification,
            ChannelsConfiguration::default(),
        );
    
    // When trully connected, you will receive a ConnectionEvent
```

- To process server messages, you can use a bevy system such as the one below. The function `receive_message` is generic, here `ServerMessage` is a user provided enum deriving `Serialize` and `Deserialize`.

```rust
fn handle_server_messages(
    mut client: ResMut<QuinnetClient>,
    /*...*/
) {
    while let Ok(Some(message)) = client.connection_mut().receive_message::<ServerMessage>() {
        match message {
            // Match on your own message types ...
            ServerMessage::ClientConnected { client_id, username} => {/*...*/}
            ServerMessage::ClientDisconnected { client_id } => {/*...*/}
            ServerMessage::ChatMessage { client_id, message } => {/*...*/}
        }
    }
}
```

### Server

- Add the `QuinnetServerPlugin` to the bevy app:

```rust
 App::new()
        /*...*/
        .add_plugins(QuinnetServerPlugin::default())
        /*...*/
        .run();
```

- You can then use the `Server` resource to start the listening server:

```rust
fn start_listening(mut server: ResMut<QuinnetServer>) {
    server
        .start_endpoint(
            ServerEndpointConfiguration::from_ip(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 6000),
            CertificateRetrievalMode::GenerateSelfSigned,
            ChannelsConfiguration::default(),
        )
        .unwrap();
}
```

- To process client messages & send messages, you can use a bevy system such as the one below. The function `receive_message` is generic, here `ClientMessage` is a user provided enum deriving `Serialize` and `Deserialize`.

```rust
fn handle_client_messages(
    mut server: ResMut<QuinnetServer>,
    /*...*/
) {
    let mut endpoint = server.endpoint_mut();
    for client_id in endpoint.clients() {
        while let Some(message) = endpoint.try_receive_message_from::<ClientMessage>(client_id) {
            match message {
                // Match on your own message types ...
                ClientMessage::Join { username} => {
                    // Send a messsage to 1 client
                    endpoint.send_message(client_id, ServerMessage::InitClient {/*...*/}).unwrap();
                    /*...*/
                }
                ClientMessage::Disconnect { } => {
                    // Disconnect a client
                    endpoint.disconnect_client(client_id);
                    /*...*/
                }
                ClientMessage::ChatMessage { message } => {
                    // Send a message to a group of clients
                    endpoint.send_group_message(
                            client_group, // Iterator of ClientId
                            ServerMessage::ChatMessage {/*...*/}
                        )
                        .unwrap();
                    /*...*/
                }           
            }
        }
    }
}
```

You can also use `endpoint.broadcast_message`, which will send a message to all connected clients. "Connected" here means connected to the server plugin, which happens before your own app handshakes/verifications if you have any. Use `send_group_message` if you want to control the recipients.

## Channels

There are currently 3 types of channels available when you send a message:
- `OrderedReliable`: ensure that messages sent are delivered, and are processed by the receiving end in the same order as they were sent (exemple usage: chat messages)
- `UnorderedReliable`: ensure that messages sent are delivered, in any order (exemple usage: an animation trigger)
- `Unreliable`: no guarantees on the delivery or the order of processing by the receiving end (exemple usage: an entity position sent every ticks)

When you open a connection/endpoint, some channels are created directly according to the given `ChannelsConfiguration`.

```rust
// Default channels configuration contains only 1 channel of the OrderedReliable type,
// akin to a TCP connection.
let channels_config = ChannelsConfiguration::default();
// Creates 2 OrderedReliable channels, and 1 unreliable channel,
// with channel ids being respectively 0, 1 and 2.
let channels_config = ChannelsConfiguration::from_types(vec![
    ChannelType::OrderedReliable,
    ChannelType::OrderedReliable,
    ChannelType::Unreliable]);
```

Each channel is identified by its own `ChannelId`. Among those, there is a `default` channel which will be used when you don't specify the channel. At startup, the first opened channel becomes the default channel.

```rust
let connection = client.connection();
// No channel specified, default channel is used
connection.send_message(message);
// Specifying the channel id
connection.send_message_on(channel_id, message);
// Changing the default channel
connection.set_default_channel(channel_id);
```

In some cases, you may want to create more than one channel instance of the same type. As an example, using multiple `OrderedReliable` channels to avoid some [Head of line blocking](https://en.wikipedia.org/wiki/Head-of-line_blocking) issues. Although channels can be defined through a `ChannelsConfiguration`, they can also currently be opened & closed at any time. You may have up to 256 differents channels opened simultaneously.

```rust
// If you want to create more channels
let chat_channel = client.connection().open_channel(ChannelType::OrderedReliable).unwrap();
client.connection().send_message_on(chat_channel, chat_message);
```

On the server, channels are created and closed at the endpoint level and exist for all current & future clients.
```rust
let chat_channel = server.endpoint().open_channel(ChannelType::OrderedReliable).unwrap();
server.endpoint().send_message_on(client_id, chat_channel, chat_message);
```

## Certificates and server authentication

Bevy Quinnet (through Quinn & QUIC) uses TLS 1.3 for authentication, the server needs to provide the client with a certificate confirming its identity, and the client must be configured to trust the certificates it receives from the server.

Here are the current options available to the server and client plugins for the server authentication:
- Client : 
    - [x] Skip certificate verification (messages are still encrypted, but the server is not authentified)
    - [x] Accept certificates issued by a Certificate Authority (implemented in [Quinn](https://github.com/quinn-rs/quinn), using [rustls](https://github.com/rustls/rustls))
    - [x] [Trust on first use](https://en.wikipedia.org/wiki/Trust_on_first_use) certificates (implemented in Quinnet, using [rustls](https://github.com/rustls/rustls))
- Server:
    - [x] Generate and issue a self-signed certificate
    - [x] Issue an already existing certificate (CA or self-signed)

On the client:

```rust
    // To accept any certificate
    client.open_connection(/*...*/, CertificateVerificationMode::SkipVerification);
    // To only accept certificates issued by a Certificate Authority
    client.open_connection(/*...*/, CertificateVerificationMode::SignedByCertificateAuthority);
    // To use the default configuration of the Trust on first use authentication scheme
    client.open_connection(/*...*/, CertificateVerificationMode::TrustOnFirstUse(TrustOnFirstUseConfig {
            // You can configure TrustOnFirstUse through the TrustOnFirstUseConfig:
            // Provide your own fingerprint store variable/file,
            // or configure the actions to apply for each possible certificate verification status.
            ..Default::default()
        }),
    );
```

On the server:

```rust
    // To generate a new self-signed certificate on each startup 
    server.start_endpoint(/*...*/, CertificateRetrievalMode::GenerateSelfSigned { 
        server_hostname: "127.0.0.1".to_string(),
    });
    // To load a pre-existing one from files
    server.start_endpoint(/*...*/, CertificateRetrievalMode::LoadFromFile {
        cert_file: "./certificates.pem".into(),
        key_file: "./privkey.pem".into(),
    });
    // To load one from files, or to generate a new self-signed one if the files do not exist.
    server.start_endpoint(/*...*/, CertificateRetrievalMode::LoadFromFileOrGenerateSelfSigned {
        cert_file: "./certificates.pem".into(),
        key_file: "./privkey.pem".into(),
        save_on_disk: true, // To persist on disk if generated
        server_hostname: "127.0.0.1".to_string(),
    });
```

See more about certificates in the [certificates readme](docs/Certificates.md)

## Examples

<details>
  <summary>Chat example</summary>

This demo comes with an headless [server](examples/chat/server.rs), a [terminal client](examples/chat/client.rs) and a shared [protocol](examples/chat/protocol.rs).

Start the server with `cargo run --example chat-server` and as many clients as needed with `cargo run --example chat-client`. Type `quit` to disconnect with a client.

![terminal_chat_demo](https://user-images.githubusercontent.com/19689618/197757086-0643e6e7-6c69-4760-9af6-cb323529dc52.gif)

</details>

<details>
  <summary>Breakout versus example</summary>

This demo is a modification of the classic [Bevy breakout](https://bevyengine.org/examples/games/breakout/) example to turn it into a 2 players versus game.

It hosts a local server from inside a client, instead of a dedicated headless server as in the chat demo. You can find a [server module](examples/breakout/server.rs), a [client module](examples/breakout/client.rs), a shared [protocol](examples/breakout/protocol.rs) and the [bevy app schedule](examples/breakout/breakout.rs).

It also makes uses of [`Channels`](#channels). The server broadcasts the paddle position every tick via the `PaddleMoved` message on an `Unreliable` channel, the `BrickDestroyed` and `BallCollided` events are emitted on an `UnorderedReliable` channel, while the game setup and start are using the default `OrderedReliable` channel.

Start two clients with `cargo run --example breakout`, "Host" on one and "Join" on the other.

[breakout_versus_demo_short.mp4](https://user-images.githubusercontent.com/19689618/213700921-85967bd7-9a47-44ac-9471-77a33938569f.mp4)
</details>

Examples can be found in the [examples](examples) directory.

## Replicon integration

Bevy Quinnet can be used as a transport in [`bevy_replicon`](https://github.com/projectharmonia/bevy_replicon) with the provided [`bevy_replicon_quinnet`](https://github.com/Henauxg/bevy_quinnet/tree/main/bevy_replicon_quinnet).

## Compatible Bevy versions

| bevy_quinnet | bevy |
| :----------- | :--- |
| 0.7-0.8      | 0.13 |
| 0.6          | 0.12 |
| 0.5          | 0.11 |
| 0.4          | 0.10 |
| 0.2-0.3      | 0.9  |
| 0.1          | 0.8  |

## Misc

### Cargo features

*Find the list and description in [cargo.toml](Cargo.toml)*

- `shared-client-id` *[default]*: When a new client connects to the server, the server sends its `ClientId` to the client. The client will consider himself `Connected` once it receives this id. When not enabled, the client does not know its `ClientId` on the server.

### Logs

For logs configuration, see the unoffical [bevy cheatbook](https://bevy-cheatbook.github.io/features/log.html).

### Limitations

* QUIC is not available directly in a Browser (used in browsers but not exposed as an API). For now I would rather wait on [WebTransport](https://web.dev/webtransport/)("QUIC" on the Web) than hack on WebRTC data channels.

## Credits

Thanks to the [Renet](https://github.com/lucaspoffo/renet) crate for the inspiration on the high level API.

## License

This crate is free and open source. All code in this repository is dual-licensed under either:

* MIT License ([LICENSE-MIT](LICENSE-MIT) or [http://opensource.org/licenses/MIT](http://opensource.org/licenses/MIT))
* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or [http://www.apache.org/licenses/LICENSE-2.0](http://www.apache.org/licenses/LICENSE-2.0))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
