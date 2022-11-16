# Certificates and server authentication

## Trust on first use

### Default configuration

Use the default configuration like this:
```rust
client.open_connection(/*...*/, CertificateVerificationMode::TrustOnFirstUse(TrustOnFirstUseConfig {
        ..Default::default()
    }),
);
```

With the default configuration, known hosts and their fingerprints are stored in a file, which defaults to `quinnet/known_hosts`.

The defaults verifier behaviours are:
- For an `unknown` certificate (first time this server is encountered) => the client trusts this certificate, stores its fingerprint and continue the connection;
- For a `trusted` certificate (the fingerprint matches the one stored for this server) => the client trusts this certificate and continue the connection;
- For an `untrusted` certificate (the certificate's fingerprint does not match the one in the store) => the client raises an event to the Bevy app and waits for an action to apply.
 
### Examples configurations

Default verifier behaviours with a custom store file:
```rust
client.open_connection(/*...*/, CertificateVerificationMode::TrustOnFirstUse(TrustOnFirstUseConfig {
        known_hosts: KnownHosts::HostsFile("MyCustomFile".to_string()),
        ..Default::default()
    }),
);
```

Custom verifier behaviours with a custom store:
```rust
client.open_connection(/*...*/, CertificateVerificationMode::TrustOnFirstUse(TrustOnFirstUseConfig {
        known_hosts: KnownHosts::Store(my_cert_store),
        verifier_behaviour: HashMap::from([
                (
                    CertVerificationStatus::UnknownCertificate,
                    CertVerifierBehaviour::ImmediateAction(CertVerifierAction::TrustAndStore),
                ),
                (
                    CertVerificationStatus::UntrustedCertificate,
                    CertVerifierBehaviour::ImmediateAction(CertVerifierAction::AbortConnection),
                ),
                (
                    CertVerificationStatus::TrustedCertificate,
                    CertVerifierBehaviour::ImmediateAction(CertVerifierAction::TrustOnce),
                ),
            ]),
    }),
);
```

### Events

The Quinnet client plugin raises Bevy events during the connection process (during the certificate verification).

- `CertInteractionEvent`: a user action is requested before continuing. This event is only raised when the verifier behaviour for a specific certificate status is set to `CertVerifierBehaviour::RequestClientAction`
- `CertTrustUpdateEvent`: the client plugin encoutered a new trust entry to register. If the store is a file, the client plugin has already updated it. If the store is a custom hashmap given to the client plugin (via `KnownHosts::Store(my_cert_store)`), it is up to the user to update its store accordingly.
- `CertConnectionAbortEvent`: signals that the connection was aborted during the certificate verification (through the `CertVerifierAction::AbortConnection`).

Here is a simple example for a custom handler of `CertInteractionEvent`:

```rust
fn handle_cert_events(mut cert_action_events: EventReader<CertInteractionEvent>) {
    // We may receive a CertInteractionEvent during the connection
    for cert_event in cert_action_events.iter() {
        match cert_event.status {
            // We want to abort the connection if the certificate is untrusted, else we continue
            CertVerificationStatus::UntrustedCertificate => cert_event
                .apply_cert_verifier_action(CertVerifierAction::AbortConnection)
                .unwrap(),
            _ => cert_event
                .apply_cert_verifier_action(CertVerifierAction::TrustOnce)
                .unwrap(),
        }
    }
}
```

### Fingerprints

Fingerprints in Quinnet are a SHA-256 hash of the certificate data in DER form.

### Known hosts file format

This hosts file format is really simplistic for now.
There is one line per entry, and each entry is a server name (as dns or ip) followed by a space, followed by the currently known fingerprint encoded in base64.

Example:
```
127.0.0.1 o1cpTe602uTq4pVwT+km8QtEPQE/xCAgk+3AicW/i9g=
123.123.123.123 kzXIwhvMSbWCQOimT3btnFlmc/Lq0UN0JhSeQadaGbg=
```

#### Limitations 

This simple format implies that if two servers are hosted on the same machine on two different ports, they should currently share the same certificate to avoid any conflict.
