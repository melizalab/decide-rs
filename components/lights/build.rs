use std::io::Result;

extern crate prost_build;

fn main() -> Result<()> {
    prost_build::compile_protos(&["src/lights.proto"], &["src/"])?;
    Ok(())
}
