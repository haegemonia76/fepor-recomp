//! Audio output for the recompiled game: an SDL3 playback stream fed from the AI DMA buffers, and an
//! optional WAV dump (`FEPOR_DUMP_AUDIO=file.wav`) for headless checks.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

use fepor_core::settings::SettingsHandle;
use fepor_runtime::dsp::AudioSink;

/// Streaming WAV writer: the header is rewritten after every push so a killed process leaves a valid file.
struct WavSink {
    file: File,
    rate: u32,
    data_bytes: u32,
}

impl WavSink {
    fn create(path: &str) -> std::io::Result<WavSink> {
        let mut w = WavSink {
            file: File::create(path)?,
            rate: 0,
            data_bytes: 0,
        };
        w.header(32_000)?;
        Ok(w)
    }

    fn header(&mut self, rate: u32) -> std::io::Result<()> {
        self.rate = rate;
        self.file.seek(SeekFrom::Start(0))?;
        let mut h = Vec::with_capacity(44);
        h.extend_from_slice(b"RIFF");
        h.extend_from_slice(&(36 + self.data_bytes).to_le_bytes());
        h.extend_from_slice(b"WAVEfmt ");
        h.extend_from_slice(&16u32.to_le_bytes());
        h.extend_from_slice(&1u16.to_le_bytes());
        h.extend_from_slice(&2u16.to_le_bytes());
        h.extend_from_slice(&rate.to_le_bytes());
        h.extend_from_slice(&(rate * 4).to_le_bytes());
        h.extend_from_slice(&4u16.to_le_bytes());
        h.extend_from_slice(&16u16.to_le_bytes());
        h.extend_from_slice(b"data");
        h.extend_from_slice(&self.data_bytes.to_le_bytes());
        self.file.write_all(&h)?;
        self.file.seek(SeekFrom::End(0))?;
        Ok(())
    }
}

impl AudioSink for WavSink {
    fn push(&mut self, rate: u32, samples: &[i16]) {
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let _ = self.file.write_all(&bytes);
        self.data_bytes += bytes.len() as u32;
        let _ = self.header(rate);
    }
}

/// Plays through SDL3. The SDL objects live on their own thread (they are not `Send`).
struct SdlSink {
    tx: Sender<(u32, Vec<i16>)>,
    settings: SettingsHandle,
}

impl AudioSink for SdlSink {
    fn push(&mut self, rate: u32, samples: &[i16]) {
        // the volume slider is perceptual: the gain is its square
        let v = self.settings.get().audio.volume;
        let gain = v * v;
        let out = if gain >= 0.999 {
            samples.to_vec()
        } else {
            samples.iter().map(|s| (*s as f32 * gain) as i16).collect()
        };
        let _ = self.tx.send((rate, out));
    }
}

