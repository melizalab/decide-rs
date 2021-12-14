use decide_core::ComponentCollection;
use decide_proto::{decide::Reply, GeneralRequest, Request, RequestType, REQ_ENDPOINT};
use tmq::{request, Context, Multipart};

use test_context::{test_context, AsyncTestContext};

use std::sync::Once;

static INIT: Once = Once::new();

struct DecideContext {}

#[async_trait::async_trait]
impl AsyncTestContext for DecideContext {
    async fn setup() -> Self {
        INIT.call_once(|| {
            tokio::spawn(async {
                let config = "
            house-lights:
              driver: Lights
              config:
                pin: 4"
                    .as_bytes();
                let (components, state_stream) = ComponentCollection::from_reader(config).unwrap();
                let res = decide_core::run::launch_decide(components, state_stream).unwrap();
                res.await
            });
        });
        DecideContext {}
    }

    async fn teardown(self) {
        nix::sys::signal::kill(nix::unistd::Pid::this(), nix::sys::signal::Signal::SIGINT)
            .expect("cannot send ctrl-c");
    }
}

#[test_context(DecideContext)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[test_log::test]
async fn request_lock() -> anyhow::Result<()> {
    let ctx = Context::new();
    tracing::trace!("trying to connect");
    let req_sock = request(&ctx).connect(REQ_ENDPOINT)?;
    tracing::trace!("connected");

    let request = Request {
        request_type: RequestType::General(GeneralRequest::RequestLock),
        component: None,
        body: vec![],
    };
    let message = Multipart::from(request);
    tracing::trace!("trying to send message");
    let reply_sock = req_sock.send(message).await?;
    tracing::trace!("sent message");
    let (multipart, _req) = reply_sock.recv().await?;
    tracing::trace!("received reply");
    let reply = Reply::from(multipart);
    println!("{:?}", reply);
    assert!(reply.result.is_some());
    Ok(())
}
