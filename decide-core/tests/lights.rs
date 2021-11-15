use decide_core::ComponentCollection;
use decide_proto::{decide, GeneralRequest, DECIDE_VERSION};
use futures::{SinkExt, StreamExt};
use num_traits::ToPrimitive;
use tmq::{request, router, Context, Multipart};
use std::rc::Rc;

async fn setup() -> (tokio::task::LocalSet, Rc<Context>) {
    let tasks = tokio::task::LocalSet::new();
    let context = Rc::new(Context::new());
    let task_ctx = context.clone();
    tasks.spawn_local( async move {
//        let config = "
//        house-lights:
//          driver: Lights
//          config:
//            pin: 4"
//        .as_bytes();
//        let (mut components, _) = ComponentCollection::from_reader(config).unwrap();


    panic!("oops");
    let mut sock = router(&task_ctx)
        .bind("tcp://127.0.0.1:7897")
        .unwrap();

    while let Some(request) = sock.next().await {
        let request = request.unwrap();
        //let reply = components.dispatch(request).await;
        sock.send(request).await.expect("failed to send");
    }
    });
    (tasks, context)
}

//#[tokio::test]
async fn request_lock() {
    console_subscriber::init();


    let (tasks, context): (_, Rc<Context>) = setup().await;

    let mut send_sock = request(&context.clone())
        .connect("tcp://127.0.0.1:7897")
        .unwrap();
    let request_type = &[GeneralRequest::RequestLock.to_u8().unwrap()];
    let payload = &[0];
    let message = vec![&DECIDE_VERSION[..], request_type, payload];
    let recv_sock = send_sock.send(message.into()).await.unwrap();
    let (msg, _) = recv_sock.recv().await.unwrap();
    assert_eq!(msg, vec![vec![1, 2]].into());
}

//#[tokio::test]
async fn basic() {
    let endpoint = "tcp://127.0.0.1:5555".to_string();
    let ctx = Rc::new(Context::new());

    let mut runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build().unwrap();
    let tasks = tokio::task::LocalSet::new();

    let router_task = {
        let ctx = ctx.clone();
        let endpoint = endpoint.clone();
        tasks.spawn_local(async move {
                let (mut router_tx, mut router_rx) = router(&ctx).bind(&endpoint).unwrap().split();
                let request = router_rx.next().await.unwrap().unwrap();
                router_tx.send(request).await.unwrap();
        })
    };

    let request_task = {
        let ctx = ctx.clone();
        let endpoint = endpoint.clone();
        tasks.spawn_local(async move {
            let send_sock = request(&ctx).connect(&endpoint).unwrap();
            let message = Multipart::from(vec!["hi"]);
            let recv = send_sock.send(message).await.unwrap();
            let (reply, _) = recv.recv().await.unwrap();
            assert_eq!(reply, Multipart::from(vec!["hi"]));
        })
    };
    router_task.await.unwrap();
    request_task.await.unwrap();
}

use zmq::{SocketType};

use tmq::{Result};
use utils::{generate_tcp_address, msg, sync_echo};
use std::thread::{spawn, JoinHandle};


mod utils;

#[tokio::test]
async fn single_message() -> Result<()> {
    let address = generate_tcp_address();
    let ctx = Context::new();
    let requester = request(&ctx).connect(&address)?;

    let echo = sync_echo(address, SocketType::REP, 1);

    let m1 = "Msg";
    let m2 = "Msg (contd.)";
    let message = Multipart::from(vec![msg(m1.as_bytes()), msg(m2.as_bytes())]);
    let reply_receiver = requester.send(message).await?;
    if let Ok((multipart, _)) = reply_receiver.recv().await {
        let expected: Multipart = vec![msg(m1.as_bytes()), msg(m2.as_bytes())].into();
        assert_eq!(expected, multipart);
    } else {
        panic!("Reply is missing.");
    }

    echo.join().unwrap();

    Ok(())
}

#[tokio::test]
async fn request_hammer() -> Result<()> {
    let address = generate_tcp_address();
    let ctx = Context::new();
    let mut req_sock = request(&ctx).connect(&address)?;

    let count = 1_000;
    let echo = sync_echo(address, SocketType::REP, count);

    for i in 0..count {
        let m1 = format!("Msg #{}", i);
        let m2 = format!("Msg #{} (contd.)", i);
        let message = Multipart::from(vec![msg(m1.as_bytes()), msg(m2.as_bytes())]);
        let reply_sock = req_sock.send(message).await?;
        let (multipart, req) = reply_sock.recv().await?;
        req_sock = req;
        let expected: Multipart = vec![msg(m1.as_bytes()), msg(m2.as_bytes())].into();
        assert_eq!(expected, multipart);
    }

    echo.join().unwrap();

    Ok(())
}

#[tokio::test]
async fn router_hammer() -> Result<()> {
    let address = generate_tcp_address();
    let ctx = Context::new();
    let mut req_sock = request(&ctx).connect(&address)?;

    let count = 1_000;
    let echo = start_decide(address, count);

    for i in 0..count {
        let m1 = format!("Msg #{}", i);
        let m2 = format!("Msg #{} (contd.)", i);
        let message = Multipart::from(vec![msg(m1.as_bytes()), msg(m2.as_bytes())]);
        let reply_sock = req_sock.send(message).await?;
        let (multipart, req) = reply_sock.recv().await?;
        req_sock = req;
        let expected: Multipart = vec![msg(m1.as_bytes()), msg(m2.as_bytes())].into();
        assert_eq!(expected, multipart);
    }

    echo.join().unwrap();

    Ok(())
}

pub fn start_decide(address: String, count: u32) -> JoinHandle<()> {
    spawn(move || {
        let socket = Context::new().socket(SocketType::ROUTER).unwrap();
        socket.bind(&address).unwrap();

        let config = "
        house-lights:
          driver: Lights
          config:
            pin: 4"
        .as_bytes();
        let (mut components, _) = ComponentCollection::from_reader(config).unwrap();
        for _ in 0..count {
            let received = socket.recv_multipart(0).unwrap();
            //let reply = components.dispatch(received).await;
            let reply = received;
            socket.send_multipart(reply, 0).unwrap();
        }
    })
}
