use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::read_dir;
use std::ops::Deref;
use std::path::Path;
use std::sync::{Arc, Mutex, Condvar, atomic, mpsc as std_mpsc};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use alsa::{Direction, pcm::{Access, Format, HwParams, PCM, State},
           ValueOr};
use async_trait::async_trait;
use audrey::read::BufFileReader;
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use tokio::{self, sync::mpsc::{self, Sender}};

use decide_protocol::{Component,
                      error::{DecideError}
};
use futures::executor::block_on;
use proto::state::PlayBack;

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}
#[derive(Deserialize)]
pub struct Config {
    audio_device: String,
    sample_rate: u32,
    channel: u32,
}
pub struct AlsaPlayback {
    audio_dir: Arc<Mutex<String>>,
    audio_id: Arc<Mutex<String>>,
    audio_count: Arc<AtomicU32>,
    channel: Arc<AtomicBool>,
    playback: Arc<(Mutex<PlayBack>, Condvar)>, //pause or resume
    elapsed: Arc<Mutex<Option<prost_types::Duration>>>,
    playback_queue: Arc<Mutex<Option<HashMap<OsString, Vec<i16>>>>>,
    state_sender: Sender<Any>,
    import_switch: Arc<(Mutex<bool>, Condvar)>,
    shutdown: Option<(thread::JoinHandle<()>,std_mpsc::Sender<bool>)>,
}

#[async_trait]
impl Component for AlsaPlayback {
    type State = proto::State;
    type Params = proto::Params;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "melizalab.org/proto/sound_alsa_state";
    const PARAMS_TYPE_URL: &'static str = "melizalab.org/proto/sound_alsa_params";

    fn new(_config: Self::Config, state_sender: Sender<Any>) -> Self {
        AlsaPlayback{
            audio_id: Arc::new(Mutex::new(String::from("None"))),
            audio_dir: Arc::new(Mutex::new(String::from("None"))),
            audio_count: Arc::new(AtomicU32::new(0)),
            channel: Arc::new(AtomicBool::new(false)), //mono
            playback: Arc::new((Mutex::new(PlayBack::Stopped), Condvar::new())),
            playback_queue: Arc::new(Mutex::new(None)),
            elapsed: Arc::new(Mutex::new(Some(prost_types::Duration{
                seconds:0, nanos:0
            }))),
            state_sender,
            import_switch: Arc::new((Mutex::new(true), Condvar::new())),
            shutdown: None,
        }
    }

