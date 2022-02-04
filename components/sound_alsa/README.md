AlsaPlayback COMPONENT
=======================
This audio playback component relies on the [alsa-rs crate](https://github.com/diwic/alsa-rs), which wraps the [ALSA high level commands](https://www.alsa-project.org/alsa-doc/alsa-lib/group___h_control.html).

At the time of writing, Rust already has multiple libraries dedicated to audio streaming, notably [cpal](https://github.com/RustAudio/cpal) and [rodio](https://github.com/RustAudio/rodio) which also utilize the ALSA API. However, starting a PCM stream with cpal on a BeagleBone will produe a [timestamp error](https://github.com/RustAudio/cpal/issues/604); cpal does not provide the ALSA-level wrappers necessary to fix this.

Drawbacks and oddities of alsa-rs includes:
1. Not being thread-safe and not having `send` implemented for its `pcm` struct. We put async components such as `send()` and `recv()` methods in a `block_on()` when calling them inside the thread that the `pcm` runs in.
2. A `HwParams` struct must be created from the `pcm` created, then updated and pushed back to said `pcm`. However, any modifications to `HwParams` that does not fit the hardware capabilities will produce an error. There doesn't seem to be a method for listing compatible parameters for a given stream; trial and error recommended. For the USB speaker used in MelizaLab's setup:
```
sample rate: 44100Hz
channels: 2 (stereo)
format: s16
io: io_u8 (set after HwParams is imported to pcm)
```
3. The `pcm`'s state (not to be confused with Status) will change to Prepared upon re-importing HwParams, signifying that it's ready to stream audio. Upon finishing a stream with `io.writei()`, however, the state will reset to `Setup` and require the HwParams to be re-imported again.

WAV files are read using `audrey::read()`, which requires knowing beforehand how many channels the WAV file is encoded in. The `process_audio()` function handles converting the read audio file (stored as a vector of mono or duo frames) by calling `flatten()` on these frames so that `io.writei()` can read them.

All audio files are found and read upon initialization, each stored as a Vector<u8> in a Hashmap along with the filename.