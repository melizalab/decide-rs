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
use sha3::{Digest, Sha3_256};
use tmq::{request, subscribe, Context, Multipart};
use tokio::sync::Mutex;
use tokio::test;
#[macro_use]
extern crate tracing;
use rstest::*;

const HOUSE_LIGHTS_CONFIG: &str = "
            house-lights:
              driver: Lights
              config:
                pin: 4";

// derived from the config text itself so it can never drift out of sync,
// the way the old hardcoded literal did
fn config_identifier() -> String {
    format!("{:x}", Sha3_256::digest(HOUSE_LIGHTS_CONFIG.as_bytes()))
}

struct Decide;

impl Drop for Decide {
    fn drop(&mut self) {
        panic!("darn");
    }
}

#[fixture]
#[once]
fn decide() -> Decide {
    // Run on a dedicated thread with its own long-lived runtime, rather than
    // tokio::spawn on whichever test's runtime happens to invoke this fixture
    // first: #[tokio::test] gives every test its own runtime that's torn down
    // when that test returns, which would kill the server along with it.
    std::thread::spawn(|| {
        let rt = tokio::runtime::Runtime::new().expect("failed to build decide-core runtime");
        rt.block_on(async {
            let (components, state_stream) =
                ComponentCollection::from_reader(HOUSE_LIGHTS_CONFIG.as_bytes())?;
            let res = run::launch_decide(components, state_stream)?;
            res.await
        })
        .expect("decide-core instance exited with an error");
    });
    // give the dedicated thread a moment to bind the ZMQ sockets before any
    // test tries to connect
    std::thread::sleep(std::time::Duration::from_millis(100));
    Decide
}

// the three tests below all talk to the single shared `decide` instance
// above, so they can't run concurrently without racing on shared lock/component
// state; this fixture serializes their bodies regardless of how the test
// harness schedules them across threads
#[fixture]
#[once]
fn test_lock() -> Mutex<()> {
    Mutex::new(())
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
            identifier: config_identifier(),
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
async fn locking_behavior(decide: &Decide, test_lock: &Mutex<()>) -> anyhow::Result<()> {
    let _guard = test_lock.lock().await;
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
async fn parameters(decide: &Decide, test_lock: &Mutex<()>) {
    let _guard = test_lock.lock().await;
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
async fn state(decide: &Decide, test_lock: &Mutex<()>) {
    let _guard = test_lock.lock().await;
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
    // sent because the publish socket doesn't buffer messages; ZMQ's
    // subscription handshake is also async, so give it a moment to actually
    // reach the publisher before triggering the state change (the "slow
    // joiner" problem)
    let mut state_stream = pub_stream(b"state/house-lights").unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let result = send_request(request).await.unwrap();
    assert_eq!(result, reply::Result::Ok(()));
    trace!("waiting for pub");
    let state_update = state_stream.next().await.unwrap();
    assert_eq!(state_update.state.unwrap(), state);
}
