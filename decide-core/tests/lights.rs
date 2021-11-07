use decide_core::ComponentCollection;
use num_traits::ToPrimitive;
use decide_proto::{decide, GeneralRequest, DECIDE_VERSION};
use futures::{SinkExt, StreamExt};
use tmq::{router, request, Context};

async fn setup() {
    let config = "
        house-lights:
          driver: Lights
          config:
            pin: 4
    ".as_bytes();
    let mut components = ComponentCollection::from_reader(config).unwrap();

    let mut sock = router(&Context::new()).bind("tcp://127.0.0.1:7897").unwrap();

    loop {
        let request = sock.next().await.unwrap().unwrap();
        let reply = components.dispatch(request).await;
        sock.send(reply).await.expect("failed to send");
    }
}

#[tokio::test]
async fn request_lock() {
    tokio::spawn( async {
        setup().await;
    }).await.unwrap();

    let mut send_sock = request(&Context::new()).connect("tcp://127.0.0.1:7897").unwrap();
    let request_type = &[GeneralRequest::RequestLock.to_u8().unwrap()];
    let payload = &[0];
    let message = vec![&DECIDE_VERSION[..], request_type, payload];
    let recv_sock = send_sock.send(message.into()).await.unwrap();
    let (msg, send) = recv_sock.recv().await.unwrap();
    assert_eq!(msg, vec![vec![1, 2]].into());
}
