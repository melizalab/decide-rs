use std::io::Result;
use protobuf_src;

extern crate prost_build;

fn main() -> Result<()> {
    std::env::set_var("PROTOC", protobuf_src::protoc());
    prost_build::compile_protos(&["src/decide.proto"], &["src/"])?;
    Ok(())
}
