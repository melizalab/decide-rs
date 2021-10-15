use decide_core::ComponentList;
use std::env;
use tmq::{reply, Context};
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "reply=DEBUG");
    }

    pretty_env_logger::init();

    let mut recv_sock = reply(&Context::new()).bind("tcp://127.0.0.1:7897")?;

    let mut components = ComponentList{};
    loop {
        let (multipart, send_sock) = recv_sock.recv().await?;
        let response = components.dispatch(&multipart.into())?;
        recv_sock = send_sock.send(response.into()).await?;
    }
}
