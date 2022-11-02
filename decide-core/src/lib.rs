/*!
# Crate features

All are disabled by default.

## Debugging features

* **dummy-mode** -
  When enabled, replaces all component structs with
  automatically generated structs that have the same
  State, Params, and Config, but have no internal logic.
  In other words, they have zero side effects and their
  state will not change unless explicitly set.
*/
use anyhow::Context as AnyhowContext;
use decide_protocol::{
    error::{ClientError, ControllerError, DecideError},
    proto, ComponentName,
    ComponentRequest::{self, *},
    GeneralRequest::{self, *},
    Request, RequestType, Result,
};
use directories::ProjectDirs;
use futures::{future, stream, FutureExt, Stream, StreamExt};
use prost::Message;
use prost_types::Any;
use prost_types::Timestamp;
use serde::Deserialize;
use serde_value::Value;
use sha3::{digest::Update, Digest, Sha3_256};
use std::convert::TryFrom;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{collections::HashMap, time::Duration};
use std::{fs::File, io::Read};
use tmq::Multipart;
use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};
use tokio_stream::wrappers::ReceiverStream;
#[macro_use]
extern crate tracing;
use tracing::instrument;

mod components;
use components::ComponentKind;

pub mod run;

static SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

type RequestBundle = ((ComponentRequest, Vec<u8>), oneshot::Sender<proto::Reply>);

#[derive(Debug)]
pub struct ComponentCollection {
    components: HashMap<ComponentName, mpsc::Sender<RequestBundle>>,
    locked: bool,
    config_id: String,
}

#[derive(Deserialize, Debug)]
struct ComponentsConfig(HashMap<ComponentName, ComponentsConfigItem>);

#[derive(Deserialize, Debug)]
struct ComponentsConfigItem {
    driver: String,
    config: Value,
}

impl ComponentCollection {
    #[instrument]
    pub fn new() -> anyhow::Result<(Self, impl Stream<Item = Multipart>)> {
        let config_file = ProjectDirs::from("org", "meliza", "decide")
            .ok_or(ControllerError::NoConfigDir)?
            .config_dir()
            .join("components.yml");
        let reader = File::open(&config_file).map_err(|e| ControllerError::ConfigReadError {
            path: Some(config_file),
            source: e,
        })?;
        Self::from_reader(reader)
    }

    pub fn from_reader<T: Read>(
        mut config_reader: T,
    ) -> anyhow::Result<(Self, impl Stream<Item = Multipart>)> {
        let mut file_buf: Vec<u8> = Vec::new();
        config_reader
            .read_to_end(&mut file_buf)
            .map_err(|e| ControllerError::ConfigReadError {
                path: None,
                source: e,
            })?;
        let components_config: ComponentsConfig =
            serde_yaml::from_slice(&file_buf[..]).map_err(ControllerError::from)?;
        let config_id = Sha3_256::new().chain(&file_buf).finalize();
        let config_id = format!("{:x}", config_id);
        let (components, state_stream): (_, HashMap<_, _>) = components_config
            .0
            .into_iter()
            .map(|(name, item)| {
                let (request_tx, mut request_rx) = mpsc::channel::<RequestBundle>(100);
                let (state_tx, state_rx) = mpsc::channel::<Any>(100);
                let config = item.config.clone();
                let mut component =
                    ComponentKind::from_name(&item.driver[..], item.config, state_tx)
                        .with_context(|| format!("failed to initialize {:?}", name))?;
                let name_ = name.clone();
                tokio::spawn(async move {
                    component.init(config).await;
                    debug!("initializing {:?}", name_);
                    while let Some(((request_type, payload), reply_tx)) = request_rx.recv().await {
                        let reply = execute(&mut component, request_type, payload).await;
                        reply_tx
                            .send(reply.into())
                            .expect("controller dropped a oneshot receiver");
                        if request_type == ComponentShutdown {
                            break;
                        }
                    }
                });
                Ok(((name.clone(), request_tx), (name, state_rx.into())))
            })
            .collect::<anyhow::Result<Vec<_>>>()?
            .into_iter()
            .unzip();
        debug!("components initialized");
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
        let reply = proto::Reply::from(self.handle_request(request).await);
        let mut reply = Multipart::from(reply);
        reply.push_front(empty_frame);
        reply.push_front(client_id);
        reply
    }

