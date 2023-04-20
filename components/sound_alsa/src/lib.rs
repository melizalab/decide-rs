use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::read_dir;
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use atomic_wait::{wait, wake_all};
use alsa::{Direction, pcm::{Access, Format, HwParams, PCM, State},
           ValueOr};
use async_trait::async_trait;
use audrey::read::BufFileReader;
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use tokio::{self, sync::mpsc::Sender};

use decide_protocol::{Component,
                      error::{DecideError}
};
use futures::executor::block_on;
use proto::sa_state::PlayBack;


pub struct AlsaPlayback {
    audio_dir: Arc<Mutex<String>>,
    audio_id: Arc<Mutex<String>>,
    audio_count: Arc<AtomicU32>,
    channels: bool,
    playback: Arc<AtomicU32>, //pause or resume
    elapsed: Arc<Mutex<Option<prost_types::Duration>>>,
    playback_queue: Arc<Mutex<HashMap<OsString, Vec<i16>>>>,
    state_sender: Sender<Any>,
    import_switch: Arc<AtomicU32>,
    shutdown: Option<(thread::JoinHandle<()>,std_mpsc::Sender<bool>)>,
}

#[async_trait]
impl Component for AlsaPlayback {
    type State = proto::SaState;
    type Params = proto::SaParams;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "sa_state";
    const PARAMS_TYPE_URL: &'static str = "sa_params";

    fn new(_config: Self::Config, state_sender: Sender<Any>) -> Self {

        AlsaPlayback{
            audio_id: Arc::new(Mutex::new(String::from("None"))),
            audio_dir: Arc::new(Mutex::new(String::from("None"))),
            audio_count: Arc::new(AtomicU32::new(0)),
            channels: false, //mono
            playback: Arc::new(AtomicU32::new(0)), //0: Stopped, 1: Playing, 2: Next
            playback_queue: Arc::new(Mutex::new(HashMap::new())),
            elapsed: Arc::new(Mutex::new(Some(prost_types::Duration{
                seconds:0, nanos:0
            }))),
            state_sender,
            import_switch: Arc::new(AtomicU32::new(1)),
            shutdown: None,
        }
    }

