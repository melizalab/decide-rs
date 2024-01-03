use anyhow::Context;
use decide_core::{run, ComponentCollection};
use tracing_subscriber::filter::EnvFilter;
use time;

#[tokio::main(flavor = "multi_thread", worker_threads = 10)]
async fn main() -> anyhow::Result<()> {
    let timer_fmt = time::format_description::parse(
        "[year]-[month padding:zero]-[day padding:zero] [hour]:[minute]:[second]",
    ).expect(" Setting Timer Format ");
    let timer_offset = time::UtcOffset::current_local_offset()
        .unwrap_or_else(|_| time::UtcOffset::from_hms(-5, 0, 0).unwrap());
    let timer =
        tracing_subscriber::fmt::time::OffsetTime::new(timer_offset, timer_fmt);
    let filter = EnvFilter::try_from_env("DECIDE_LOG")
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();
    tracing_subscriber::fmt()
        .pretty()
        .with_thread_names(true)
        .with_env_filter(filter)
        .with_timer(timer)
        // enable everything
        // sets this to be the default, global collector for this application.
        .init();

    let (components, state_stream) =
        ComponentCollection::new().context("could not initialize controller")?;
    let res = run::launch_decide(components, state_stream)?;
    res.await
}
