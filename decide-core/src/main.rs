use anyhow::Result;
use decide_core::ComponentCollection;
use decide_proto::{PUB_ENDPOINT, REQ_ENDPOINT};
use futures::{SinkExt, StreamExt};
use std::env;
use tmq::{publish, router, Context};

#[tokio::main]
async fn main() -> Result<()> {
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "decide-core=DEBUG");
    }

    pretty_env_logger::init();

    let mut router_sock = router(&Context::new()).bind(REQ_ENDPOINT)?;
    let mut publish_sock = publish(&Context::new()).bind(PUB_ENDPOINT)?;

    let (mut components, mut state_stream) = ComponentCollection::new()?;

    tokio::spawn(async move {
        while let Some(state_update) = state_stream.next().await {
            publish_sock.send(state_update).await.unwrap();
        }
    });

    while let Some(request) = router_sock.next().await {
        let reply = components.dispatch(request?).await;
        router_sock.send(reply).await.expect("failed to send");
    }
    Ok(())
}
