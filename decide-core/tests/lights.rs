use decide_core::ComponentCollection;
use decide_proto::{
    decide, decide::Reply, GeneralRequest, Request, RequestType, DECIDE_VERSION, REQ_ENDPOINT,
};
use futures::{SinkExt, StreamExt};
use num_traits::ToPrimitive;
use std::rc::Rc;
use tmq::{request, router, Context, Multipart};

use zmq::SocketType;

use std::thread::{spawn, JoinHandle};
use tmq::Result;
use utils::{generate_tcp_address, msg, sync_echo};

mod utils;

#[test_log::test]
fn log() {
    tracing::info!("hi");
}

use std::sync::Once;

static INIT: Once = Once::new();

pub fn initialize() {
    INIT.call_once(|| {
        tokio::spawn(async move {
            let config = "
            house-lights:
              driver: Lights
              config:
                pin: 4"
                .as_bytes();
            let (components, state_stream) = ComponentCollection::from_reader(config).unwrap();
            decide_core::launch_decide(components, state_stream).await;
        });
    });
}

#[tokio::test]
async fn request_lock() {
    initialize();
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
