//! Screenshots and video recording (ADR 0025).
//!
//! [`Capture`] is the meeting point of four threads: the window thread and the overlay ask (hotkeys, buttons),
//! the render thread hands over the picture, the guest thread hands over the audio, and a writer thread feeds
//! an external `ffmpeg`. The renderer decides what the picture is (`capture.overlay` in the settings, the
//! internal resolution) and writes screenshots itself; this module owns the file names, the recording and
//! the clock that keeps picture and sound together.
//!
//! Recording: frames go raw to an `ffmpeg` pipe (H.264, or MPEG-4 when ffmpeg has no H.264 encoder, in a Matroska part file), the audio to a WAV part
//! file, and when the recording stops the two are muxed (video copied, audio AAC) into `<data>/videos/*.mp4`.
//! Both are timed by the wall clock of the game thread, whose audio is paced by the same clock (the AI DMA
//! timer): the picture is made constant-rate by repeating each frame until the next one arrives (the game
//! presents 30 or 60 frames a second), and a gap in the audio (the guest was late) is filled with silence.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Frame rate of the recorded video (the NTSC video clock the game runs at).
const FPS_NUM: u64 = 60_000;
const FPS_DEN: u64 = 1_001;
/// A hole in the audio longer than this is filled with silence (shorter ones are jitter of the pacing timer).
const GAP: Duration = Duration::from_millis(100);
/// How long a message stays on screen.
const TOAST: Duration = Duration::from_secs(4);

/// What `FEPOR_CAPTURE` can script, at a game frame number.
enum Step {
    Screenshot,
    Start,
    Stop,
}

pub struct Capture {
    /// The data folder: `screenshots` and `videos` live in it.
    data: PathBuf,
    shot: AtomicBool,
    want_record: AtomicBool,
    recording: AtomicBool,
    rec: Mutex<Option<Recording>>,
    /// Muxing of the last recording, so that `finish` can wait for it.
    muxing: Mutex<Option<JoinHandle<()>>>,
    message: Arc<Mutex<Option<(String, Instant)>>>,
    /// `FEPOR_CAPTURE=shot@300,record@100-400`: screenshot at game frame 300, recording from 100 to 400.
    script: Mutex<Vec<(u64, Step)>>,
}

impl Capture {
    pub fn new(data: PathBuf) -> Capture {
        let mut script = Vec::new();
        for item in std::env::var("FEPOR_CAPTURE").unwrap_or_default().split(',') {
            let Some((what, at)) = item.split_once('@') else {
                continue;
            };
            let num = |s: &str| s.parse::<u64>().ok();
            match what {
                "shot" => script.extend(num(at).map(|n| (n, Step::Screenshot))),
                "record" => {
                    if let Some((a, b)) = at.split_once('-').and_then(|(a, b)| Some((num(a)?, num(b)?))) {
                        script.push((a, Step::Start));
                        script.push((b, Step::Stop));
                    }
                }
                _ => eprintln!("capture: FEPOR_CAPTURE: unknown item {item:?} (shot@N, record@N-M)"),
            }
        }
        Capture {
            data,
            shot: AtomicBool::new(false),
            want_record: AtomicBool::new(false),
            recording: AtomicBool::new(false),
            rec: Mutex::new(None),
            muxing: Mutex::new(None),
            message: Arc::default(),
            script: Mutex::new(script),
        }
    }

    pub fn request_screenshot(&self) {
        self.shot.store(true, Ordering::Release);
    }

    /// Start recording, or stop it when it runs. The renderer acts on it at the next frame.
    pub fn toggle_recording(&self) {
        self.want_record.fetch_xor(true, Ordering::AcqRel);
    }

    /// Whether a recording is running (the encoder is open).
    pub fn is_recording(&self) -> bool {
        self.recording.load(Ordering::Acquire)
    }

