use anyhow::Result;
use decide_core::ComponentCollection;
use futures::{SinkExt, StreamExt};
use std::env;
use tmq::{router, Context};

#[tokio::main]
async fn main() -> Result<()> {
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "decide-core=DEBUG");
    }

    pretty_env_logger::init();

    let mut sock = router(&Context::new()).bind("tcp://127.0.0.1:7897")?;

    let mut components = ComponentCollection::new()?;

    loop {
        let request = sock.next().await.unwrap()?;
        let reply = components.dispatch(request).await;
        sock.send(reply).await.expect("failed to send");
    }
}
