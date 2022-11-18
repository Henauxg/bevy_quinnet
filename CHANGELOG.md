# Changelog

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