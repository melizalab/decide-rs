# decide

This workspace contains crates for the behavioral experiment framework `decide`.

## contents
- `decide-protocol`: protobuf definitions, error types, the `Component` trait, and trait implementations for transforming between `tmq::Multipart` messages and the types used internally
- `decide-core`: the main logic for initializing components and routing messages between clients and components
- `components/*`: crates with a type that implements `decide_protocol::Component`

## building and running

Since this workspace only contains a single binary crate, `decide-core`, you can use `cargo build` and `cargo run` as normal. If you want to set feature flags (e.g. `dummy-mode`, see [decide-core/src/lib.rs] for details), you can pass them from the command line, like
`cargo run --features dummy-mode`.

To compile for a specific architecture (BBB in our case), use `cross` (make sure you have `docker` installed):
```bash
cargo install cross
docker build -t decide-rs/image:tag ./
cross build --target armv7-unknown-linux-gnueabihf --release
```

## Logging
Logging level defaults to INFO, but can be overwritten with `export DECIDE_LOG="value"`

## running tests

The tests require a running instance of `decide`, otherwise they will hang.
```bash
mkdir -p ~/.config/decide/
ln -s components/lights/tests/components.yml ~/.config/decide/
cargo run &
cargo test
kill $(jobs -l -p)
```
