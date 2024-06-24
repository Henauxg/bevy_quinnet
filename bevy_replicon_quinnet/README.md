<div align="center">

[![Bevy tracking](https://img.shields.io/badge/Bevy%20tracking-released%20version-lightblue)](https://github.com/bevyengine/bevy/blob/main/docs/plugins_guidelines.md#main-branch-tracking)
[![crates.io](https://img.shields.io/crates/v/bevy_replicon_quinnet)](https://crates.io/crates/bevy_replicon_quinnet)
[![bevy_quinnet on doc.rs](https://docs.rs/bevy_replicon_quinnet/badge.svg)](https://docs.rs/bevy_replicon_quinnet)

# Bevy Replicon Quinnet

An integration of [`bevy_quinnet`](https://github.com/Henauxg/bevy_quinnet) as a transport for [`bevy_replicon`](https://github.com/projectharmonia/bevy_replicon)

</div>

## Examples

_Examples were ported from [bevy_replicon_renet's examples](https://github.com/projectharmonia/bevy_replicon/tree/master/bevy_replicon_renet)_

<details>
  <summary>Simple box</summary>

Start a server with `cargo run --example simple_box server` and a client with `cargo run --example simple_box client`.

</details>

<details>
  <summary>Tic tac toe</summary>

Start a server with `cargo run --example tic_tac_toe server` and a client with `cargo run --example tic_tac_toe client`.

</details>

Sources for the examples can be found in the [examples](examples) directory.

## Compatible versions

| bevy | bevy_quinnet | bevy_replicon_quinnet | bevy_replicon |
| :--- | :----------- | :-------------------- | :------------ |
| 0.13 | 0.8          | 0.3                   | 0.26          |
| 0.13 | 0.8          | 0.2                   | 0.25          |
| 0.13 | 0.8          | 0.1                   | 0.24          |

## License

This crate is free and open source. All code in this repository is dual-licensed under either:

* MIT License ([LICENSE-MIT](LICENSE-MIT) or [http://opensource.org/licenses/MIT](http://opensource.org/licenses/MIT))
* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or [http://www.apache.org/licenses/LICENSE-2.0](http://www.apache.org/licenses/LICENSE-2.0))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
