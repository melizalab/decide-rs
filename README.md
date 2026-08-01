# decide

This workspace contains crates for the behavioral experiment framework `decide`.

## contents
- `decide-protocol`: protobuf definitions, error types, the `Component` trait, and trait implementations for transforming between `tmq::Multipart` messages and the types used internally
- `decide-core`: the main logic for initializing components and routing messages between clients and components
- `components/*`: crates with a type that implements `decide_protocol::Component`

## building and running

Since this workspace only contains a single binary crate, `decide-core`, you can use `cargo build` and `cargo run` as normal. If you want to set feature flags (e.g. `dummy-mode`, see [decide-core/src/lib.rs] for details), you can pass them from the command line, like
`cargo run --features dummy-mode`.

To compile for a specific architecture (Beaglebone Black, in our case), use `cross` (requires `docker` or `podman`):
```bash
cargo install cross
cross build --target armv7-unknown-linux-gnueabihf --release
```

## logging
Logging level defaults to INFO, but can be overwritten with `export DECIDE_LOG="value"`

## adding components

1. Design component as a separate crate ("package") under `./components/` according to the [protocol](PROTOCOL.md). Follow `./components/lights` as an example.
2. Include component crate in `decide-core` dependencies (`./decide-core/Cargo.toml`).
3. Import `Component`-implemented struct and add to list of components in `impl_components!` macro in `./decide-core/src/components.rs`
4. Include component crate in the main cargo manifest  (`./Cargo.toml`)

## running tests

Just run `cargo test` (or `cargo test -p <crate>`) as normal — no external
setup required.

`components/lights` is currently the only crate with integration tests.
Its test suite spins up its own `decide-core` instance in-process (on a
dedicated thread with its own Tokio runtime, so it stays alive for the
whole test binary) bound to the standard ZMQ endpoints
(`tcp://127.0.0.1:7897`/`7898`). Because of that, **do not** have a
separate `cargo run`/`decide-core` instance running at the same time you
run its tests — it will fight over the same ports and the tests will fail
or hang.
