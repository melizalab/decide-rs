use std::io::Result;

extern crate prost_build;

fn main() -> Result<()> {
    prost_build::compile_protos(&["src/house_light.proto"], &["src/"])?;
    Ok(())
}