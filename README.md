[![Bevy tracking](https://img.shields.io/badge/Bevy%20tracking-released%20version-lightblue)](https://github.com/bevyengine/bevy/blob/main/docs/plugins_guidelines.md#main-branch-tracking)
[![crates.io](https://img.shields.io/crates/v/bevy_quinnet)](https://crates.io/crates/bevy_quinnet)

# Bevy Quinnet

A Client/Server game networking plugin using [QUIC](https://www.chromium.org/quic/), for the Bevy game engine.

- [Bevy Quinnet](#bevy-quinnet)
  - [QUIC as a game networking protocol](#quic-as-a-game-networking-protocol)
  - [Features](#features)
  - [Roadmap](#roadmap)
  - [Quickstart](#quickstart)
    - [Client](#client)
    - [Server](#server)
  - [Certificates](#certificates)
  - [Logs](#logs)
  - [Examples](#examples)
    - [Chat example](#chat-example)
    - [Breakout versus](#breakout-versus)
  - [Compatible Bevy versions](#compatible-bevy-versions)
  - [Limitations](#limitations)
  - [Credits](#credits)
  - [License](#license)

## QUIC as a game networking protocol

QUIC was really attractive to me as a game networking protocol because most of the hard-work is done by the protocol specification and the implementation (here [Quinn](https://github.com/quinn-rs/quinn)). No need to reinvent the wheel once again on error-prones subjects such as a UDP reliability wrapper, some encryption & authentication mechanisms, congestion-control, and so on.

Most of the features proposed by the big networking libs are supported by default through QUIC. As an example, here is the list of features presented in [GameNetworkingSockets](https://github.com/ValveSoftware/GameNetworkingSockets):

* *Connection-oriented API (like TCP)*: -> by default
* *... but message-oriented (like UDP), not stream-oriented*: -> by default (*)
* *Supports both reliable and unreliable message types*: ->by default
* *Messages can be larger than underlying MTU. The protocol performs fragmentation, reassembly, and retransmission for reliable messages*: -> by default
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

Quinnet does not have many features, I made it mostly to satisfy my own needs for my own game projects.

It currently features:

- A Client plugin which can:
    - Connect/disconnect to/from a server
    - Send ordered reliable messages (same as messages over TCP) to the server
    - Receive (ordered or unordered) reliable messages from the server
- A Server plugin which can:
    - Accept client connections & disconnect them
    - Send ordered reliable messages to the clients
    - Receive (ordered or unordered) reliable messages from any client
- Both client & server accept custom protocol structs/enums defined by the user as the message format.

Although Quinn and parts of Quinnet are asynchronous, the APIs exposed by Quinnet for the client and server are synchronous. This makes the surface API easy to work with and adapted to a Bevy usage.
The implementation uses [tokio channels](https://tokio.rs/tokio/tutorial/channels) to communicate with the networking async tasks.

##  Roadmap

Those are the features/tasks that will probably come next (in no particular order):

- [ ] Security: More certificates support, see [certificates](#certificates)
- [x] Feature: Send messages from the server to a specific client
- [x] Feature: Send messages from the server to a selected group of clients
- [x] Feature: Raise connection/disconnection events from the plugins
- [ ] Feature: Send unordered reliable messages from the server
- [x] Feature: Implementing a way to launch a local server from a client
- [ ] Feature: Client should be capable to connect to another server after disconnecting
- [ ] Performance: Messages aggregation before sending
- [ ] Clean: Rework the error handling
- [x] Clean: Rework the configuration input for the client & server plugins
- [ ] Documentation: Document the API

## Quickstart

### Client

- Add the `QuinnetClientPlugin` to the bevy app:

```rust
 App::new()
        // ...
        .add_plugin(QuinnetClientPlugin::default())
        // ...
        .run();
```

- You can then use the `Client` resource to connect, send & receive messages:

```rust
fn start_connection(client: ResMut<Client>) {
    client
        .connect(
            ClientConfigurationData::new(
                "127.0.0.1".to_string(),
                6000,
                "0.0.0.0".to_string(),
                0,
            ),
            CertificateVerificationMode::SkipVerification,
        )
        .unwrap();

    // You can already send message(s) even before being connected, they will be buffered.
    // To be trully connected, you should wait for a ConnectionEvent
    // or check client.is_connected()
    client
        .send_message(...)
        .unwrap();
}
```

- To process server messages, you can use a bevy system such as the one below. The function `receive_message` is generic, here `ServerMessage` is a user provided enum deriving `Serialize` and `Deserialize`.

```rust
fn handle_server_messages(
    mut client: ResMut<Client>,
    /*...*/
) {
    while let Ok(Some(message)) = client.receive_message::<ServerMessage>() {
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
        .add_plugin(QuinnetServerPlugin::default())
        /*...*/
        .run();
```

- You can then use the `Server` resource to start the listening server:

```rust
fn start_listening(mut server: ResMut<Server>) {
    server
        .start(
            ServerConfigurationData::new("127.0.0.1".to_string(), 6000, "0.0.0.0".to_string()),
            CertificateRetrievalMode::GenerateSelfSigned,
        )
        .unwrap();
}
```

- To process client messages & send messages, you can use a bevy system such as the one below. The function `receive_message` is generic, here `ClientMessage` is a user provided enum deriving `Serialize` and `Deserialize`.

```rust
fn handle_client_messages(
    mut server: ResMut<Server>,
    /*...*/
) {
    while let Ok(Some((message, client_id))) = server.receive_message::<ClientMessage>() {
        match message {
            // Match on your own message types ...
            ClientMessage::Join { username} => {
                // Send a messsage to 1 client
                server.send_message(client_id, ServerMessage::InitClient {/*...*/}).unwrap();
                /*...*/
            }
            ClientMessage::Disconnect { } => {
                // Disconnect a client
                server.disconnect_client(client_id);
                /*...*/
            }
            ClientMessage::ChatMessage { message } => {
                // Send a message to a group of clients
                server.send_group_message(
                        client_group, // Iterator of ClientId
                        ServerMessage::ChatMessage {/*...*/}
                    )
                    .unwrap();
                /*...*/
            }           
        }
    }
}
```

You can also use `server.broadcast_message`, which will send a message to all connected clients. "Connected" here means connected to the server plugin, which happens before your own app handshakes/verifications if you have any. Use `send_group_message` if you want to control the recipients.

## Certificates

Bevy Quinnet (through Quinn & QUIC) uses TLS 1.3 for authentication, the server needs to provide the client with a certificate confirming its identity, and the client must be configured to trust the certificates it receives from the server.

Here are the current options available to the server and client plugins:
- Client : 
    - [x] Skip certificate verification
    - [ ] "Trust on first use" certificates
    - [x] Accept certificates issued by a Certificate Authority
- Server:
    - [x] Generate and issue a self-signed certificate
    - [x] Issue an already existing certificate (CA or self-signed)

- On the client:

```rust
    // To accept any certificate
    client.connect(/*...*/, CertificateVerificationMode::SkipVerification);
    // To only accept certificates issued by a Certificate Authority
    client.connect(/*...*/, CertificateVerificationMode::SignedByCertificateAuthority);
```

- On the server:

```rust
    // To generate a new self-signed certificate on each startup 
    server.start(/*...*/, CertificateRetrievalMode::GenerateSelfSigned);
    // To load a pre-existing one from files
    server.start(/*...*/, CertificateRetrievalMode::LoadFromFile {
        cert_file: "./certificates.pem".into(),
        key_file: "./privkey.pem".into(),
    });
    // To load one from files, or to generate a new self-signed one if the files do not exist.
    server.start(/*...*/, CertificateRetrievalMode::LoadFromFileOrGenerateSelfSigned {
        cert_file: "./certificates.pem".into(),
        key_file: "./privkey.pem".into(),
        save_on_disk: true, // To persist on disk if generated
    });
```

## Logs

For logs configuration, see the unoffical [bevy cheatbook](https://bevy-cheatbook.github.io/features/log.html).

## Examples

Examples can be found in the [examples](examples) directory.
### Chat example

This demo comes with an headless [server](examples/chat/server.rs), a [terminal client](examples/chat/client.rs) and a shared [protocol](examples/chat/protocol.rs).

Start the server with `cargo run --example chat-server` and as many clients as needed with `cargo run --example chat-client`. Type `quit` to disconnect with a client.

![terminal_chat_demo](https://user-images.githubusercontent.com/19689618/197757086-0643e6e7-6c69-4760-9af6-cb323529dc52.gif)

### Breakout versus

This demo is a modification of the classic [Bevy breakout](https://bevyengine.org/examples/games/breakout/) example to turn it into a 2 players versus game.

It hosts a local server from inside a client, instead of a dedicated headless server as in the chat demo. You can find a [server module](examples/breakout/server.rs), a [client module](examples/breakout/client.rs), a shared [protocol](examples/breakout/protocol.rs) and the [bevy app schedule](examples/breakout/breakout.rs).

Start two clients with `cargo run --example breakout`, "Host" one one and "Join" on the other.

![breakout_versus_demo_short](https://user-images.githubusercontent.com/19689618/199804335-17df365c-32aa-49b1-94f1-11b8c7162ae3.gif)

## Compatible Bevy versions

Compatibility of `bevy_quinnet` versions:

| `bevy_quinnet` | `bevy` |
| :------------- | :----- |
| `0.1` - `0.2`  | `0.8`  |

## Limitations

* QUIC is not available in a Browser (used in browsers but not exposed as an API). For now I would rather wait on [WebTransport](https://web.dev/webtransport/)("QUIC" on the Web) than hack on WebRTC data channels.

## Credits

Thanks to the [Renet](https://github.com/lucaspoffo/renet) crate for the inspiration on the high level API.

## License

bevy-quinnet is free and open source! All code in this repository is dual-licensed under either:

* MIT License ([LICENSE-MIT](docs/LICENSE-MIT) or [http://opensource.org/licenses/MIT](http://opensource.org/licenses/MIT))
* Apache License, Version 2.0 ([LICENSE-APACHE](docs/LICENSE-APACHE) or [http://www.apache.org/licenses/LICENSE-2.0](http://www.apache.org/licenses/LICENSE-2.0))

at your option. This means you can select the license you prefer! This dual-licensing approach is the de-facto standard in the Rust ecosystem and there are [very good reasons](https://github.com/bevyengine/bevy/issues/2373) to include both.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
