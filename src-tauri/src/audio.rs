use crate::error::{AppError, AppResult};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

const RETRY_MIN: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(30);
const FALLBACK_CHECK: Duration = Duration::from_secs(5);
const HEALTHY_AFTER: Duration = Duration::from_secs(30);
const IDLE_WAIT: Duration = Duration::from_secs(30);

pub type Notifier = Arc<dyn Fn() + Send + Sync>;

enum AudioCmd {
    Rebuild(Option<String>),
    Refresh,
    SetActive(bool),
    StreamFailed(u64),
}

#[derive(Clone)]
struct Shared {
    recording: Arc<AtomicBool>,
    buffer: Arc<Mutex<Vec<f32>>>,
    sample_rate: Arc<AtomicU32>,
    channels: Arc<AtomicU32>,
    available: Arc<AtomicBool>,
    settled: Arc<AtomicBool>,
    level: Arc<AtomicU32>,
    cmd_tx: Sender<AudioCmd>,
    notify: Notifier,
}

pub struct AudioEngine {
    shared: Shared,
}

impl AudioEngine {
    pub fn new(device: Option<String>, notify: Notifier) -> Arc<AudioEngine> {
        let (cmd_tx, cmd_rx) = channel::<AudioCmd>();
        let shared = Shared {
            recording: Arc::new(AtomicBool::new(false)),
            buffer: Arc::new(Mutex::new(Vec::<f32>::with_capacity(16000 * 30))),
            sample_rate: Arc::new(AtomicU32::new(48000)),
            channels: Arc::new(AtomicU32::new(1)),
            available: Arc::new(AtomicBool::new(false)),
            settled: Arc::new(AtomicBool::new(false)),
            level: Arc::new(AtomicU32::new(0f32.to_bits())),
            cmd_tx,
            notify,
        };

        let thread_shared = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("synapse-audio".to_string())
            .spawn(move || audio_thread(normalize_device(device), thread_shared, cmd_rx));
        if let Err(err) = spawned {
            tracing::error!("audio thread could not be started: {err}");
            shared.settled.store(true, Ordering::Release);
        }

        Arc::new(AudioEngine { shared })
    }

    fn send(&self, cmd: AudioCmd) {
        if self.shared.cmd_tx.send(cmd).is_err() {
            tracing::warn!("audio thread is not running; command dropped");
        }
    }

    pub fn set_device(&self, device: Option<String>) {
        self.send(AudioCmd::Rebuild(normalize_device(device)));
    }

    pub fn refresh(&self) {
        self.send(AudioCmd::Refresh);
    }

    pub fn start(&self) {
        self.shared.buffer.lock().clear();
        self.shared.level.store(0f32.to_bits(), Ordering::Release);
        self.shared.recording.store(true, Ordering::Release);
        self.send(AudioCmd::SetActive(true));
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.shared.level.load(Ordering::Acquire))
    }

    pub fn stop(&self) -> CapturedAudio {
        self.shared.recording.store(false, Ordering::Release);
        self.shared.level.store(0f32.to_bits(), Ordering::Release);
        self.send(AudioCmd::SetActive(false));
        let samples = std::mem::take(&mut *self.shared.buffer.lock());
        let sample_rate = self.shared.sample_rate.load(Ordering::Acquire).max(1);
        let channels = self.shared.channels.load(Ordering::Acquire).max(1);
        let frames = samples.len() as u64 / channels as u64;
        let duration_ms = frames * 1000 / sample_rate as u64;
        tracing::info!(
            "audio captured: {} samples, {} ch, {} Hz, {} ms",
            samples.len(),
            channels,
            sample_rate,
            duration_ms
        );
        CapturedAudio {
            samples,
            sample_rate,
            channels,
            duration_ms,
        }
    }

    pub fn is_available(&self) -> bool {
        self.shared.available.load(Ordering::Acquire)
    }

    pub fn is_settled(&self) -> bool {
        self.shared.settled.load(Ordering::Acquire)
    }
}

pub struct CapturedAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u32,
    pub duration_ms: u64,
}

impl CapturedAudio {
    pub fn to_mono_16k(&self) -> Vec<f32> {
        let mono = downmix(&self.samples, self.channels.max(1));
        resample(&mono, self.sample_rate.max(1), 16000)
    }
}

fn normalize_device(device: Option<String>) -> Option<String> {
    device.filter(|name| !name.trim().is_empty())
}

pub fn list_devices() -> Vec<String> {
    match std::panic::catch_unwind(enumerate_input_names) {
        Ok(names) => names,
        Err(_) => {
            tracing::error!("audio device enumeration panicked");
            Vec::new()
        }
    }
}