    async fn init(&mut self, config: Self::Config) {
        let audio_id = self.audio_id.clone();
        let playback = self.playback.clone();
        let elapsed = self.elapsed.clone();
        let sender = self.state_sender.clone();
        let queue = self.playback_queue.clone();
        let import_switch2 = self.import_switch.clone();
        let (sd_tx, sd_rx) = std_mpsc::channel();
        let num_channel = &*self.channel;
        let channels = match config.channel {
            1 => {false}
            2 => {true}
            _ => {
                tracing::error!("Invalid number of audio channels specified in config");
                false
            }
        };
        num_channel.store(channels, Ordering::Relaxed);

        //playback thread
        let handle = thread::spawn( move|| {
            let pcm = PCM::new(&*config.audio_device, Direction::Playback, false)
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();

            let hwp = HwParams::any(&pcm)
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            hwp.set_channels(config.channel)
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            hwp.set_rate(config.sample_rate, ValueOr::Nearest)
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            hwp.set_access(Access::RWInterleaved)
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            hwp.set_format(Format::s16())
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            pcm.hw_params(&hwp)
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();

            let swp = pcm.sw_params_current()
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            swp.set_start_threshold(hwp.get_buffer_size().unwrap())
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            pcm.sw_params(&swp)
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            let io = pcm.io_i16()
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            tracing::debug!("Completed audio hardware setup");

            'stim: loop {
                //shutdown
                if sd_rx.try_recv().unwrap_err() == std_mpsc::TryRecvError::Disconnected {
                    pcm.drop().map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                    break};

                //block & await completion of import
                let (imp_lock, imp_cnd) = &*import_switch2;
                //as long as the value within imp_lock is true, block the thread and wait
                let _import_guard = imp_cnd.wait_while(imp_lock.lock().unwrap(),
                                                       |importing| {*importing}).unwrap();

                //playback loop
                let stim = audio_id.lock().unwrap();
                let (pb_lock, pb_cnd) = &*playback;

                //block and wait until state change resumes playing
                let _pb_guard = pb_cnd.wait_while(pb_lock.lock().unwrap(),
                |playing| *playing != PlayBack::Playing).unwrap();
                //we allow it to pass only if playback is set to Playing, instead of
                //waiting while playing is Stopped, to prevent a Next from passing through.

                tracing::debug!("Playing");
                if stim.ends_with("wav")  {
                    let queue_lock = queue.lock().unwrap();
                    let stim_queue = queue_lock.as_ref()
                        .expect("Empty stim queue.");
                    let stim_name = OsString::from(stim.clone());
                    let mut elapsed = elapsed.lock().unwrap();

                    pcm.hw_params(&hwp)
                        .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                    let data = stim_queue.get(&*stim_name).unwrap();

                    io.writei(data).map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                    tracing::debug!("Begin playback");
                    let timer = std::time::Instant::now();

                    //loop while playing
                    'playback: while pcm.state() == State::Running {
                        //shutdown check
                        if sd_rx.try_recv().unwrap_err() == std_mpsc::TryRecvError::Disconnected {
                            pcm.drop().map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                            break 'stim}
                        //check for client half-way interruption:
                        let p = pb_lock.lock().unwrap().clone();
                        match p {
                            PlayBack::Next => { //interrupted and skipped
                                pcm.drop()
                                    .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                                let duration = prost_types::Duration::try_from(timer.elapsed())
                                    .map_err(|e| println!("Could not convert protobuf duration to std duration")).unwrap();
                                *elapsed = Some(duration.clone());

                                //Send info about interrupted stim
                                let state = Self::State {
                                    audio_id: stim_name.clone().into_string().unwrap(),
                                    playback: PlayBack::Stopped as i32,
                                    elapsed: Some(duration)
                                };
                                let message = Any {
                                    value: state.encode_to_vec(),
                                    type_url: Self::STATE_TYPE_URL.into(),
                                };
                                block_on(sender.send(message))
                                    .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                                tracing::debug!("Preparing to play next stim, elapsed time recorded");

                                let mut playback = pb_lock.lock().unwrap();
                                *playback = PlayBack::Playing;

                                continue 'stim}

                            PlayBack::Stopped => { //interrupted

                                pcm.drop()
                                    .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                                let duration = prost_types::Duration::try_from(timer.elapsed())
                                    .map_err(|e| println!("Could not convert protobuf duration to std duration")).unwrap();
                                *elapsed = Some(duration.clone());

                                //Send info about interrupted stim
                                let state = Self::State {
                                    audio_id: stim_name.clone().into_string().unwrap(),
                                    playback: PlayBack::Stopped as i32,
                                    elapsed: Some(duration)
                                };
                                let message = Any {
                                    value: state.encode_to_vec(),
                                    type_url: Self::STATE_TYPE_URL.into(),
                                };
                                block_on(sender.send(message))
                                    .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                                tracing::debug!("Preparing to play next stim, elapsed time recorded");

                                //not sure if this would be necessary:
                                //let mut playback = pb_lock.lock().unwrap();
                                //*playback = PlayBack::Stopped;

                                continue 'stim}

                            PlayBack::Playing => {continue 'playback}
                        };
                    }
                    //playback finished without interruption:
                    let duration = prost_types::Duration::try_from(timer.elapsed())
                        .map_err(|e| println!("Could not convert protobuf duration to std duration")).unwrap();
                    *elapsed = Some(duration.clone());
                    //Send info about completed stim
                    let state = Self::State {
                        audio_id: stim_name.clone().into_string().unwrap(),
                        playback: PlayBack::Stopped as i32,
                        elapsed: Some(duration)
                    };
                    let message = Any {
                        value: state.encode_to_vec(),
                        type_url: Self::STATE_TYPE_URL.into(),
                    };
                    block_on(sender.send(message))
                        .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                    tracing::debug!("Completed stim without interruption");
                    let mut playback = pb_lock.lock().unwrap();
                    *playback = PlayBack::Stopped;

                } else {
                    tracing::error!("Playback set to Playing but filename is invalid");
                }
            }
        });

        self.shutdown = Some((handle, sd_tx));
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        let (pb_lock, pb_cnd) = &*self.playback;
        let current_pb = pb_lock.lock().unwrap().clone();
        let mut audio_id = self.audio_id.lock().unwrap();
        //can't change 'elapsed' with change_state()?

        //compare sent playback control signal against current control
        match PlayBack::from_i32(state.playback).expect("Invalid value of received state")  {
            PlayBack::Playing => {
                match current_pb {
                    PlayBack::Playing => {tracing::error!("Requested stim while already playing. Send next or stop first.")}
                    PlayBack::Stopped => {
                        *audio_id = state.audio_id.clone();
                        let mut playback = pb_lock.lock().unwrap();
                        *playback = PlayBack::Playing;
                        pb_cnd.notify_one();
                        tracing::debug!("Notified playback thread of changing state to Playing");
                    }
                    PlayBack::Next => {
                        //tricky: playback thread currently transitioning between a "skip" received
                        // and the next stim playing, sending a play signal during this time is
                        //equivalent to sending a play signal during a stim playing. Considered invalid.
                        tracing::error!("Requested stim while already playing. Send next or stop first.")
                    }
                }
            }
            PlayBack::Stopped => {
                match current_pb {
                    PlayBack::Playing => {
                        let mut playback = pb_lock.lock().unwrap();
                        *playback = PlayBack::Stopped;
                        tracing::debug!("Playback requested interrupt.");
                    }
                    PlayBack::Stopped => {
                        tracing::info!("Playback requested to stop while already stopped.");
                    }
                    PlayBack::Next => {
                        //another tricky situation: the next signal could have been sent prior to
                        //pub change message of playback finishing, but arriving after the actual
                        //state variable change.
                        //allow this to stop while playback thread has not necessarily returned in time.
                        let mut playback = pb_lock.lock().unwrap();
                        *playback = PlayBack::Stopped;
                        tracing::info!("Playback requested to stop while in interrupt & skip.")
                    }
                }
            }
            PlayBack::Next => {
                match current_pb {
                    //interrupt and skip:
                    PlayBack::Playing => {
                        *audio_id = state.audio_id.clone();
                        let mut playback = pb_lock.lock().unwrap();
                        *playback = PlayBack::Next;
                    }
                    PlayBack::Stopped => {
                        *audio_id = state.audio_id.clone();
                        let mut playback = pb_lock.lock().unwrap();
                        *playback = PlayBack::Playing;
                        pb_cnd.notify_one();
                        tracing::info!("Playback requested to advance while stopped.")
                    }
                    PlayBack::Next => {
                        tracing::error!("Playback requested to interrupt & skip while in interrupt & skip.")
                    }
                }
            }
        };
        //we do not send actual state change PUB messages in change_state(), since it's more important
        //that PUB msgs come from the playback thread

        /*let sender = self.state_sender.clone();
        tokio::spawn(async move {
            sender
                .send(Any {
                    type_url: String::from(Self::STATE_TYPE_URL),
                    value: state.encode_to_vec(),
                })
                .await
                .map_err(|e| DecideError::Component { source: e.into() })
                .unwrap();
            tracing::debug!("Sound_Alsa state changed");
        });*/
        Ok(())
    }

    fn set_parameters(&mut self, params: Self::Params) -> decide_protocol::Result<()> {

        //stop playback when params are changed:
        let (pb_lock, _pb_cvar) = &*self.playback;
        let mut playing = pb_lock.lock().unwrap();
        *playing = PlayBack::Stopped;

        //import
        let current_dir = self.audio_dir.lock().unwrap();
        let import_switch = self.import_switch.clone();
        let queue_lock = self.playback_queue.lock().unwrap();
        let queue = match queue_lock.deref() {
            Some(_T) => {self.playback_queue.clone()},
            None => {
                let mut queue_lock2 = self.playback_queue.lock().unwrap();
                *queue_lock2 = Option::from(HashMap::new());
                self.playback_queue.clone()
            }
        };

        if params.audio_dir != current_dir.as_ref() {
            let mut audio_dir = self.audio_dir.lock().unwrap();
            *audio_dir = params.audio_dir;
            import_audio(import_switch, queue, self.channel.clone(),
                         self.audio_dir.clone(),self.audio_count.clone())
        };
        tracing::debug!("Sound playback parameters changed");
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        let (pb_lock, _pb_cvar) = &*self.playback;
        Self::State {
            audio_id: self.audio_id.try_lock().unwrap().clone(),
            playback: *pb_lock.try_lock().unwrap() as i32,
            elapsed: self.elapsed.try_lock().unwrap().clone(),
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params {
            audio_dir: self.audio_dir.try_lock().unwrap().clone(),
            audio_count: self.audio_count.load(Ordering::Relaxed),
        }
    }

    async fn shutdown(&mut self) {
        if let Some((handle, sender)) = self.shutdown.take() {
            drop(sender);
            handle.join()
                //disable mapping here due to multiple errors
                //.map_err(|e| DecideError::Component { source: e.into() })
                .unwrap()
        }
    }
}

fn process_audio(mut wav: BufFileReader, wav_channels: u32, hw_channels: u32) -> Vec<i16>{
    let mut result = Vec::new();
    if wav_channels == 1 {
        result = wav.frames::<[i16;1]>()
            .map(Result::unwrap)
            .map(|file| audrey::dasp_frame::Frame::scale_amp(file, 0.8))
            .map(|note| note[0])
            .collect::<Vec<i16>>();
        if hw_channels == 2 {
            result = result.iter()
                .map(|note| [note, note])
                .flatten()
                .map(|f| *f )
                .collect::<Vec<_>>()
        }
    } else if wav_channels == 2 {
        result = wav.frames::<[i16;2]>()
            .map(Result::unwrap)
            .map(|file| audrey::dasp_frame::Frame::scale_amp(file, 0.8))
            .flatten()
            .collect::<Vec<i16>>();
        if hw_channels == 1 {
            result = result.iter()
                .enumerate()
                .filter(|f| f.0 % 2 == 0)
                .map(|f| *f.1)
                .collect::<Vec<_>>()
        }
    };
    result
}

fn import_audio(switch: Arc<(Mutex<bool>, Condvar)>,
                queue: Arc<Mutex<Option<HashMap<OsString, Vec<i16>>>>>,
                channels: Arc<AtomicBool>,
                audio_dir: Arc<Mutex<String>>,
                audio_count: Arc<AtomicU32>) {
    tokio::spawn(async move {
        let dir = audio_dir.lock().unwrap();
        let mut reader = read_dir(Path::new(&*dir))
            .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
        let mut entry= reader.next();
        //fails if provided dir is empty
        if entry.is_some() {
            //begin reading files
            while entry.is_some() {
                let file = entry
                    .unwrap()
                    .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                //path() returns the full path of the file
                let path = file.path();
                //strip the stored dir from full path
                let fname = OsString::from(path.clone().strip_prefix(&*dir)
                    .map_err(|e|DecideError::Component { source: e.into() })
                    .unwrap());

                let mut queue_lock = queue.lock()
                    .map_err(|_e| tracing::error!("Couldn't acquire lock on playlist"))
                    .unwrap();
                let stim_queue = queue_lock.as_mut()
                    .expect("None value found in stim queue hashmap");

                //avoid duplicates
                if !stim_queue.contains_key(&fname) {
                    //make sure file is an audio file with "wav" extension
                    if path.extension().unwrap() == "wav" {
                        let wav = audrey::open(path)
                            .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                        let wav_channels = wav.description().channel_count();
                        let hw_channels = match channels.load(Ordering::Acquire) {
                            true => {1}
                            false => {2}
                        };
                        let audio = process_audio(wav, wav_channels, hw_channels);
                        stim_queue.insert(fname.clone(),audio);
                        tracing::debug!("Importing file {:?}", fname);
                    }
                }
                entry = reader.next();
            }
        } else {tracing::info!("Current audio directory is empty!")}
        tracing::info!("Finished importing audio files");
        //add number of stims:
        let length = queue.lock().unwrap().as_ref()
            .expect("Non-existent queue right after import??")
            .keys().len() as u32;
        audio_count.store(length, Ordering::Relaxed);
        //ref line 117
        let (imp_lock, imp_cnd) = &*switch;
        let mut importing = imp_lock.lock().unwrap();
        *importing = false;
        imp_cnd.notify_one();
    });
}