    async fn handle_request(&mut self, request: Multipart) -> Result<proto::Reply> {
        let request = Request::try_from(request)?;
        info!("Received request {:?}", request);
        match request.request_type {
            RequestType::General(req) => self.handle_general(req, request.body).await,
            RequestType::Component(req) => self.handle_component(req, request).await,
        }
    }

    async fn handle_general(
        &mut self,
        request_type: GeneralRequest,
        payload: Vec<u8>,
    ) -> Result<proto::Reply> {
        Ok(match request_type {
            RequestLock => {
                self.request_lock(proto::Config::decode(&*payload).map_err(ClientError::from)?)?
            }
            ReleaseLock => self.release_lock()?,
            Shutdown => self.shutdown().await?,
        }
        .into())
    }

    async fn handle_component(
        &mut self,
        request_type: ComponentRequest,
        mut request: Request,
    ) -> Result<proto::Reply> {
        let component_name = request.component.take().unwrap();
        let component_tx = self
            .components
            .get_mut(&component_name)
            .ok_or(ClientError::UnknownComponent(component_name))?;
        let (reply_tx, reply_rx) = oneshot::channel();
        component_tx
            .send(((request_type, request.body), reply_tx))
            .await
            .expect("could not talk over mpsc");
        Ok(reply_rx.await.map_err(ControllerError::from)?)
    }

    fn request_lock(&mut self, config: proto::Config) -> Result<proto::reply::Result> {
        if self.locked {
            Err(ClientError::AlreadyLocked.into())
        } else if self.config_id != config.identifier {
            Err(ClientError::ConfigIdMismatch {
                client: config.identifier,
                controller: self.config_id.clone(),
            }
            .into())
        } else {
            self.locked = true;
            Ok(proto::reply::Result::Ok(()))
        }
    }

    fn release_lock(&mut self) -> Result<proto::reply::Result> {
        self.locked = false;
        Ok(proto::reply::Result::Ok(()))
    }

    async fn shutdown(&mut self) -> Result<proto::reply::Result> {
        future::join_all(self.components.iter().map(|(name, component_tx)| {
            let (reply_tx, _reply_rx) = oneshot::channel();

            let name = name.clone();
            timeout(
                SHUTDOWN_TIMEOUT,
                component_tx.send(((ComponentShutdown, Vec::new()), reply_tx)),
            )
            .map(|r| r.map_err(|_| ControllerError::ShutdownTimeout { component: name }))
        }))
        .await;
        Ok(proto::reply::Result::Ok(()))
    }
}

async fn execute(
    component: &mut ComponentKind,
    request_type: ComponentRequest,
    payload: Vec<u8>,
) -> Result<proto::Reply> {
    Ok(match request_type {
        ChangeState => {
            let state_change = proto::StateChange::decode(&*payload).map_err(ClientError::from)?;
            component.decode_and_change_state(state_change.state.ok_or(ClientError::NoState)?)?;
            proto::reply::Result::Ok(())
        }
        ResetState => {
            component.reset_state()?;
            proto::reply::Result::Ok(())
        }
        SetParameters => {
            let params = proto::ComponentParams::decode(&*payload).map_err(ClientError::from)?;
            component
                .decode_and_set_parameters(params.parameters.ok_or(ClientError::NoParameters)?)?;
            proto::reply::Result::Ok(())
        }
        GetParameters => {
            let params = component.get_encoded_parameters();
            proto::reply::Result::Params(params)
        }
        ComponentShutdown => {
            component.shutdown().await;
            proto::reply::Result::Ok(())
        }
    }
    .into())
}

fn build_pub_stream<I>(state_stream: I) -> impl Stream<Item = Multipart>
where
    I: IntoIterator<Item = (ComponentName, ReceiverStream<Any>)>,
{
    stream::select_all(
        state_stream
            .into_iter()
            .map(|(name, state_rx)| state_rx.map(move |state| (name.clone(), state))),
    )
    .map(|(name, state)| {
        let topic = String::from("state/") + &name.0;
        let pub_message = proto::Pub {
            state: Some(state),
            time: Some(Timestamp {
                seconds: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("time went backwards")
                    .as_secs() as i64,
                nanos: 0,
            }),
        };
        Multipart::from(vec![topic.as_bytes(), &pub_message.encode_to_vec()[..]])
    })
}
