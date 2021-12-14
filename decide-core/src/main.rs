use anyhow::Context;
use decide_core::{run, ComponentCollection};

#[tokio::main(flavor = "multi_thread", worker_threads = 10)]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .pretty()
        .with_thread_names(true)
        // enable everything
        .with_max_level(tracing::Level::TRACE)
        // sets this to be the default, global collector for this application.
        .init();

    tracing::debug!("hi");

    let (components, state_stream) =
        ComponentCollection::new().context("could not initialize controller")?;
    let res = run::launch_decide(components, state_stream)?;
    res.await
}
