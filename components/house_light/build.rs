use std::io::Result;

extern crate prost_build;

fn main() -> Result<()> {
    std::env::set_var("PROTOC", protobuf_src::protoc());
    prost_build::compile_protos(&["src/house_light.proto"], &["src/"])?;
    Ok(())
}