    /// Seconds since the running recording started.
    pub fn recording_secs(&self) -> Option<u64> {
        self.rec
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|r| r.start.elapsed().as_secs())
    }

    /// The last message (file saved, error), while it is recent enough to show.
    pub fn toast(&self) -> Option<String> {
        let m = self.message.lock().unwrap_or_else(|e| e.into_inner());
        m.as_ref().filter(|(_, at)| at.elapsed() < TOAST).map(|(t, _)| t.clone())
    }

    fn say(message: &Mutex<Option<(String, Instant)>>, text: String) {
        eprintln!("capture: {text}");
        *message.lock().unwrap_or_else(|e| e.into_inner()) = Some((text, Instant::now()));
    }

    /// Called by the renderer for every presented game frame: runs the `FEPOR_CAPTURE` script.
    pub fn on_frame(&self, frame: u64) {
        let mut script = self.script.lock().unwrap_or_else(|e| e.into_inner());
        if script.is_empty() {
            return;
        }
        script.retain(|(at, step)| {
            if *at > frame {
                return true;
            }
            match step {
                Step::Screenshot => self.request_screenshot(),
                Step::Start => self.want_record.store(true, Ordering::Release),
                Step::Stop => self.want_record.store(false, Ordering::Release),
            }
            false
        });
    }

    /// The path of the screenshot to write now, when one was asked for.
    pub fn take_screenshot(&self) -> Option<PathBuf> {
        if !self.shot.swap(false, Ordering::AcqRel) {
            return None;
        }
        match next_path(&self.data.join("screenshots"), "screenshot", "png") {
            Ok(p) => Some(p),
            Err(e) => {
                Self::say(&self.message, format!("cannot save a screenshot: {e}"));
                None
            }
        }
    }

    /// Report a screenshot written (or not).
    pub fn screenshot_done(&self, path: &Path, result: Result<(), String>) {
        let text = match result {
            Ok(()) => format!("screenshot saved: {}", path.display()),
            Err(e) => format!("cannot save {}: {e}", path.display()),
        };
        Self::say(&self.message, text);
    }

    /// Bring the recording in line with what was asked for (starting it with pictures of `w` x `h`, or
    /// stopping it) and say whether the renderer should deliver this frame with [`Capture::frame`].
    pub fn sync_recording(&self, w: u32, h: u32) -> bool {
        let want = self.want_record.load(Ordering::Acquire);
        let mut rec = self.rec.lock().unwrap_or_else(|e| e.into_inner());
        match (want, rec.is_some()) {
            (true, false) => match Recording::start(&self.data, w & !1, h & !1) {
                Ok(r) => {
                    Self::say(&self.message, format!("recording to {}", r.out.display()));
                    *rec = Some(r);
                    self.recording.store(true, Ordering::Release);
                }
                Err(e) => {
                    self.want_record.store(false, Ordering::Release);
                    Self::say(&self.message, format!("cannot record: {e}"));
                }
            },
            (false, true) => {
                let r = rec.take().expect("recording");
                drop(rec);
                self.stop(r);
            }
            _ => {}
        }
        self.recording.load(Ordering::Acquire)
    }

    /// One presented picture (packed RGB, `w` x `h`) of the running recording.
    pub fn frame(&self, rgb: Vec<u8>, w: u32, h: u32) {
        let mut guard = self.rec.lock().unwrap_or_else(|e| e.into_inner());
        let Some(r) = guard.as_mut() else { return };
        if let Err(e) = r.frame(rgb, w, h) {
            let r = guard.take().expect("recording");
            drop(guard);
            self.want_record.store(false, Ordering::Release);
            eprintln!("capture: recording failed: {e}");
            self.stop(r);
        }
    }

    /// Audio the game plays: interleaved left/right samples at `rate`.
    pub fn push_audio(&self, rate: u32, samples: &[i16]) {
        if !self.is_recording() {
            return;
        }
        if let Some(r) = self.rec.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            r.audio(rate, samples);
        }
    }

    /// Stop a running recording and wait until its file is complete (the process is about to exit).
    pub fn finish(&self) {
        self.want_record.store(false, Ordering::Release);
        let r = self.rec.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(r) = r {
            self.stop(r);
        }
        let h = self.muxing.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(h) = h {
            let _ = h.join();
        }
    }

    /// Close `r` and finish its file on a thread of its own: the guest does not wait for the encoder.
    fn stop(&self, r: Recording) {
        self.recording.store(false, Ordering::Release);
        let message = self.message.clone();
        let spawned = std::thread::Builder::new()
            .name("capture".into())
            .spawn(move || {
                let out = r.out.clone();
                match r.finish() {
                    Ok(()) => Self::say(&message, format!("video saved: {}", out.display())),
                    Err(e) => Self::say(&message, format!("video not saved: {e}")),
                }
            });
        let mut slot = self.muxing.lock().unwrap_or_else(|e| e.into_inner());
        // a recording started right after another one: the earlier muxing must finish first
        if let Some(old) = slot.take() {
            let _ = old.join();
        }
        *slot = spawned.ok();
    }
}