fn enumerate_input_names() -> Vec<String> {
    let host = cpal::default_host();
    let mut names = Vec::new();
    match host.input_devices() {
        Ok(devices) => {
            for device in devices {
                if let Some(name) = device_name(&device) {
                    names.push(name);
                }
            }
        }
        Err(err) => tracing::warn!("audio device enumeration failed: {err}"),
    }
    names
}

fn device_name(device: &cpal::Device) -> Option<String> {
    match std::panic::catch_unwind(AssertUnwindSafe(|| device.description())) {
        Ok(Ok(description)) => Some(description.name().to_string()),
        Ok(Err(err)) => {
            tracing::debug!("audio device name unavailable: {err}");
            None
        }
        Err(_) => {
            tracing::warn!("audio device description panicked; device skipped");
            None
        }
    }
}

fn audio_thread(initial_device: Option<String>, shared: Shared, cmd_rx: Receiver<AudioCmd>) {
    let mut worker = Worker::new(initial_device);
    loop {
        let outcome =
            std::panic::catch_unwind(AssertUnwindSafe(|| worker.run(&shared, &cmd_rx)));
        match outcome {
            Ok(()) => {
                tracing::info!("audio thread stopping: command channel closed");
                return;
            }
            Err(_) => {
                tracing::error!("audio thread panicked; restarting the audio supervisor");
                worker.recover(&shared);
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

struct Built {
    stream: cpal::Stream,
    name: String,
    fallback: bool,
    sample_rate: u32,
    channels: u32,
}

struct Worker {
    preferred: Option<String>,
    stream: Option<cpal::Stream>,
    generation: u64,
    active: bool,
    on_fallback: bool,
    retry_at: Option<Instant>,
    backoff: Duration,
    failures: u32,
    built_at: Option<Instant>,
    fallback_check_at: Instant,
    seed: u64,
}

impl Worker {
    fn new(preferred: Option<String>) -> Worker {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15)
            | 1;
        Worker {
            preferred,
            stream: None,
            generation: 0,
            active: false,
            on_fallback: false,
            retry_at: Some(Instant::now()),
            backoff: RETRY_MIN,
            failures: 0,
            built_at: None,
            fallback_check_at: Instant::now() + FALLBACK_CHECK,
            seed,
        }
    }

    fn run(&mut self, shared: &Shared, cmd_rx: &Receiver<AudioCmd>) {
        loop {
            match cmd_rx.recv_timeout(self.wait_duration()) {
                Ok(cmd) => self.handle(cmd, shared),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            self.tick(shared);
        }
    }

    fn wait_duration(&self) -> Duration {
        let now = Instant::now();
        let mut wait = IDLE_WAIT;
        if let Some(at) = self.retry_at {
            wait = wait.min(at.saturating_duration_since(now));
        }
        if self.on_fallback && self.stream.is_some() && !self.active {
            wait = wait.min(self.fallback_check_at.saturating_duration_since(now));
        }
        wait
    }

    fn handle(&mut self, cmd: AudioCmd, shared: &Shared) {
        match cmd {
            AudioCmd::Rebuild(device) => {
                self.preferred = device;
                self.backoff = RETRY_MIN;
                self.failures = 0;
                self.rebuild(shared, "device selection changed");
            }
            AudioCmd::Refresh => {
                self.backoff = RETRY_MIN;
                self.rebuild(shared, "refresh requested");
            }
            AudioCmd::SetActive(on) => self.set_active(on, shared),
            AudioCmd::StreamFailed(generation) => {
                if generation == self.generation && self.stream.is_some() {
                    tracing::warn!("audio stream failed; scheduling rebuild");
                    self.drop_stream();
                    shared.available.store(false, Ordering::Release);
                    self.schedule_retry();
                    (shared.notify)();
                }
            }
        }
    }

    fn tick(&mut self, shared: &Shared) {
        let now = Instant::now();
        if let Some(at) = self.retry_at {
            if now >= at {
                self.rebuild(shared, "retry");
                return;
            }
        }
        if let Some(built) = self.built_at {
            if now.duration_since(built) >= HEALTHY_AFTER {
                self.backoff = RETRY_MIN;
                self.failures = 0;
            }
        }
        if self.on_fallback
            && self.stream.is_some()
            && !self.active
            && now >= self.fallback_check_at
        {
            self.fallback_check_at = now + FALLBACK_CHECK;
            if preferred_present(&self.preferred) {
                self.rebuild(shared, "preferred device is back");
            }
        }
    }

    fn set_active(&mut self, on: bool, shared: &Shared) {
        self.active = on;
        let result = self
            .stream
            .as_ref()
            .map(|stream| if on { stream.play() } else { stream.pause() });
        match result {
            Some(Ok(())) => {}
            Some(Err(err)) if on => {
                tracing::warn!("audio stream play failed ({err}); rebuilding once");
                self.rebuild(shared, "play failed");
            }
            Some(Err(err)) => tracing::debug!("audio stream pause failed: {err}"),
            None if on => self.rebuild(shared, "recording requested without a stream"),
            None => {}
        }
    }

    fn drop_stream(&mut self) {
        self.built_at = None;
        if let Some(stream) = self.stream.take() {
            if std::panic::catch_unwind(AssertUnwindSafe(move || drop(stream))).is_err() {
                tracing::warn!("audio stream teardown panicked");
            }
        }
    }

    fn schedule_retry(&mut self) {
        let delay = jittered(self.backoff, &mut self.seed);
        self.retry_at = Some(Instant::now() + delay);
        self.backoff = (self.backoff * 2).min(RETRY_MAX);
    }

    fn rebuild(&mut self, shared: &Shared, reason: &str) {
        self.drop_stream();
        self.retry_at = None;
        self.generation = self.generation.wrapping_add(1);
        match build_stream(&self.preferred, shared, self.generation) {
            Ok(built) => {
                let started = if self.active {
                    built.stream.play()
                } else {
                    built.stream.pause()
                };
                match started {
                    Err(err) if self.active => {
                        tracing::warn!(
                            "audio input '{}' could not start ({err}); will retry",
                            built.name
                        );
                        drop_quietly(built.stream);
                        self.failures = self.failures.saturating_add(1);
                        shared.available.store(false, Ordering::Release);
                        self.schedule_retry();
                    }
                    other => {
                        if let Err(err) = other {
                            tracing::debug!("audio stream pause after build failed: {err}");
                        }
                        tracing::info!(
                            "audio input ready: '{}' ({} Hz, {} ch{}) [{reason}]",
                            built.name,
                            built.sample_rate,
                            built.channels,
                            if built.fallback {
                                ", preferred device missing, using default"
                            } else {
                                ""
                            }
                        );
                        self.stream = Some(built.stream);
                        self.on_fallback = built.fallback;
                        self.built_at = Some(Instant::now());
                        self.fallback_check_at = Instant::now() + FALLBACK_CHECK;
                        shared.available.store(true, Ordering::Release);
                    }
                }
            }
            Err(err) => {
                self.failures = self.failures.saturating_add(1);
                if self.failures == 1 || self.failures % 10 == 0 {
                    tracing::warn!(
                        "audio input unavailable ({err}) [{reason}]; attempt {} will retry",
                        self.failures
                    );
                } else {
                    tracing::debug!("audio input still unavailable ({err})");
                }
                shared.available.store(false, Ordering::Release);
                self.schedule_retry();
            }
        }
        shared.settled.store(true, Ordering::Release);
        (shared.notify)();
    }

    fn recover(&mut self, shared: &Shared) {
        self.drop_stream();
        shared.available.store(false, Ordering::Release);
        shared.settled.store(true, Ordering::Release);
        self.backoff = RETRY_MIN;
        self.retry_at = Some(Instant::now() + RETRY_MIN);
        (shared.notify)();
    }
}

fn drop_quietly(stream: cpal::Stream) {
    if std::panic::catch_unwind(AssertUnwindSafe(move || drop(stream))).is_err() {
        tracing::warn!("audio stream teardown panicked");
    }
}

fn jittered(base: Duration, seed: &mut u64) -> Duration {
    let mut x = *seed;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *seed = x;
    let factor = 0.8 + (x % 1000) as f64 / 2500.0;
    base.mul_f64(factor)
}

fn preferred_present(preferred: &Option<String>) -> bool {
    let target = match preferred {
        Some(target) => target,
        None => return false,
    };
    let host = cpal::default_host();
    match host.input_devices() {
        Ok(devices) => devices
            .into_iter()
            .any(|device| device_name(&device).as_deref() == Some(target.as_str())),
        Err(_) => false,
    }
}

fn select_device(host: &cpal::Host, preferred: &Option<String>) -> Option<(cpal::Device, bool)> {
    match preferred {
        Some(target) => {
            if let Ok(devices) = host.input_devices() {
                for device in devices {
                    if device_name(&device).as_deref() == Some(target.as_str()) {
                        return Some((device, false));
                    }
                }
            }
            host.default_input_device().map(|device| (device, true))
        }
        None => host.default_input_device().map(|device| (device, false)),
    }
}

fn error_callback(tx: Sender<AudioCmd>, generation: u64) -> impl FnMut(cpal::Error) + Send + 'static {
    move |err: cpal::Error| match err.kind() {
        cpal::ErrorKind::Xrun | cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied => {
            tracing::debug!("audio stream notice: {err}");
        }
        _ => {
            tracing::error!("audio stream error: {err}");
            let _ = tx.send(AudioCmd::StreamFailed(generation));
        }
    }
}

fn build_stream(
    device_name_pref: &Option<String>,
    shared: &Shared,
    generation: u64,
) -> AppResult<Built> {
    let host = cpal::default_host();
    let (device, fallback) = select_device(&host, device_name_pref)
        .ok_or_else(|| AppError::Audio("no input device available".to_string()))?;
    let name = device_name(&device).unwrap_or_else(|| "unnamed input".to_string());

    let supported = device
        .default_input_config()
        .map_err(|e| AppError::Audio(format!("default input config failed: {e}")))?;

    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    shared.sample_rate.store(config.sample_rate, Ordering::Release);
    shared.channels.store(config.channels as u32, Ordering::Release);

    let err_fn = error_callback(shared.cmd_tx.clone(), generation);
    let recording = &shared.recording;
    let buffer = &shared.buffer;
    let level = &shared.level;

    let stream = match sample_format {
        cpal::SampleFormat::F32 => {
            let rec = recording.clone();
            let buf = buffer.clone();
            let lvl = level.clone();
            device.build_input_stream(
                config.clone(),
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    capture_samples(data, &rec, &buf, &lvl, |s| *s);
                },
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let rec = recording.clone();
            let buf = buffer.clone();
            let lvl = level.clone();
            device.build_input_stream(
                config.clone(),
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    capture_samples(data, &rec, &buf, &lvl, |s| *s as f32 / 32768.0);
                },
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let rec = recording.clone();
            let buf = buffer.clone();
            let lvl = level.clone();
            device.build_input_stream(
                config.clone(),
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    capture_samples(data, &rec, &buf, &lvl, |s| (*s as f32 - 32768.0) / 32768.0);
                },
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::I32 => {
            let rec = recording.clone();
            let buf = buffer.clone();
            let lvl = level.clone();
            device.build_input_stream(
                config.clone(),
                move |data: &[i32], _: &cpal::InputCallbackInfo| {
                    capture_samples(data, &rec, &buf, &lvl, |s| *s as f32 / 2_147_483_648.0);
                },
                err_fn,
                None,
            )
        }
        other => {
            return Err(AppError::Audio(format!(
                "unsupported sample format: {other:?}"
            )))
        }
    }
    .map_err(|e| AppError::Audio(format!("build input stream failed: {e}")))?;

    Ok(Built {
        stream,
        name,
        fallback,
        sample_rate: config.sample_rate,
        channels: config.channels as u32,
    })
}

fn capture_samples<T, F>(
    data: &[T],
    recording: &Arc<AtomicBool>,
    buffer: &Arc<Mutex<Vec<f32>>>,
    level: &Arc<AtomicU32>,
    convert: F,
) where
    F: Fn(&T) -> f32,
{
    if !recording.load(Ordering::Acquire) {
        return;
    }
    let mut sum_squares = 0.0f32;
    {
        let mut guard = buffer.lock();
        if !recording.load(Ordering::Acquire) {
            return;
        }
        guard.reserve(data.len());
        for sample in data {
            let value = convert(sample);
            sum_squares += value * value;
            guard.push(value);
        }
    }
    if !data.is_empty() {
        let rms = (sum_squares / data.len() as f32).sqrt();
        level.store(rms.to_bits(), Ordering::Release);
    }
}

fn downmix(samples: &[f32], channels: u32) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    let channels = channels as usize;
    let frames = samples.len() / channels;
    let mut mono = Vec::with_capacity(frames);
    for frame in 0..frames {
        let base = frame * channels;
        let mut sum = 0.0f32;
        for c in 0..channels {
            sum += samples[base + c];
        }
        mono.push(sum / channels as f32);
    }
    mono
}

fn resample(input: &[f32], in_rate: u32, out_rate: u32) -> Vec<f32> {
    if input.is_empty() || in_rate == out_rate {
        return input.to_vec();
    }
    let ratio = in_rate as f64 / out_rate as f64;
    let out_len = ((input.len() as f64) / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    let mut cursor = 0usize;
    for n in 0..out_len {
        let start = cursor;
        let mut end = (((n + 1) as f64) * ratio).floor() as usize;
        if end > input.len() {
            end = input.len();
        }
        if end <= start {
            end = (start + 1).min(input.len());
        }
        let slice = &input[start..end];
        let sum: f32 = slice.iter().sum();
        out.push(sum / slice.len() as f32);
        cursor = end;
    }
    out
}
