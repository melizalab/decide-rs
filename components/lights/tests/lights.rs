use decide_core::{run, ComponentCollection};
use decide_protocol::{
    proto::{reply, ComponentParams, Config, Pub, Reply, StateChange},
    Component, ComponentName, ComponentRequest, GeneralRequest, Request, RequestType, PUB_ENDPOINT,
    REQ_ENDPOINT,
};
use futures::{Stream, StreamExt};
use lights::Lights;
use prost::Message;
use prost_types::Any;
use tmq::{request, subscribe, Context, Multipart};
use tokio::test;
#[macro_use]
extern crate tracing;
use rstest::*;

struct Decide;

impl Drop for Decide {
    fn drop(&mut self) {
        panic!("darn");
    }
}

#[fixture]
#[once]
fn decide() -> Decide {
    tokio::spawn(async {
        let config = "
            house-lights:
              driver: Lights
              config:
                pin: 4";
        let (components, state_stream) = ComponentCollection::from_reader(config.as_bytes())?;
        let res = run::launch_decide(components, state_stream)?;
        res.await
    });
    return Decide;
}

async fn send_request(message: Request) -> anyhow::Result<reply::Result> {
    let ctx = Context::new();
    trace!("trying to connect");
    let req_sock = request(&ctx).connect(REQ_ENDPOINT)?;
    trace!("connected");

    let message = Multipart::from(message);
    trace!("trying to send message");
    let reply_sock = req_sock.send(message).await?;
    trace!("sent message");
    let (multipart, _req) = reply_sock.recv().await?;
    trace!("received reply");
    let reply = Reply::from(multipart);
    println!("{:?}", reply);
    Ok(reply.result.unwrap())
}

fn pub_stream(topic: &[u8]) -> anyhow::Result<impl Stream<Item = Pub>> {
    let socket = subscribe(&Context::new())
        .connect(PUB_ENDPOINT)?
        .subscribe(topic)?
        .map(|message| {
            let mut message = message.unwrap();
            trace!("received pub {:?}", &message);
            let _topic = message.pop_front().unwrap();
            let encoded_pub = message.pop_front().unwrap();
            Pub::decode(&encoded_pub[..]).expect("could not decode protobuf")
        });
    Ok(socket)
}

macro_rules! lock {
    () => {{
        let config = Config {
            identifier: String::from(
                "39c12c22af89685008eb9725a40b94089dfa36d27cfc0cdb912629c6ff2de50e",
            ),
        };
        let request = Request {
            request_type: RequestType::General(GeneralRequest::RequestLock),
            component: None,
            body: config.encode_to_vec(),
        };
        let result = send_request(request).await?;
        result
    }};
}

macro_rules! unlock {
    () => {{
        let request = Request {
            request_type: RequestType::General(GeneralRequest::ReleaseLock),
            component: None,
            body: vec![],
        };
        let result = send_request(request).await?;
        result
    }};
}

#[rstest]
#[test]
async fn locking_behavior(decide: &Decide) -> anyhow::Result<()> {
    let result = lock!();
    assert_eq!(result, reply::Result::Ok(()));
    let result = lock!();
    assert_eq!(
        result,
        reply::Result::Error(String::from("controller is already locked"))
    );
    let result = unlock!();
    assert_eq!(result, reply::Result::Ok(()));
    let result = lock!();
    assert_eq!(result, reply::Result::Ok(()));
    unlock!();
    Ok(())
}

#[rstest]
#[test]
async fn parameters(decide: &Decide) {
    let params = Any {
        type_url: String::from(Lights::PARAMS_TYPE_URL),
        value: lights::proto::Params { blink: false }.encode_to_vec(),
    };
    let params_message = ComponentParams {
        parameters: Some(params.clone()),
    };
    let request = Request {
        request_type: RequestType::Component(ComponentRequest::SetParameters),
        component: Some(ComponentName(String::from("house-lights"))),
        body: params_message.encode_to_vec(),
    };
    let result = send_request(request).await.unwrap();
    assert_eq!(result, reply::Result::Ok(()));
    let request = Request {
        request_type: RequestType::Component(ComponentRequest::GetParameters),
        component: Some(ComponentName::from("house-lights")),
        body: vec![],
    };
    let result = send_request(request).await.unwrap();
    assert_eq!(result, reply::Result::Params(params));
}

#[rstest]
#[test]
async fn state(decide: &Decide) {
    let state = Any {
        type_url: String::from(Lights::STATE_TYPE_URL),
        value: lights::proto::State { on: true }.encode_to_vec(),
    };
    let state_message = StateChange {
        state: Some(state.clone()),
    };
    let request = Request {
        request_type: RequestType::Component(ComponentRequest::ChangeState),
        component: Some(ComponentName::from("house-lights")),
        body: state_message.encode_to_vec(),
    };
    // the subscriber must be initialized before the state change is
    // sent because the publish socket doesn't buffer messages
    let mut state_stream = pub_stream(b"state/house-lights").unwrap();
    let result = send_request(request).await.unwrap();
    assert_eq!(result, reply::Result::Ok(()));
    trace!("waiting for pub");
    let state_update = state_stream.next().await.unwrap();
    assert_eq!(state_update.state.unwrap(), state);
}
