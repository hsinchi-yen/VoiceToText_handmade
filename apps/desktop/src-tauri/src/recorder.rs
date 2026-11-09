//! Microphone capture (`tasks/plan.md` section 7): `cpal` default (or configured) input device
//! -> mono -> `rubato` resample to 16 kHz.
//!
//! `mic_mode = always_on` (default) keeps the stream open and the last `preroll_ms` of audio in
//! an in-memory ring buffer, which is prepended when recording starts so the first word is not
//! lost. `on_press` opens the stream only while recording. On a device error the stream is
//! reopened automatically. Audio never leaves memory.
//!
//! Threads: cpal's callback thread only down-mixes and forwards samples; a manager thread owns
//! the stream, resamples, and fills the buffers.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};
use tracing::{debug, info, warn};
use v2c_core::audio::{AudioClip, SAMPLE_RATE};
use v2c_core::config::{AudioConfig, MicMode};

/// What the controller needs from a recorder. Implemented by [`Recorder`] and by test fakes.
pub trait AudioSource: Send + Sync {
    /// Starts collecting a clip (with the pre-roll prepended in `always_on` mode).
    fn begin(&self);
    /// Stops and returns the clip collected since [`AudioSource::begin`].
    fn finish(&self) -> AudioClip;
    /// Stops and discards the current clip.
    fn cancel(&self);
    /// Current input level (RMS of the latest audio block), for the overlay meter.
    fn level(&self) -> f32;
}

/// Stream state changes, reported to the controller (codes only, no audio).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecorderNotice {
    Opened,
    /// The stream could not be opened or was lost; it will be retried.
    Unavailable {
        code: &'static str,
    },
}

pub type NoticeFn = Arc<dyn Fn(RecorderNotice) + Send + Sync>;

const RETRY_INTERVAL: Duration = Duration::from_secs(2);
/// While recording, no audio callback for this long means the device stopped delivering
/// (e.g. a USB/Bluetooth headset went away without an error): report `mic_lost` and reopen.
const STALL_TIMEOUT: Duration = Duration::from_secs(1);
/// How often the manager wakes up to check for a stall while a stream is open.
const STALL_POLL: Duration = Duration::from_millis(250);
const FINISH_TIMEOUT: Duration = Duration::from_millis(200);
/// Input frames per resampler call (about 21 ms at 48 kHz).
const RESAMPLE_CHUNK: usize = 1024;

enum Msg {
    Data {
        generation: u64,
        samples: Vec<f32>,
    },
    StreamError {
        generation: u64,
        kind: cpal::ErrorKind,
    },
    Open,
    Close,
    /// Take the clip after all audio queued before this message has been processed.
    Finish(mpsc::Sender<AudioClip>),
    Shutdown,
}

struct Shared {
    capture: Mutex<Capture>,
    level_bits: AtomicU32,
}

impl Shared {
    fn set_level(&self, level: f32) {
        self.level_bits.store(level.to_bits(), Ordering::Relaxed);
    }
}