fn sdl_thread(
    rx: std::sync::mpsc::Receiver<(u32, Vec<i16>)>,
    ready: Sender<Result<(), String>>,
    settings: SettingsHandle,
    devices: Arc<Mutex<Vec<String>>>,
) {
    use sdl3::audio::{AudioDevice, AudioFormat, AudioSpec};
    let setup = || -> Result<_, String> {
        let sdl = sdl3::init().map_err(|e| e.to_string())?;
        let audio = sdl.audio().map_err(|e| e.to_string())?;
        Ok((sdl, audio))
    };
    let (sdl, audio) = match setup() {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut rate = 0u32;
    let trace = std::env::var("FEPOR_TRACE").is_ok_and(|v| v.split(',').any(|x| x == "audio"));
    let (mut underruns, mut drops, mut last) = (0u32, 0u32, std::time::Instant::now());
    let (mut qmin, mut qmax) = (i32::MAX, 0i32);
    let _ = ready.send(Ok(()));
    let mut stream = None;
    let (mut device, mut seen_gen) = (None::<String>, 0u64);
    let mut listed = None::<std::time::Instant>;
    while let Ok((r, samples)) = rx.recv() {
        if listed.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(2)) {
            listed = Some(std::time::Instant::now());
            let names: Vec<String> = audio
                .audio_playback_device_ids()
                .map(|ids| ids.iter().filter_map(|id| id.name().ok()).collect())
                .unwrap_or_default();
            if let Ok(mut d) = devices.lock() {
                if *d != names {
                    *d = names;
                }
            }
        }
        if settings.generation() != seen_gen {
            seen_gen = settings.generation();
            let want = settings.get().audio.device;
            if want != device {
                device = want;
                stream = None;
            }
        }
        if r != rate || stream.is_none() {
            rate = r;
            let spec = AudioSpec {
                freq: Some(r as i32),
                channels: Some(2),
                format: Some(AudioFormat::s16_sys()),
            };
            let chosen = device.as_ref().and_then(|name| {
                audio
                    .audio_playback_device_ids()
                    .ok()?
                    .into_iter()
                    .find(|id| id.name().ok().as_ref() == Some(name))
            });
            // a device that is gone (unplugged) falls back to the system default
            let dev = match chosen {
                Some(id) => AudioDevice::new(id, audio.clone()),
                None => audio.default_playback_device(),
            };
            stream = dev.open_device_stream(Some(&spec)).ok();
            if let Some(s) = &stream {
                let _ = s.resume();
            }
        }
        if let Some(s) = &stream {
            let queued = s.queued_bytes().unwrap_or(0);
            qmin = qmin.min(queued);
            qmax = qmax.max(queued);
            if trace && last.elapsed() >= std::time::Duration::from_secs(5) {
                let ms = |b: i32| b as i64 * 1000 / (rate as i64 * 4);
                eprintln!(
                    "audio: queue {}..{} ms, {underruns} underruns, {drops} drops in 5 s",
                    ms(qmin),
                    ms(qmax)
                );
                (qmin, qmax, underruns, drops, last) =
                    (i32::MAX, 0, 0, 0, std::time::Instant::now());
            }
            if queued == 0 {
                underruns += 1;
                // start or underrun: refill a 60 ms cushion so that late frames from the guest do not crackle
                let silence = vec![0i16; (rate as usize * 60 / 1000) * 2];
                let _ = s.put_data_i16(&silence);
            }
            // keep the queue short: drop audio when the device falls behind (about 200 ms)
            if queued >= (rate as i32 * 4 / 5) {
                drops += 1;
            } else {
                let _ = s.put_data_i16(&samples);
            }
        }
    }
    drop(sdl);
}

/// Feeds the video recorder (ADR 0044); does nothing while no recording runs.
struct CaptureSink(Arc<fepor_core::capture::Capture>);

impl AudioSink for CaptureSink {
    fn push(&mut self, rate: u32, samples: &[i16]) {
        self.0.push_audio(rate, samples);
    }
}

/// Duplicates audio to several sinks.
struct Tee(Vec<Box<dyn AudioSink>>);

impl AudioSink for Tee {
    fn push(&mut self, rate: u32, samples: &[i16]) {
        for s in &mut self.0 {
            s.push(rate, samples);
        }
    }
}

/// The audio output selected by the environment: SDL3 unless `FEPOR_NO_AUDIO` is set (played at the volume
/// of `settings`), plus a WAV dump when `FEPOR_DUMP_AUDIO` names a file (always at full volume) and the tap
/// of the video recorder `capture`.
pub fn open(
    settings: SettingsHandle,
    devices: Arc<Mutex<Vec<String>>>,
    capture: Arc<fepor_core::capture::Capture>,
) -> Option<Box<dyn AudioSink>> {
    let thread_settings = settings.clone();
    // the recorder gets the audio at full volume, as the WAV dump does
    let mut sinks: Vec<Box<dyn AudioSink>> = vec![Box::new(CaptureSink(capture))];
    if let Ok(path) = std::env::var("FEPOR_DUMP_AUDIO") {
        match WavSink::create(&path) {
            Ok(w) => sinks.push(Box::new(w)),
            Err(e) => eprintln!("audio: cannot write {path}: {e}"),
        }
    }
    if std::env::var_os("FEPOR_NO_AUDIO").is_none() {
        let (tx, rx) = channel();
        let (rtx, rrx) = channel();
        std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || sdl_thread(rx, rtx, thread_settings, devices))
            .ok()?;
        match rrx.recv() {
            Ok(Ok(())) => sinks.push(Box::new(SdlSink { tx, settings })),
            Ok(Err(e)) => eprintln!("audio disabled: {e}"),
            Err(_) => {}
        }
    }
    match sinks.len() {
        1 => sinks.pop(),
        _ => Some(Box::new(Tee(sinks))),
    }
}
