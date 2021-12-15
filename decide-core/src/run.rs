use super::ComponentCollection;
use decide_proto::{PUB_ENDPOINT, REQ_ENDPOINT};
use futures::{
    future::{self, Future, FutureExt},
    SinkExt, Stream, StreamExt,
};
use tmq::{Context, Multipart};
use tokio::sync::oneshot;

pub fn launch_decide<S>(
    components: ComponentCollection,
    state_stream: S,
) -> Result<impl Future<Output = anyhow::Result<()>>, oneshot::error::RecvError>
where
    S: Stream<Item = Multipart> + Unpin + Send + 'static,
{
    let (tx_pub, rx_pub) = oneshot::channel();
    tokio::spawn(async move {
        tx_pub
            .send(process_pubs(state_stream).await)
            .expect("failed to send result");
    });
    let (tx_req, rx_req) = oneshot::channel();
    tokio::spawn(async move {
        tx_req
            .send(process_requests(components).await)
            .expect("failed to send result");
    });
    Ok(future::select_all(vec![rx_pub, rx_req]).map(|(res, _, _)| res?))
}

async fn process_pubs<S>(mut state_stream: S) -> anyhow::Result<()>
where
    S: Stream<Item = Multipart> + Unpin + Send + 'static,
{
    let mut publish_sock = tmq::publish(&Context::new()).bind(PUB_ENDPOINT)?;
    while let Some(state_update) = state_stream.next().await {
        publish_sock.send(state_update).await.unwrap();
    }
    Ok(())
}

async fn process_requests(mut components: ComponentCollection) -> anyhow::Result<()> {
    let mut router_sock = tmq::router(&Context::new()).bind(REQ_ENDPOINT)?;
    while let Some(request) = router_sock.next().await {
        let reply = components.dispatch(request?).await;
        router_sock.send(reply).await?;
    }
    Ok(())
}
