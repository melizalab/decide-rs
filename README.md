# decide

This workspace contains crates for the behavioral experiment framework `decide`.

## contents
- `decide-proto`: protobuf defintions, the error type used for everything, the `Component` trait, and trait implementations for transforming between `tmq::Multipart` messages and the types used internally
- `decide-core`: the main logic for initializing components and routing messages between clients and components
- `components/*`: crates with a type that implements `decide_proto::Component`


## running tests

The tests in `decide-core` require a running instance of `decide`, otherwise they will hang. Follow these steps to run tests:
```bash
mkdir -p ~/.config/decide/
ln -s decide-core/tests/components.yml ~/.config/decide/
cargo run &
cargo test
```
