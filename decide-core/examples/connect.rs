use decide_protocol::{proto::Reply, GeneralRequest, Request, RequestType, REQ_ENDPOINT};
use tmq::{request, Context, Multipart};

pub fn msg(bytes: &[u8]) -> zmq::Message {
    zmq::Message::from(bytes)
}

#[tokio::main]
async fn main() {
    let ctx = Context::new();
    let mut req_sock = request(&ctx).connect(REQ_ENDPOINT).unwrap();

    let request = Request {
        request_type: RequestType::General(GeneralRequest::RequestLock),
        component: None,
        body: vec![],
    };
    let message = Multipart::from(request);
    let reply_sock = req_sock.send(message).await.unwrap();
    let (multipart, req) = reply_sock.recv().await.unwrap();
    req_sock = req;
    println!("{:?}", Reply::from(multipart));
}