/// `<dir>/<prefix>_NNNN.<ext>` with the first number not used yet.
fn next_path(dir: &Path, prefix: &str, ext: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    (1..)
        .map(|n| dir.join(format!("{prefix}_{n:04}.{ext}")))
        .find(|p| !p.exists())
        .ok_or_else(|| std::io::Error::other("no free file name"))
}

/// The encoder: `FEPOR_FFMPEG`, an `ffmpeg` next to the executable, or the one on the `PATH`.
fn ffmpeg() -> PathBuf {
    if let Some(p) = std::env::var_os("FEPOR_FFMPEG") {
        return p.into();
    }
    let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    std::env::current_exe()
        .ok()
        .and_then(|e| Some(e.parent()?.join(name)))
        .filter(|p| p.exists())
        .unwrap_or_else(|| name.into())
}

/// The video encoder to use and its quality options: x264 when this ffmpeg has it, else OpenH264, else MPEG-4
/// part 2 (every build has it). Many distributions ship ffmpeg without x264.
fn video_codec(ffmpeg: &Path) -> (&'static str, &'static [&'static str]) {
    let list = Command::new(ffmpeg)
        .args(["-hide_banner", "-v", "error", "-encoders"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let has = |name: &str| list.lines().any(|l| l.split_whitespace().nth(1) == Some(name));
    if has("libx264") {
        ("libx264", &["-preset", "veryfast", "-crf", "18"])
    } else if has("libopenh264") {
        ("libopenh264", &["-b:v", "20M"])
    } else {
        ("mpeg4", &["-q:v", "2"])
    }
}

const NO_FFMPEG: &str = "no video encoder: ffmpeg was not found (install it, put it next to the game, or set FEPOR_FFMPEG to its path)";

/// Streaming 16-bit stereo WAV file.
struct Wav {
    file: BufWriter<File>,
    rate: u32,
    frames: u64,
}

impl Wav {
    fn create(path: &Path, rate: u32) -> std::io::Result<Wav> {
        let mut w = Wav {
            file: BufWriter::new(File::create(path)?),
            rate,
            frames: 0,
        };
        w.header()?;
        Ok(w)
    }

    fn header(&mut self) -> std::io::Result<()> {
        let data = (self.frames * 4).min(u32::MAX as u64 - 36) as u32;
        let mut h = Vec::with_capacity(44);
        h.extend_from_slice(b"RIFF");
        h.extend_from_slice(&(36 + data).to_le_bytes());
        h.extend_from_slice(b"WAVEfmt ");
        h.extend_from_slice(&16u32.to_le_bytes());
        h.extend_from_slice(&1u16.to_le_bytes());
        h.extend_from_slice(&2u16.to_le_bytes());
        h.extend_from_slice(&self.rate.to_le_bytes());
        h.extend_from_slice(&(self.rate * 4).to_le_bytes());
        h.extend_from_slice(&4u16.to_le_bytes());
        h.extend_from_slice(&16u16.to_le_bytes());
        h.extend_from_slice(b"data");
        h.extend_from_slice(&data.to_le_bytes());
        self.file.write_all(&h)
    }

    fn write(&mut self, samples: &[i16]) {
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let _ = self.file.write_all(&bytes);
        self.frames += samples.len() as u64 / 2;
    }

    fn silence(&mut self, frames: u64) {
        let _ = std::io::copy(&mut std::io::repeat(0).take(frames * 4), &mut self.file);
        self.frames += frames;
    }

    /// Write the final size into the header.
    fn close(mut self) -> std::io::Result<()> {
        self.file.flush()?;
        self.file.seek(SeekFrom::Start(0))?;
        self.header()?;
        self.file.flush()
    }
}

type Frames = SyncSender<(Arc<Vec<u8>>, u64)>;

struct Recording {
    start: Instant,
    w: u32,
    h: u32,
    out: PathBuf,
    part_video: PathBuf,
    part_audio: PathBuf,
    log: PathBuf,
    /// Pictures to the writer thread, each with the number of frame slots it fills.
    tx: Option<Frames>,
    writer: Option<JoinHandle<()>>,
    /// The picture on screen now and the first frame slot not written yet.
    last: Option<Arc<Vec<u8>>>,
    slot: u64,
    wav: Wav,
    /// The audio rate of the file: the first one pushed.
    rate: Option<u32>,
}

/// The first frame slot that begins at or after `t`.
fn slots_until(t: Duration) -> u64 {
    (t.as_nanos() * FPS_NUM as u128).div_ceil(FPS_DEN as u128 * 1_000_000_000) as u64
}

/// `rgb` (`sw` x `sh`) at `dw` x `dh`, nearest neighbour; the picture itself when the size is right.
fn fit(rgb: Vec<u8>, sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    if (sw, sh) == (dw, dh) {
        return rgb;
    }
    let mut out = Vec::with_capacity((dw * dh * 3) as usize);
    for y in 0..dh {
        let sy = (y as u64 * sh as u64 / dh as u64) as usize;
        for x in 0..dw {
            let sx = (x as u64 * sw as u64 / dw as u64) as usize;
            let i = (sy * sw as usize + sx) * 3;
            out.extend_from_slice(&rgb[i..i + 3]);
        }
    }
    out
}

impl Recording {
    fn start(data: &Path, w: u32, h: u32) -> Result<Recording, String> {
        let dir = data.join("videos");
        let out = next_path(&dir, "video", "mp4").map_err(|e| format!("{}: {e}", dir.display()))?;
        let part_video = out.with_extension("part.mkv");
        let part_audio = out.with_extension("part.wav");
        let log = out.with_extension("part.log");
        let ffmpeg = ffmpeg();
        let (codec, quality) = video_codec(&ffmpeg);
        let mut child = Command::new(&ffmpeg)
            .args(["-y", "-v", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-video_size"])
            .arg(format!("{w}x{h}"))
            .args(["-framerate", &format!("{FPS_NUM}/{FPS_DEN}"), "-i", "-"])
            .args(["-c:v", codec])
            .args(quality)
            .args(["-pix_fmt", "yuv420p", "-f", "matroska"])
            .arg(&part_video)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(File::create(&log).map_err(|e| format!("{}: {e}", log.display()))?)
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    NO_FFMPEG.to_string()
                } else {
                    format!("cannot run ffmpeg: {e}")
                }
            })?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let (tx, rx) = sync_channel::<(Arc<Vec<u8>>, u64)>(8);
        let writer = std::thread::Builder::new()
            .name("capture-video".into())
            .spawn(move || {
                for (frame, repeat) in rx {
                    for _ in 0..repeat {
                        if stdin.write_all(&frame).is_err() {
                            // ffmpeg died: the sender notices the closed channel and reports the log
                            return;
                        }
                    }
                }
                drop(stdin);
                let _ = child.wait();
            })
            .map_err(|e| e.to_string())?;
        let wav = Wav::create(&part_audio, 32_000).map_err(|e| format!("{}: {e}", part_audio.display()))?;
        Ok(Recording {
            start: Instant::now(),
            w,
            h,
            out,
            part_video,
            part_audio,
            log,
            tx: Some(tx),
            writer: Some(writer),
            last: None,
            slot: 0,
            wav,
            rate: None,
        })
    }

    fn send(&mut self, frame: Arc<Vec<u8>>, repeat: u64) -> Result<(), String> {
        if repeat == 0 {
            return Ok(());
        }
        self.slot += repeat;
        let tx = self.tx.as_ref().expect("open");
        tx.send((frame, repeat)).map_err(|_| self.encoder_error())
    }

    /// What ffmpeg said, when it stopped taking pictures.
    fn encoder_error(&self) -> String {
        let log = std::fs::read_to_string(&self.log).unwrap_or_default();
        let last = log.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("it exited");
        format!("ffmpeg stopped: {last}")
    }

    /// The picture shown from now on: the one before it fills the slots until now.
    fn frame(&mut self, rgb: Vec<u8>, w: u32, h: u32) -> Result<(), String> {
        let now = slots_until(self.start.elapsed());
        let new = Arc::new(fit(rgb, w, h, self.w, self.h));
        // the first picture also stands for the slots before it
        let shown = self.last.replace(new.clone()).unwrap_or(new);
        let repeat = now.saturating_sub(self.slot);
        self.send(shown, repeat)
    }

    fn audio(&mut self, rate: u32, samples: &[i16]) {
        let target = *self.rate.get_or_insert_with(|| {
            // the rate of the file is the first one pushed
            self.wav.rate = rate;
            rate
        });
        let resampled;
        let samples = if rate == target {
            samples
        } else {
            let frames = samples.len() / 2;
            let n = frames * target as usize / rate as usize;
            resampled = (0..n)
                .flat_map(|i| {
                    let s = (i * rate as usize / target as usize).min(frames - 1);
                    [samples[s * 2], samples[s * 2 + 1]]
                })
                .collect::<Vec<i16>>();
            &resampled
        };
        // the buffer ends now: it began as long before as it lasts
        let len = Duration::from_micros(samples.len() as u64 / 2 * 1_000_000 / target as u64);
        let begin = self.start.elapsed().saturating_sub(len);
        let at = Duration::from_micros(self.wav.frames * 1_000_000 / target as u64);
        if begin > at + GAP {
            self.wav.silence((begin - at).as_micros() as u64 * target as u64 / 1_000_000);
        }
        self.wav.write(samples);
    }

    /// Write what is left, wait for the encoder and mux the part files into the video.
    fn finish(mut self) -> Result<(), String> {
        let end = slots_until(self.start.elapsed());
        let result = self.finish_video(end);
        let parts = [self.part_video.clone(), self.part_audio.clone(), self.log.clone()];
        let result = result.and_then(|()| self.mux());
        for p in &parts {
            let _ = std::fs::remove_file(p);
        }
        result
    }

    fn finish_video(&mut self, end: u64) -> Result<(), String> {
        if let Some(last) = self.last.clone() {
            let repeat = end.saturating_sub(self.slot).max(1);
            self.send(last, repeat)?;
        } else {
            return Err("no picture was presented while recording".into());
        }
        self.tx = None;
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
        let log = std::fs::read_to_string(&self.log).unwrap_or_default();
        if log.trim().is_empty() {
            Ok(())
        } else {
            Err(format!("ffmpeg: {}", log.trim()))
        }
    }

    fn mux(self) -> Result<(), String> {
        let has_audio = self.wav.frames > 0;
        let (part_video, part_audio, out) = (self.part_video, self.part_audio, self.out);
        self.wav.close().map_err(|e| e.to_string())?;
        let mut cmd = Command::new(ffmpeg());
        cmd.args(["-y", "-v", "error", "-i"]).arg(&part_video);
        if has_audio {
            cmd.arg("-i").arg(&part_audio).args(["-map", "0:v", "-map", "1:a", "-c:a", "aac", "-b:a", "192k"]);
        }
        let status = cmd
            .args(["-c:v", "copy", "-movflags", "+faststart"])
            .arg(&out)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run ffmpeg: {e}"))?;
        if status.status.success() {
            Ok(())
        } else {
            Err(format!("ffmpeg: {}", String::from_utf8_lossy(&status.stderr).trim()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_count_the_frames_started_by_a_time() {
        assert_eq!(slots_until(Duration::ZERO), 0);
        assert_eq!(slots_until(Duration::from_nanos(1)), 1);
        // one second of NTSC video is 59.94 frames
        assert_eq!(slots_until(Duration::from_secs(1)), 60);
        assert_eq!(slots_until(Duration::from_secs(10)), 600);
    }

    #[test]
    fn pictures_are_fitted_to_the_recording_size() {
        let two_by_one = vec![1, 1, 1, 2, 2, 2];
        assert_eq!(fit(two_by_one.clone(), 2, 1, 2, 1), two_by_one);
        assert_eq!(fit(two_by_one, 2, 1, 4, 2), [1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2].repeat(2));
    }

    #[test]
    fn file_names_count_up() {
        let dir = tempfile::tempdir().unwrap();
        let a = next_path(dir.path(), "shot", "png").unwrap();
        assert_eq!(a.file_name().unwrap(), "shot_0001.png");
        std::fs::write(&a, b"").unwrap();
        assert_eq!(next_path(dir.path(), "shot", "png").unwrap().file_name().unwrap(), "shot_0002.png");
    }

    #[test]
    fn a_recording_without_an_encoder_says_so() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: only this test touches FEPOR_FFMPEG
        unsafe { std::env::set_var("FEPOR_FFMPEG", dir.path().join("none")) };
        let c = Capture::new(dir.path().to_path_buf());
        c.toggle_recording();
        assert!(!c.sync_recording(640, 480));
        assert!(c.toast().unwrap().contains("no video encoder"));
        assert!(!c.is_recording());
    }
}
