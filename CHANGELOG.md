# Changelog

## Version 0.11.0 (2024-11-30)

- Updated `rustls` to 0.23 and `quinn` to 0.11 (thanks to [Cyannide](https://github.com/Cyannide), PR [#28](https://github.com/Henauxg/bevy_quinnet/pull/28))
- Changed doc & examples to use IPv6 by default (thanks to [MyZeD](https://github.com/MyZeD), PR [#29](https://github.com/Henauxg/bevy_quinnet/pull/29))
- Changed some errors logs to be warnnings
- Fixed: channels send tasks won't try to flush anymore if we know that the peer connection is closed/lost: less warnings logs emitted.

## Version 0.10.0 (2024-09-09)

- Add `client` & `server` features.

## Version 0.9.0 (2024-07-05)

- Update to use Bevy 0.14

## Version 0.9.0 (2024-07-05)

- Update to use Bevy 0.14

## Version 0.8.0 (2024-05-12)

- Added a new crate `bevy_replicon_quinnet` with tests and examples, providing an integration of bevy_quinnet as a replicon back-end.
- Added a `shared-client-id` cargo feature: server sends the client id to the client, client wait for it before being “connected”
- Added #![warn(missing_docs)]
- Channels:
  - Changed `ChannelId` to be a `u8`
  - Some channels can be preconfigured to be opened on a client or server connection by using `ChannelsConfiguration`
  - Channels payloads now contain the `channel_id`
  - When receiving a message, the message's `channel_id` is now available
  - You can now have only up to 256 channels opened simultaneously
  - You can now have more than 1 `Unreliable` or `UnorderedReliable` channel
- Client:
  - Renamed `Client` to `QuinnetClient`
  - In `QuinnetClient::Connection`
    - Changed `disconnect` function to be `pub` (was previously only accessible through `Client::close_connection`)
    - Added `reconnect` function
    - Added a new bevy event `ConnectionFailedEvent` raised when a connection fails
      - Added a `QuinnetConnectionError` type
    - Renamed `ConnectionId` to `ConnectionLocalId`
    - State:
      - Removed `is_connected`
      - Renamed internal `ConnectionState` to `InternalConnectionState`
      - Added a new `ConnectionState`
      - Added `state` function
  - Changed `update_sync_client` system to be `pub`
  - Added `client_connecting`, `client_connected`, `client_just_connected` and `client_just_disconnected` ergonomic system conditions
- Server:
  - Renamed `Server` to `QuinnetServer`
  - Added `Debug`, `Copy`, `Clone` traits to the server's bevy events
  - Changed `update_sync_server` system to be `pub`
  - Added `server_listening`, `server_just_opened` and `server_just_closed` ergonomic system conditions
  - Removed unnecessary `Clone` requirement from broadcast methods
- Tests:
  - Updated to use the new channel API
  - Added a reconnection test
- Moved `QuinnetError` to `shared::error`

## Version 0.7.0 (2024-02-18)

- Update plugins, tests & examples to use Bevy 0.13
- Update Tokio to 1.36
- Update Bytes to 1.5
- Update ring to 0.17.7
- Update rcgen to 0.12.1
- Fix breakout example UI (by [protofarer](https://github.com/protofarer))
- Fix minor code documentation

## Version 0.6.0 (2023-11-04)

- Update plugins, tests & examples to use Bevy 0.12

## Version 0.5.0 (2023-07-11)

- Update plugins, tests & examples to use Bevy 0.11
- Update quinn & rustls
- Update other dependencies: tokio, bytes, rcgen
- [example:breakout] Reduce the collision sound volume

## Version 0.4.0 (2023-03-07)

- Update Bevy to 0.10
- [client]: Add missing `try_send_message_on` and `try_send_payload_on` in `connection`
- Internal improvements to channels and server's `send_group_message`
- `AsyncRuntime` is now pub
- [client & server] In order to have more control over plugin initialization and only do the strict necessary, which is registering systems and events in the Bevy schedule, `.add_plugin(QuinnetClient/ServerPlugin { initialize_later: true })` can now be used, which will not create the Client/Server `Resource` immediately.
  - A command such as `commands.init_resource::<Client/Server>();` can be used later on when needed.
  - Client & server plugins systems are scheduled to only run if their respective resource exists.
  - Their respective `Resource` can be removed through a command at runtime.
- [client]: Fix IPv6 handling.
  - Remove `ConnectionConfiguration::new`
  - Add `ConnectionConfiguration::from_strings`, `ConnectionConfiguration::from_ips` and `ConnectionConfiguration::from_addrs`
- [server]: Fix IPv6 handling.
  - Rename `ServerConfigurationData` to `ServerConfiguration`.
  - Remove `ServerConfiguration::new`
  - Add `ServerConfiguration::from_string`, `ServerConfiguration::from_ip` and `ServerConfiguration::from_addr`
  - Add `server_hostname` to `CertificateRetrievalMode::GenerateSelfSigned` and `CertificateRetrievalMode::LoadFromFileOrGenerateSelfSigned`

## Version 0.3.0 (2023-01-20)

### Added

- [client & server] Add `OrderedReliable`, `UnorderedReliable` and `Unreliable` send channels. The existing API uses default channels, additional channel can be opened/closed with `open_channel`/`close_channel` and used with derivatives of `send_message_on`.
- [client & server] Now also receive `Unreliable` messages
- [client & server] Add a stats() function on a connection to retrieve statistics (thanks to [Andrewvy](https://github.com/andrewvy), PR [#4](https://github.com/Henauxg/bevy_quinnet/pull/4))
- [server] Add a clients() function to retrieve all the connected Client ids
- [tests] Add tests for channels, and move tests to cargo integration test directory

### Changed

- [server] `receive_message` and `receive_payload` functions are now `receive_message_from` and `receive_payload_from`, taking a ClientId as parameter (clients messages are now stored on separate queues to prevent a client from filling the shared queue)
- [client] Expose `ConnectionId` as `pub` in `ConnectionEvent` and `ConnectionLostEvent ` (thanks to [Zheilbron](https://github.com/zheilbron), PR [#5](https://github.com/Henauxg/bevy_quinnet/pull/5))
- [example:breakout] Make use of Unreliable and UnorderedReliable channels
- Updated dependencies

### Fixed
- [client & server] Enhancement on disconnection behaviours, existing outgoing messages are now flushed (thanks to [Zheilbron](https://github.com/zheilbron), PR [#6](https://github.com/Henauxg/bevy_quinnet/pull/6))
- [client] Do not fail in store_known_hosts_to_file if the path has no prefix
- [server] Do not fail in write_certs_to_files if the cert and key files have non-existing parent directories, create them instead

## Version 0.2.0 (2022-11-18)

### Added

- New events: ConnectionEvent and ConnectionLostEvent on both client & server
- Implemented Trust on First Use authentication scheme on the client
  - Added CertificateVerificationMode for client : SkipVerification, SignedByCertificateAuthority and TrustOnFirstUse
  - New client events for the certificate verification: CertInteractionEvent, CertTrustUpdateEvent and CertConnectionAbortEvent
- Added new ways to handle server certificates with CertificateRetrievalMode: GenerateSelfSigned, LoadFromFile & LoadFromFileOrGenerateSelfSigned
- Client can now have multiple connections simultaneously to multiple servers
- Server now returns the generated/loaded certificate
- It is now possible to host a server locally on a client
  - New example: Bevy breakout demo as a 2 players versus game, hosted from a client
- New open_connection & close_connection methods on the client
- New start_endpoint and close_endpoint on the server
- New is_listening method on the server
- New "try" methods that log the errors
- Added tests
- Added documentation

### Changed

- Client & Server configurations now taken as parameter at connection/listen time rather than from a resource. (User can still store it inside a resource)
- Raised DEFAULT_INTERNAL_MESSAGE_CHANNEL_SIZE in client to 100
- Use thiserror internally
- Update Quinn to 0.9
- Update Bevy to 0.9 (by [Lemonzy](https://github.com/Lemonzyy))
- Updated all dependencies (minors)
- Moved chat demo to its own directory

## Version 0.1.0 (2022-10-25)

Initial release