[![Bevy tracking](https://img.shields.io/badge/Bevy%20tracking-released%20version-lightblue)](https://github.com/bevyengine/bevy/blob/main/docs/plugins_guidelines.md#main-branch-tracking)
# Bevy Quinnet

A Client/Server game networking plugin using [QUIC](https://www.chromium.org/quic/), for the Bevy game engine.

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

Quinnet does not swim in features, I made it mostly to satisfy my own needs in my game projects.
It currently features:

- A Client plugin which can:
    - Connect to a server
    - Send ordered reliable messages (same as messages over TCP) to the server
    - Receive reliable (ordered or unordered) messages from the server
- A Server plugin which can:
    - Accept client connections
    - Brodcast reliable ordered messages to the clients
    - Receive reliable (ordered or unordered) messages from any client
- Both client & server accept custom protocol structs/enums defined by the user as the message format.

Although Quinn and parts of Quinnet are asynchronous, the APIs exposed by Quinnet for the client and server are synchronous. This makes the surface API easy to work with and adapted to a Bevy usage.
The implementation uses [tokio channels](https://tokio.rs/tokio/tutorial/channelshttps://tokio.rs/tokio/tutorial/channels) to communicate with the networking async tasks.

##  Currently missing features

Those are the features that will probably come next (in no particular order):

- [ ] Security: Expose Quinn support of self-signed certificates for the server
- [ ] Security: Expose Quinn support of CA certificates for the server
- [ ] Feature: Send messages from the server to a specific client
- [ ] Feature: Broadcast from the server to a subgroup of clients
- [ ] Feature: Send reliable unordered messages from the server
- [ ] Performance: Messages aggregation before sending
- [ ] Clean: Rework the error handling

## Small sample

TODO

## Systems and resources

TODO

For logs configuration, see the unoffical [bevy cheatbook](https://bevy-cheatbook.github.io/features/log.html).

## Compatible Bevy versions

Compatibility of `bevy_quinnet` versions:

| `bevy_quinnet` | `bevy` |
| :--           | :--    |
| `0.1`         | `0.8`  |

## Limitations

* QUIC is not available in a Browser (used in browsers but not exposed as an API).

## Credits

Thanks to the [Renet](https://github.com/lucaspoffo/renet) crate for the inspiration on the high level API.

## License

bevy-quinnet is free and open source! All code in this repository is dual-licensed under either:

* MIT License ([LICENSE-MIT](docs/LICENSE-MIT) or [http://opensource.org/licenses/MIT](http://opensource.org/licenses/MIT))
* Apache License, Version 2.0 ([LICENSE-APACHE](docs/LICENSE-APACHE) or [http://www.apache.org/licenses/LICENSE-2.0](http://www.apache.org/licenses/LICENSE-2.0))

at your option. This means you can select the license you prefer! This dual-licensing approach is the de-facto standard in the Rust ecosystem and there are [very good reasons](https://github.com/bevyengine/bevy/issues/2373) to include both.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
