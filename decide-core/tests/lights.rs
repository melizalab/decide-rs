use decide_proto::{decide::Reply, GeneralRequest, Request, RequestType, REQ_ENDPOINT};
use tmq::{request, Context, Multipart};

async fn send_request(message: Request) -> anyhow::Result<Reply> {
    let ctx = Context::new();
    tracing::trace!("trying to connect");
    let req_sock = request(&ctx).connect(REQ_ENDPOINT)?;
    tracing::trace!("connected");

    let message = Multipart::from(message);
    tracing::trace!("trying to send message");
    let reply_sock = req_sock.send(message).await?;
    tracing::trace!("sent message");
    let (multipart, _req) = reply_sock.recv().await?;
    tracing::trace!("received reply");
    let reply = Reply::from(multipart);
    println!("{:?}", reply);
    Ok(reply)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[test_log::test]
async fn request_lock() -> anyhow::Result<()> {
    let request = Request {
        request_type: RequestType::General(GeneralRequest::RequestLock),
        component: None,
        body: vec![],
    };
    let reply = send_request(request).await?;
    assert!(reply.result.is_some());
    Ok(())
}
