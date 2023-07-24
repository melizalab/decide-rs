use anyhow::Context;
use decide_core::{run, ComponentCollection};
use tracing_subscriber::filter::EnvFilter;

#[tokio::main(flavor = "multi_thread", worker_threads = 10)]
async fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_env("DECIDE_LOG")
        .or_else(|_| EnvFilter::try_new("debug"))
        .unwrap();
    tracing_subscriber::fmt()
        .pretty()
        .with_thread_names(true)
        .with_env_filter(filter)
        // enable everything
        // sets this to be the default, global collector for this application.
        .init();

    let (components, state_stream) =
        ComponentCollection::new().context("could not initialize controller")?;
    let res = run::launch_decide(components, state_stream)?;
    res.await
}
