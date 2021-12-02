use decide_proto::{
    decide, ComponentName,
    ComponentRequest::{self, *},
    error::{ClientError, ControllerError},
    GeneralRequest::{self, *},
    Request, RequestType, Result,
};
use directories::ProjectDirs;
use futures::{stream, Stream, StreamExt};
use prost::Message;
use prost_types::Any;
use prost_types::Timestamp;
use serde::Deserialize;
use serde_value::Value;
use sha3::{Digest, Sha3_256};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs::File, io::Read};
use tmq::Multipart;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

mod components;
use components::ComponentKind;

type RequestBundle = ((ComponentRequest, Vec<u8>), oneshot::Sender<decide::Reply>);
pub struct ComponentCollection {
    components: HashMap<ComponentName, mpsc::Sender<RequestBundle>>,
    locked: bool,
    config_id: String,
}

#[derive(Deserialize)]
struct ComponentsConfig(HashMap<ComponentName, ComponentsConfigItem>);

#[derive(Deserialize)]
struct ComponentsConfigItem {
    driver: String,
    config: Value,
}

impl ComponentCollection {
    pub fn new() -> Result<(Self, impl Stream<Item = decide::Pub>)> {
        let config_file = ProjectDirs::from("org", "meliza", "decide")
            .ok_or_else(|| ControllerError::NoConfigDir)?
            .config_dir()
            .join("components.yml");
        let reader = File::open(&config_file).map_err(|e| ControllerError::ConfigReadError{
            path: Some(config_file),
            source: e
        })?;
        Self::from_reader(reader)
    }

    pub fn from_reader<T: Read>(
        mut config_reader: T,
    ) -> Result<(Self, impl Stream<Item = decide::Pub>)> {
        let mut file_buf: Vec<u8> = Vec::new();
        config_reader
            .read_to_end(&mut file_buf)
            .map_err(|e| ControllerError::ConfigReadError {
                path: None,
                source: e
            })?;
        let components_config: ComponentsConfig = serde_yaml::from_slice(&file_buf[..]).map_err(|e| ControllerError::from(e))?;
        let config_id = Sha3_256::new().chain(&file_buf).finalize();
        let config_id = format!("{:?}", config_id.as_slice());
        let (components, state_stream): (_, HashMap<_, _>) =
            components_config.0.into_iter().map(init_component).unzip();
        let pub_stream = build_pub_stream(state_stream);
        Ok((
            ComponentCollection {
                components,
                config_id,
                locked: false,
            },
            pub_stream,
        ))
    }

    pub async fn dispatch(&mut self, mut request: Multipart) -> Multipart {
        let client_id = request.pop_front().unwrap();
        let empty_frame = request.pop_front().unwrap();
        let reply = decide::Reply::from(self.handle_request(request).await);
        let mut reply = Multipart::from(reply);
        reply.push_front(empty_frame);
        reply.push_front(client_id);
        reply
    }

    async fn handle_request(&mut self, request: Multipart) -> Result<decide::Reply> {
        let request = Request::try_from(request)?;
        match request.request_type {
            RequestType::General(req) => self.handle_general(req, request.body),
            RequestType::Component(req) => self.handle_component(req, request).await,
        }
    }

    fn handle_general(
        &mut self,
        request_type: GeneralRequest,
        payload: Vec<u8>,
    ) -> Result<decide::Reply> {
        Ok(match request_type {
            RequestLock => self.request_lock(decide::Config::decode(&*payload).map_err(|e| ClientError::from(e))?)?,
            ReleaseLock => self.release_lock()?,
        }
        .into())
    }

    async fn handle_component(
        &mut self,
        request_type: ComponentRequest,
        mut request: Request,
    ) -> Result<decide::Reply> {
        let component_name = request.component.take().unwrap();
        let component_tx = self
            .components
            .get_mut(&component_name)
            .ok_or_else(|| ClientError::UnknownComponent(component_name))?;
        let (reply_tx, reply_rx) = oneshot::channel();
        component_tx
            .send(((request_type, request.body), reply_tx))
            .await
            .expect("could not talk over mpsc");
        Ok(reply_rx.await.map_err(|e| ControllerError::from(e))?)
    }

    fn request_lock(&mut self, config: decide::Config) -> Result<decide::reply::Result> {
        if self.locked {
            Err(ClientError::AlreadyLocked.into())
        } else if self.config_id != config.identifier {
            Err(ClientError::ConfigIdMismatch {
                client: config.identifier,
                controller: self.config_id.clone(),
            }.into())
        } else {
            self.locked = true;
            Ok(decide::reply::Result::Ok(()))
        }
    }

    fn release_lock(&mut self) -> Result<decide::reply::Result> {
        self.locked = false;
        Ok(decide::reply::Result::Ok(()))
    }
}

fn execute(
    component: &mut ComponentKind,
    request_type: ComponentRequest,
    payload: Vec<u8>,
) -> Result<decide::Reply> {
    Ok(match request_type {
        ChangeState => {
            let state_change = decide::StateChange::decode(&*payload).map_err(|e| ClientError::from(e))?;
            component
                .decode_and_change_state(state_change.state.ok_or_else(|| ClientError::NoState)?)?;
            decide::reply::Result::Ok(())
        }
        ResetState => {
            component.reset_state()?;
            decide::reply::Result::Ok(())
        }
        SetParameters => {
            let params = decide::ComponentParams::decode(&*payload).map_err(|e| ClientError::from(e))?;
            component.decode_and_set_parameters(
                params.parameters.ok_or_else(|| ClientError::NoParameters)?,
            )?;
            decide::reply::Result::Ok(())
        }
        GetParameters => {
            let params = component.get_encoded_parameters();
            decide::reply::Result::Params(params)
        }
    }
    .into())
}

fn build_pub_stream<I>(state_stream: I) -> impl Stream<Item = decide::Pub>
where
    I: IntoIterator<Item = (ComponentName, ReceiverStream<Any>)>,
{
    stream::select_all(
        state_stream
            .into_iter()
            .map(|(name, state_rx)| state_rx.map(move |state| (name.clone(), state))),
    )
    .map(|(name, state)| decide::Pub {
        state: Some(state),
        time: Some(Timestamp {
            seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_secs() as i64,
            nanos: 0,
        }),
    })
}

fn init_component(
    (name, item): (ComponentName, ComponentsConfigItem),
) -> (
    (ComponentName, mpsc::Sender<RequestBundle>),
    (ComponentName, ReceiverStream<Any>),
) {
    let (request_tx, mut request_rx) = mpsc::channel::<RequestBundle>(100);
    let (state_tx, state_rx) = mpsc::channel::<Any>(100);
    let mut component = ComponentKind::try_from((&item.driver[..], item.config)).unwrap();
    tokio::spawn(async move {
        component.init(state_tx).await;
        while let Some(((request_type, payload), reply_tx)) = request_rx.recv().await {
            let reply = execute(&mut component, request_type, payload);
            reply_tx
                .send(reply.into())
                .expect("controller dropped a oneshot receiver");
        }
    });
    ((name.clone(), request_tx), (name, state_rx.into()))
}