/// The real microphone recorder.
pub struct Recorder {
    shared: Arc<Shared>,
    tx: mpsc::Sender<Msg>,
    mic_mode: MicMode,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Recorder {
    /// Starts the manager thread; in `always_on` mode the stream opens immediately.
    pub fn start(cfg: &AudioConfig, notify: NoticeFn) -> Self {
        let preroll_samples = (cfg.preroll_ms * u64::from(SAMPLE_RATE) / 1000) as usize;
        let shared = Arc::new(Shared {
            capture: Mutex::new(Capture::new(preroll_samples)),
            level_bits: AtomicU32::new(0),
        });
        let (tx, rx) = mpsc::channel();
        let manager = Manager {
            mic_mode: cfg.mic_mode,
            device: cfg.device.clone(),
            shared: Arc::clone(&shared),
            tx: tx.clone(),
            notify,
        };
        let thread = std::thread::Builder::new()
            .name("v2c-recorder".into())
            .spawn(move || manager.run(rx))
            .ok();
        if thread.is_none() {
            warn!("could not spawn recorder thread");
        }
        Self {
            shared,
            tx,
            mic_mode: cfg.mic_mode,
            thread: Mutex::new(thread),
        }
    }

    /// Closes the stream and stops the manager thread (at app exit; idempotent).
    pub fn shutdown(&self) {
        let _ = self.tx.send(Msg::Shutdown);
        let handle = self.thread.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(t) = handle {
            let _ = t.join();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Capture> {
        self.shared
            .capture
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }
}

impl AudioSource for Recorder {
    fn begin(&self) {
        self.lock().begin();
        if self.mic_mode == MicMode::OnPress {
            let _ = self.tx.send(Msg::Open);
        }
    }

    fn finish(&self) -> AudioClip {
        let (tx, rx) = mpsc::channel();
        let clip = if self.tx.send(Msg::Finish(tx)).is_ok() {
            rx.recv_timeout(FINISH_TIMEOUT).ok()
        } else {
            None
        };
        let clip = clip.unwrap_or_else(|| self.lock().finish());
        if self.mic_mode == MicMode::OnPress {
            let _ = self.tx.send(Msg::Close);
        }
        clip
    }

    fn cancel(&self) {
        self.lock().cancel();
        if self.mic_mode == MicMode::OnPress {
            let _ = self.tx.send(Msg::Close);
        }
    }

    fn level(&self) -> f32 {
        f32::from_bits(self.shared.level_bits.load(Ordering::Relaxed))
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Active {
    // Dropping the stream closes the device.
    _stream: cpal::Stream,
    generation: u64,
    resampler: To16k,
    /// Last audio callback (or the open time).
    last_data: Instant,
}

/// The stream is open and a clip is being recorded, but no audio arrived for
/// [`STALL_TIMEOUT`].
fn stalled(recording: bool, last_data: Instant, now: Instant) -> bool {
    recording && now.saturating_duration_since(last_data) > STALL_TIMEOUT
}

struct Manager {
    mic_mode: MicMode,
    device: Option<String>,
    shared: Arc<Shared>,
    tx: mpsc::Sender<Msg>,
    notify: NoticeFn,
}

impl Manager {
    fn run(self, rx: mpsc::Receiver<Msg>) {
        let mut active: Option<Active> = None;
        let mut generation = 0u64;
        let mut want_open = self.mic_mode == MicMode::AlwaysOn;
        let mut next_attempt = Instant::now();
        let mut failure_reported = false;

        loop {
            if want_open && active.is_none() && Instant::now() >= next_attempt {
                generation += 1;
                match self.open(generation) {
                    Ok(a) => {
                        active = Some(a);
                        failure_reported = false;
                        (self.notify)(RecorderNotice::Opened);
                    }
                    Err(code) => {
                        if !failure_reported {
                            warn!(code, "microphone unavailable; will retry");
                            (self.notify)(RecorderNotice::Unavailable { code });
                            failure_reported = true;
                        }
                        next_attempt = Instant::now() + RETRY_INTERVAL;
                    }
                }
            }

            if let Some(a) = active.as_ref() {
                let recording = self.lock().is_recording();
                if stalled(recording, a.last_data, Instant::now()) {
                    warn!("no audio from the microphone while recording; reopening");
                    active = None;
                    self.shared.set_level(0.0);
                    (self.notify)(RecorderNotice::Unavailable { code: "mic_lost" });
                    failure_reported = true;
                    next_attempt = Instant::now();
                    continue;
                }
            }

            let wait = if want_open && active.is_none() {
                RETRY_INTERVAL
            } else if active.is_some() {
                STALL_POLL
            } else {
                Duration::from_secs(60)
            };
            match rx.recv_timeout(wait) {
                Ok(Msg::Data {
                    generation: g,
                    samples,
                }) => {
                    if let Some(a) = active.as_mut().filter(|a| a.generation == g) {
                        a.last_data = Instant::now();
                        let out = a.resampler.push(&samples);
                        if !out.is_empty() {
                            self.shared.set_level(rms(&out));
                            self.lock().push(&out);
                        }
                    }
                }
                Ok(Msg::StreamError {
                    generation: g,
                    kind,
                }) => {
                    if active.as_ref().is_some_and(|a| a.generation == g) {
                        if stream_survives(kind) {
                            debug!(?kind, "audio stream notice");
                        } else {
                            warn!(?kind, "audio stream lost; reopening");
                            active = None;
                            self.shared.set_level(0.0);
                            self.lock().clear_preroll();
                            (self.notify)(RecorderNotice::Unavailable { code: "mic_lost" });
                            failure_reported = true;
                            next_attempt = Instant::now();
                        }
                    }
                }
                Ok(Msg::Open) => {
                    want_open = true;
                    next_attempt = Instant::now();
                }
                Ok(Msg::Close) => {
                    if self.mic_mode == MicMode::OnPress {
                        want_open = false;
                        active = None;
                        self.shared.set_level(0.0);
                        self.lock().clear_preroll();
                    }
                }
                Ok(Msg::Finish(reply)) => {
                    if let Some(a) = active.as_mut() {
                        let tail = a.resampler.flush();
                        self.lock().push(&tail);
                    }
                    let _ = reply.send(self.lock().finish());
                }
                Ok(Msg::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Capture> {
        self.shared
            .capture
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    fn open(&self, generation: u64) -> Result<Active, &'static str> {
        let host = cpal::default_host();
        let device = match self.device.as_deref() {
            Some(name) => find_input_device(&host, name).or_else(|| {
                warn!("configured microphone not found; using the default input device");
                host.default_input_device()
            }),
            None => host.default_input_device(),
        }
        .ok_or("mic_not_found")?;
        let supported = device
            .default_input_config()
            .map_err(|_| "mic_config_failed")?;
        let rate = supported.sample_rate();
        let channels = supported.channels();
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();

        let tx = self.tx.clone();
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, &config, generation, tx),
            SampleFormat::I16 => build::<i16>(&device, &config, generation, tx),
            SampleFormat::U16 => build::<u16>(&device, &config, generation, tx),
            SampleFormat::I32 => build::<i32>(&device, &config, generation, tx),
            SampleFormat::U32 => build::<u32>(&device, &config, generation, tx),
            SampleFormat::I8 => build::<i8>(&device, &config, generation, tx),
            SampleFormat::U8 => build::<u8>(&device, &config, generation, tx),
            SampleFormat::F64 => build::<f64>(&device, &config, generation, tx),
            _ => Err("mic_format_unsupported"),
        }?;
        stream.play().map_err(|_| "mic_start_failed")?;
        let resampler = To16k::new(rate).ok_or("resampler_failed")?;
        info!(
            sample_rate = rate,
            channels,
            ?format,
            "microphone stream opened"
        );
        Ok(Active {
            _stream: stream,
            generation,
            resampler,
            last_data: Instant::now(),
        })
    }
}

fn find_input_device(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    host.input_devices().ok()?.find(|d| {
        d.description()
            .map(|desc| desc.name() == name)
            .unwrap_or(false)
    })
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    generation: u64,
    tx: mpsc::Sender<Msg>,
) -> Result<cpal::Stream, &'static str>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let channels = config.channels;
    let data_tx = tx.clone();
    device
        .build_input_stream::<T, _, _>(
            *config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let samples = downmix(data, channels);
                let _ = data_tx.send(Msg::Data {
                    generation,
                    samples,
                });
            },
            move |err: cpal::Error| {
                let _ = tx.send(Msg::StreamError {
                    generation,
                    kind: err.kind(),
                });
            },
            None,
        )
        .map_err(|_| "mic_open_failed")
}

/// Errors after which the stream keeps running (cpal reroutes default-device streams itself).
fn stream_survives(kind: cpal::ErrorKind) -> bool {
    matches!(
        kind,
        cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::Xrun | cpal::ErrorKind::RealtimeDenied
    )
}

/// Averages interleaved channels into mono f32.
fn downmix<T>(interleaved: &[T], channels: u16) -> Vec<f32>
where
    T: Copy,
    f32: FromSample<T>,
{
    let ch = usize::from(channels.max(1));
    if ch == 1 {
        return interleaved.iter().map(|&s| f32::from_sample_(s)).collect();
    }
    interleaved
        .chunks_exact(ch)
        .map(|frame| frame.iter().map(|&s| f32::from_sample_(s)).sum::<f32>() / ch as f32)
        .collect()
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Streaming mono resampler to 16 kHz.
enum To16k {
    Passthrough,
    Fft {
        resampler: Box<Fft<f32>>,
        pending: Vec<f32>,
        out: Vec<f32>,
        /// Output rate / input rate.
        ratio: f64,
        /// Frames fed in and produced since the last reset.
        frames_in: u64,
        frames_out: u64,
    },
}

impl To16k {
    fn new(input_rate: u32) -> Option<Self> {
        if input_rate == SAMPLE_RATE {
            return Some(Self::Passthrough);
        }
        let resampler = Fft::<f32>::new(
            input_rate as usize,
            SAMPLE_RATE as usize,
            RESAMPLE_CHUNK,
            1,
            FixedSync::Input,
        )
        .ok()?;
        Some(Self::Fft {
            resampler: Box::new(resampler),
            pending: Vec::new(),
            out: Vec::new(),
            ratio: f64::from(SAMPLE_RATE) / f64::from(input_rate),
            frames_in: 0,
            frames_out: 0,
        })
    }

    /// Emits everything still inside the resampler (the partial input chunk plus the filter
    /// delay) by zero-padding, then resets it. Called when a clip ends so its last ~20-40 ms
    /// are not lost.
    fn flush(&mut self) -> Vec<f32> {
        let Self::Fft {
            resampler,
            pending,
            out,
            ratio,
            frames_in,
            frames_out,
        } = self
        else {
            return Vec::new();
        };
        // Everything fed in, at the output rate, plus the filter delay (which is leading
        // silence emitted when the stream started), minus what was already produced.
        let total_in = *frames_in + pending.len() as u64;
        let owed = ((total_in as f64 * *ratio).round() as u64 + resampler.output_delay() as u64)
            .saturating_sub(*frames_out) as usize;
        let mut result = Vec::with_capacity(owed + RESAMPLE_CHUNK);
        let mut partial = pending.len();
        // A few chunks at most: the delay is shorter than one chunk.
        for _ in 0..4 {
            if result.len() >= owed {
                break;
            }
            let out_len = resampler.output_frames_next();
            out.resize(out_len, 0.0);
            let processed = {
                let (Ok(inp), Ok(mut outp)) = (
                    InterleavedSlice::new(&pending[..], 1, partial),
                    InterleavedSlice::new_mut(&mut out[..], 1, out_len),
                ) else {
                    break;
                };
                let indexing = Indexing::new().partial_len(partial);
                resampler.process_into_buffer(&inp, &mut outp, Some(&indexing))
            };
            match processed {
                Ok((_, written)) => result.extend_from_slice(&out[..written]),
                Err(_) => break,
            }
            pending.clear();
            partial = 0;
        }
        result.truncate(owed);
        pending.clear();
        resampler.reset();
        *frames_in = 0;
        *frames_out = 0;
        result
    }

    /// Feeds input samples; returns whatever 16 kHz output is ready.
    fn push(&mut self, input: &[f32]) -> Vec<f32> {
        match self {
            Self::Passthrough => input.to_vec(),
            Self::Fft {
                resampler,
                pending,
                out,
                frames_in,
                frames_out,
                ..
            } => {
                pending.extend_from_slice(input);
                let mut result = Vec::new();
                loop {
                    let need = resampler.input_frames_next();
                    if pending.len() < need {
                        break;
                    }
                    let out_len = resampler.output_frames_next();
                    out.resize(out_len, 0.0);
                    let processed = {
                        let (Ok(inp), Ok(mut outp)) = (
                            InterleavedSlice::new(&pending[..need], 1, need),
                            InterleavedSlice::new_mut(&mut out[..], 1, out_len),
                        ) else {
                            pending.clear();
                            break;
                        };
                        resampler.process_into_buffer(&inp, &mut outp, None)
                    };
                    match processed {
                        Ok((used, written)) => {
                            result.extend_from_slice(&out[..written]);
                            pending.drain(..used);
                            *frames_in += used as u64;
                            *frames_out += written as u64;
                        }
                        Err(_) => {
                            pending.clear();
                            break;
                        }
                    }
                }
                result
            }
        }
    }
}

/// Pre-roll ring buffer plus the clip being recorded (16 kHz mono).
struct Capture {
    preroll: VecDeque<f32>,
    preroll_cap: usize,
    recording: Option<Vec<f32>>,
}

impl Capture {
    fn new(preroll_cap: usize) -> Self {
        Self {
            preroll: VecDeque::with_capacity(preroll_cap),
            preroll_cap,
            recording: None,
        }
    }

    fn push(&mut self, samples: &[f32]) {
        if let Some(rec) = self.recording.as_mut() {
            rec.extend_from_slice(samples);
            return;
        }
        if self.preroll_cap == 0 {
            return;
        }
        self.preroll.extend(samples.iter().copied());
        let excess = self.preroll.len().saturating_sub(self.preroll_cap);
        self.preroll.drain(..excess);
    }

    /// Starts a clip that begins with the buffered pre-roll.
    fn begin(&mut self) {
        let mut clip = Vec::with_capacity(self.preroll.len() + SAMPLE_RATE as usize * 10);
        clip.extend(self.preroll.drain(..));
        self.recording = Some(clip);
    }

    fn finish(&mut self) -> AudioClip {
        AudioClip::new(self.recording.take().unwrap_or_default())
    }

    fn cancel(&mut self) {
        self.recording = None;
    }

    fn clear_preroll(&mut self) {
        self.preroll.clear();
    }

    fn is_recording(&self) -> bool {
        self.recording.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stall_only_counts_while_recording() {
        let t0 = Instant::now();
        let later = t0 + STALL_TIMEOUT + Duration::from_millis(1);
        assert!(stalled(true, t0, later));
        assert!(
            !stalled(false, t0, later),
            "idle always-on stream is not watched"
        );
        assert!(!stalled(true, t0, t0 + STALL_TIMEOUT));
        assert!(!stalled(true, later, t0), "clock order is tolerated");
    }

    #[test]
    fn downmix_averages_and_converts() {
        assert_eq!(downmix(&[0.2f32, 0.4, -1.0, 1.0], 2), vec![0.3, 0.0]);
        assert_eq!(downmix(&[0.5f32], 1), vec![0.5]);
        let m = downmix(&[i16::MAX, i16::MAX], 2);
        assert!((m[0] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn preroll_keeps_only_the_last_samples_and_is_prepended() {
        let mut c = Capture::new(4);
        c.push(&[1.0, 2.0, 3.0]);
        c.push(&[4.0, 5.0, 6.0]);
        c.begin();
        c.push(&[7.0, 8.0]);
        assert_eq!(c.finish().samples, vec![3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
        // After finishing, the ring refills from scratch.
        c.push(&[9.0]);
        c.begin();
        assert_eq!(c.finish().samples, vec![9.0]);
    }

    #[test]
    fn cancel_discards_and_finish_without_begin_is_empty() {
        let mut c = Capture::new(2);
        c.begin();
        c.push(&[1.0]);
        c.cancel();
        assert!(c.finish().samples.is_empty());
        c.push(&[2.0, 3.0]);
        c.clear_preroll();
        c.begin();
        assert!(c.finish().samples.is_empty());
    }

    #[test]
    fn half_second_preroll_at_16k() {
        let mut c = Capture::new(8_000);
        c.push(&vec![0.1; 48_000]);
        c.begin();
        assert_eq!(c.finish().duration_ms(), 500);
    }

    #[test]
    fn resampler_passthrough_at_16k() {
        let mut r = To16k::new(16_000).unwrap();
        assert_eq!(r.push(&[0.1, 0.2]), vec![0.1, 0.2]);
    }

    #[test]
    fn resampler_48k_to_16k_keeps_duration_and_level() {
        let mut r = To16k::new(48_000).unwrap();
        let mut out = Vec::new();
        // One second of a 440 Hz tone in uneven blocks, like a capture callback.
        let tone: Vec<f32> = (0..48_000)
            .map(|i| 0.5 * (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin())
            .collect();
        for block in tone.chunks(480 + 7) {
            out.extend(r.push(block));
        }
        // Up to one chunk stays pending inside the resampler.
        assert!(out.len() <= 16_000, "{}", out.len());
        assert!(
            out.len() >= 16_000 - 2 * RESAMPLE_CHUNK / 3,
            "{}",
            out.len()
        );
        let level = rms(&out[4_000..]);
        assert!((level - 0.5 / 2f32.sqrt()).abs() < 0.02, "{level}");
    }

    #[test]
    fn flush_emits_the_tail_so_duration_is_exact() {
        for rate in [48_000u32, 44_100] {
            let mut r = To16k::new(rate).unwrap();
            let input: Vec<f32> = (0..rate as usize)
                .map(|i| 0.5 * (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin())
                .collect();
            let mut out = Vec::new();
            for block in input.chunks(480 + 7) {
                out.extend(r.push(block));
            }
            let before = out.len();
            out.extend(r.flush());
            assert!(out.len() > before, "{rate}: flush produced nothing");
            // One second in -> one second out, plus the filter delay emitted as leading
            // silence when the stream started.
            let delay = match &r {
                To16k::Fft { resampler, .. } => resampler.output_delay(),
                To16k::Passthrough => 0,
            };
            assert!(
                out.len().abs_diff(16_000 + delay) <= 2,
                "{rate}: {} (delay {delay})",
                out.len()
            );
            // The end of the clip is real audio, not padding.
            let tail_level = rms(&out[out.len() - 160..]);
            assert!(tail_level > 0.2, "{rate}: tail rms {tail_level}");
            // The resampler is reset and keeps working after a flush.
            assert!(!r.push(&input).is_empty());
        }
        assert!(To16k::new(16_000).unwrap().flush().is_empty());
    }

    #[test]
    fn resampler_44k1() {
        let mut r = To16k::new(44_100).unwrap();
        let out = r.push(&vec![0.0; 44_100]);
        assert!(out.len() > 15_000 && out.len() <= 16_000, "{}", out.len());
    }

    #[test]
    fn rms_values() {
        assert_eq!(rms(&[]), 0.0);
        assert!((rms(&[0.5, -0.5]) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn only_fatal_stream_errors_reopen() {
        assert!(stream_survives(cpal::ErrorKind::DeviceChanged));
        assert!(stream_survives(cpal::ErrorKind::Xrun));
        assert!(!stream_survives(cpal::ErrorKind::DeviceNotAvailable));
        assert!(!stream_survives(cpal::ErrorKind::StreamInvalidated));
    }
}