    async fn init(&mut self, config: Self::Config) {
        // Playback state message
        let audio_id = self.audio_id.clone();
        let playback = self.playback.clone();
        let elapsed = self.elapsed.clone();
        // Playback thread communication
        let sender = self.state_sender.clone();
        let queue = self.playback_queue.clone();
        let import_switch2 = self.import_switch.clone();
        let (sd_tx, sd_rx) = std_mpsc::channel();

        self.channels = match config.channels {
            1 => {false}
            2 => {true}
            _ => {
                tracing::error!("Invalid number of audio channels specified in config");
                false
            }
        };

        //playback thread
        let handle = thread::spawn( move|| {
            tracing::debug!("AlsaPlayback - Playback Thread created");

            let pcm = PCM::new(&config.audio_device.clone(), Direction::Playback, false)
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            tracing::debug!("AlsaPlayback - pcm device created on {:?}", config.audio_device.clone());

            'stim: loop {
                // Check shutdown
                if sd_rx.try_recv().unwrap_err() == std_mpsc::TryRecvError::Disconnected {
                    pcm.drop().map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                    break};

                // block & await completion of import - change to 0
                wait(&import_switch2, 1);

                // playback loop
                tracing::debug!("Sound_alsa - Awaiting playback start");
                // Block and await playback change to 1
                wait(&playback, 0);

                let stim = audio_id.lock().unwrap();
                tracing::debug!("Sound_alsa - Starting Playback of {:?}", stim.clone());
                if stim.ends_with("wav")  {

                    let stim_name = OsString::from(stim.clone());
                    tracing::debug!("Ping 1");
                    let io = get_io(&pcm, &config);
                    tracing::debug!("Ping 2");
                    let stim_queue = queue.lock().unwrap();
                    tracing::debug!("Ping 3");
                    let data = stim_queue.get(&*stim_name).unwrap();
                    tracing::debug!("Ping 4: {:?}", data.len());
                    tracing::debug!("Begin writei, state is {:?}", pcm.state());
                    let written = io.writei(data).map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                    tracing::debug!("After writei, state is {:?}", pcm.state());
                    tracing::debug!("After writei, write result is {:?}", written);
                    let timer = std::time::Instant::now();

                    //loop while playing
                    'playback: while pcm.state() == State::Running {
                        //shutdown check
                        if sd_rx.try_recv().unwrap_err() == std_mpsc::TryRecvError::Disconnected {
                            pcm.drop().map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                            break 'stim}
                        //check for client half-way interruption:
                        let pb = playback.load(Ordering::Acquire);
                        match pb {
                            2 => { // NEXT, interrupted and skipped
                                pcm.drop()
                                    .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                                let duration = prost_types::Duration::try_from(timer.elapsed())
                                    .map_err(|_e| println!("Could not convert protobuf duration to std duration")).unwrap();
                                let mut elapsed = elapsed.lock().unwrap();
                                *elapsed = Some(duration.clone());

                                //Send info about interrupted stim
                                let state = Self::State {
                                    audio_id: stim_name.clone().into_string().unwrap(),
                                    playback: pb as i32,
                                    elapsed: Some(duration)
                                };
                                AlsaPlayback::send_state(&sender, state);
                                tracing::debug!("Preparing to play next stim, elapsed time recorded");
                                playback.store(1, Ordering::Release);
                                continue 'stim}

                            0 => { // STOPPED - interrupted

                                pcm.drop()
                                    .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                                let duration = prost_types::Duration::try_from(timer.elapsed())
                                    .map_err(|_e| println!("Could not convert protobuf duration to std duration")).unwrap();
                                let mut elapsed = elapsed.lock().unwrap();
                                *elapsed = Some(duration.clone());

                                //Send info about interrupted stim
                                let state = Self::State {
                                    audio_id: stim_name.clone().into_string().unwrap(),
                                    playback: pb as i32,
                                    elapsed: Some(duration)
                                };
                                AlsaPlayback::send_state(&sender, state);
                                tracing::debug!("Preparing to play next stim, elapsed time recorded");

                                //not sure if this would be necessary:
                                //let mut playback = pb_lock.lock().unwrap();
                                //*playback = PlayBack::Stopped;

                                continue 'stim}

                            1 => {continue 'playback}

                            _ => {tracing::error!("Unacceptable value found in playback control variable {:?}", pb)}
                        };
                    }
                    //playback finished without interruption:
                    let duration = prost_types::Duration::try_from(timer.elapsed())
                        .map_err(|_e| println!("Could not convert protobuf duration to std duration")).unwrap();
                    let mut elapsed = elapsed.lock().unwrap();
                    *elapsed = Some(duration.clone());
                    //Send info about completed stim
                    let state = Self::State {
                        audio_id: stim_name.clone().into_string().unwrap(),
                        playback: PlayBack::Stopped as i32,
                        elapsed: Some(duration)
                    };
                    AlsaPlayback::send_state(&sender, state);
                    tracing::debug!("Completed stim without interruption");
                    playback.store(0, Ordering::Release);

                } else {
                    tracing::error!("Playback set to Playing but filename {:?} is invalid", stim);
                    playback.store(0, Ordering::Release);
                }
            }
        });

        self.shutdown = Some((handle, sd_tx));
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        let current_pb = self.playback.load(Ordering::Acquire) as i32;
        let mut audio_id = self.audio_id.lock().unwrap();
        //can't change 'elapsed' with change_state()?

        //compare sent playback control signal against current control
        match PlayBack::from_i32(state.playback).expect("Invalid value of received state")  {
            PlayBack::Playing => {
                match current_pb {
                    1 => {tracing::error!("Requested stim while already playing. Send next or stop first.")}
                    0 => {
                        *audio_id = state.audio_id;
                        self.playback.store(1, Ordering::Release);
                        let pb = self.playback.as_ref();
                        wake_all(pb);
                        tracing::debug!("Notified playback thread of changing state to Playing");
                    }
                    2 => {
                        // tricky: playback thread currently transitioning between a "skip" received
                        // and the next stim playing, sending a play signal during this time is
                        // equivalent to sending a play signal during a stim playing. Considered invalid.
                        tracing::error!("Requested stim while already playing. Send next or stop first.")
                    }
                    _ => {tracing::error!("Invalid Playback value detected {:?}", current_pb)}
                }
            }
            PlayBack::Stopped => {
                match current_pb {
                    1 => {
                        self.playback.store(0, Ordering::Release);
                        tracing::debug!("Playback requested interrupt.");
                    }
                    0 => {
                        tracing::info!("Playback requested to stop while already stopped.");
                    }
                    2 => {
                        // another tricky situation: the next signal could have been sent prior to
                        // pub change message of playback finishing, but arriving after the actual
                        // state variable change.
                        // allow this to stop while playback thread has not necessarily returned in time.
                        self.playback.store(0, Ordering::Release);
                        tracing::info!("Playback requested to stop while in interrupt & skip.")
                    }
                    _ => {tracing::error!("Invalid Playback value detected {:?}", current_pb)}
                }
            }
            PlayBack::Next => {
                match current_pb {
                    //interrupt and skip:
                    1 => {
                        *audio_id = state.audio_id.clone();
                        self.playback.store(2, Ordering::Release);
                        let pb = self.playback.as_ref();
                        wake_all(pb);
                    }
                    0 => {
                        *audio_id = state.audio_id.clone();
                        self.playback.store(1, Ordering::Release);
                        let pb = self.playback.as_ref();
                        wake_all(pb);
                        tracing::info!("Playback requested to advance while stopped.")
                    }
                    2 => {
                        tracing::error!("Playback requested to interrupt & skip while in interrupt & skip.")
                    }
                    _ => {tracing::error!("Invalid Playback value detected {:?}", current_pb)}

                }
            }
        };
        //we do not send actual state change PUB messages in change_state(), since it's more important
        //that PUB msgs come from the playback thread
        Ok(())
    }

    fn set_parameters(&mut self, params: Self::Params) -> decide_protocol::Result<()> {
        tracing::debug!("Setting parameters of AlsaPlayback");
        //stop playback & initiate import when params are changed:
        self.playback.store(0, Ordering::Release);
        self.import_switch.store(1, Ordering::Release);

        //import
        let current_dir: String = self.audio_dir.lock().unwrap().drain(..).collect();
        let import_switch = self.import_switch.clone();
        let queue = self.playback_queue.clone();

        tracing::debug!("Checking current playback dir to match requested {:?}", current_dir.clone());
        tracing::debug!("Requested dir change to {:?}", params.audio_dir);

        if params.audio_dir != current_dir {
            tracing::debug!("ping 1");
            let mut audio_dir = self.audio_dir.lock().unwrap();
            *audio_dir = params.audio_dir;
            tracing::debug!("Init import audio");
            import_audio(import_switch, queue, self.channels,
                         self.audio_dir.clone(),self.audio_count.clone())
        };
        tracing::debug!("Sound playback parameters changed");
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State {
            audio_id: self.audio_id.try_lock().unwrap().clone(),
            playback: self.playback.load(Ordering::Acquire) as i32,
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

impl AlsaPlayback{
    fn send_state(sender: &Sender<Any>, state: proto::SaState) {
        let message = Any {
            value: state.encode_to_vec(),
            type_url: Self::STATE_TYPE_URL.into(),
        };
        block_on(sender.send(message))
            .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
        tracing::debug!("AlsaPlayback - state sent");
    }
}


fn get_io<'a>(pcm: &'a PCM, config: &'a Config) -> alsa::pcm::IO<'a, i16> {

    let hwp = HwParams::any(&pcm)
        .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
    hwp.set_channels(config.channels)
        .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
    hwp.set_rate(config.sample_rate, ValueOr::Nearest)
        .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
    hwp.set_access(Access::RWInterleaved)
        .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
    hwp.set_format(Format::s16())
        .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
    pcm.hw_params(&hwp)
        .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
    let io = pcm.io_i16()
        .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
    tracing::debug!("Completed audio hardware setup");
    return io
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

fn import_audio(switch: Arc<AtomicU32>,
                queue: Arc<Mutex<HashMap<OsString, Vec<i16>>>>,
                channels: bool,
                audio_dir: Arc<Mutex<String>>,
                audio_count: Arc<AtomicU32>) {
    tokio::spawn(async move {
        tracing::debug!("Creating task to import audio");
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

                let mut stim_queue = queue.lock()
                    .map_err(|_e| tracing::error!("Couldn't acquire lock on playlist"))
                    .unwrap();

                //avoid duplicates
                if !stim_queue.contains_key(&fname) {
                    //make sure file is an audio file with "wav" extension
                    if path.extension().unwrap() == "wav" {
                        let wav = audrey::open(path)
                            .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                        let wav_channels = wav.description().channel_count();
                        let hw_channels = match channels {
                            true => {2}
                            false => {1}
                        };
                        tracing::debug!("Importing file {:?}", fname);
                        let audio = process_audio(wav, wav_channels, hw_channels);
                        stim_queue.insert(fname.clone(),audio);
                    }
                }
                entry = reader.next();
            }
        } else {tracing::info!("Current audio directory is empty!")}
        tracing::info!("Finished importing audio files");
        //add number of stims:
        let length = queue.lock().unwrap()
            .keys().len() as u32;
        audio_count.store(length, Ordering::Relaxed);
        //ref line 117
        switch.store(0,Ordering::Release);
        let sw = switch.as_ref();
        wake_all(sw);
    });
}

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}
#[derive(Deserialize)]
pub struct Config {
    audio_device: String,
    sample_rate: u32,
    channels: u32,
}