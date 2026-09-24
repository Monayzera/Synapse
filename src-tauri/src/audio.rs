use crate::error::{AppError, AppResult};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

enum AudioCmd {
    Rebuild(Option<String>),
    SetActive(bool),
}

pub struct AudioEngine {
    recording: Arc<AtomicBool>,
    buffer: Arc<Mutex<Vec<f32>>>,
    sample_rate: Arc<AtomicU32>,
    channels: Arc<AtomicU32>,
    available: Arc<AtomicBool>,
    level: Arc<AtomicU32>,
    cmd_tx: Sender<AudioCmd>,
}

impl AudioEngine {
    pub fn new(device: Option<String>) -> Arc<AudioEngine> {
        let recording = Arc::new(AtomicBool::new(false));
        let buffer = Arc::new(Mutex::new(Vec::<f32>::with_capacity(16000 * 30)));
        let sample_rate = Arc::new(AtomicU32::new(48000));
        let channels = Arc::new(AtomicU32::new(1));
        let available = Arc::new(AtomicBool::new(false));
        let level = Arc::new(AtomicU32::new(0f32.to_bits()));
        let (cmd_tx, cmd_rx) = channel::<AudioCmd>();

        let engine = AudioEngine {
            recording: recording.clone(),
            buffer: buffer.clone(),
            sample_rate: sample_rate.clone(),
            channels: channels.clone(),
            available: available.clone(),
            level: level.clone(),
            cmd_tx,
        };

        std::thread::Builder::new()
            .name("synapse-audio".to_string())
            .spawn(move || {
                audio_thread(
                    device,
                    recording,
                    buffer,
                    sample_rate,
                    channels,
                    available,
                    level,
                    cmd_rx,
                );
            })
            .ok();

        Arc::new(engine)
    }

    pub fn set_device(&self, device: Option<String>) {
        let _ = self.cmd_tx.send(AudioCmd::Rebuild(device));
    }

    pub fn start(&self) {
        self.buffer.lock().clear();
        self.level.store(0f32.to_bits(), Ordering::Release);
        self.recording.store(true, Ordering::Release);
        let _ = self.cmd_tx.send(AudioCmd::SetActive(true));
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Acquire))
    }

    pub fn stop(&self) -> CapturedAudio {
        self.recording.store(false, Ordering::Release);
        self.level.store(0f32.to_bits(), Ordering::Release);
        let _ = self.cmd_tx.send(AudioCmd::SetActive(false));
        let samples = std::mem::take(&mut *self.buffer.lock());
        let sample_rate = self.sample_rate.load(Ordering::Acquire).max(1);
        let channels = self.channels.load(Ordering::Acquire).max(1);
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
        self.available.load(Ordering::Acquire)
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

pub fn list_devices() -> Vec<String> {
    let host = cpal::default_host();
    let mut names = Vec::new();
    if let Ok(devices) = host.input_devices() {
        for device in devices {
            names.push(device.to_string());
        }
    }
    names
}

fn audio_thread(
    initial_device: Option<String>,
    recording: Arc<AtomicBool>,
    buffer: Arc<Mutex<Vec<f32>>>,
    sample_rate: Arc<AtomicU32>,
    channels: Arc<AtomicU32>,
    available: Arc<AtomicBool>,
    level: Arc<AtomicU32>,
    cmd_rx: Receiver<AudioCmd>,
) {
    let mut current = initial_device;
    let mut active = false;
    let mut stream =
        match build_stream(&current, &recording, &buffer, &sample_rate, &channels, &level) {
            Ok(stream) => {
                let _ = stream.pause();
                available.store(true, Ordering::Release);
                Some(stream)
            }
            Err(err) => {
                tracing::error!("audio stream init failed: {err}");
                available.store(false, Ordering::Release);
                None
            }
        };

    loop {
        match cmd_rx.recv() {
            Ok(AudioCmd::Rebuild(device)) => {
                drop(stream.take());
                current = device;
                match build_stream(&current, &recording, &buffer, &sample_rate, &channels, &level) {
                    Ok(new_stream) => {
                        let _ = if active {
                            new_stream.play()
                        } else {
                            new_stream.pause()
                        };
                        available.store(true, Ordering::Release);
                        stream = Some(new_stream);
                    }
                    Err(err) => {
                        tracing::error!("audio stream rebuild failed: {err}");
                        available.store(false, Ordering::Release);
                    }
                }
            }
            Ok(AudioCmd::SetActive(on)) => {
                active = on;
                if let Some(s) = stream.as_ref() {
                    let res = if on { s.play() } else { s.pause() };
                    if let Err(err) = res {
                        tracing::warn!("audio stream toggle failed: {err}");
                    }
                }
            }
            Err(_) => return,
        }
    }
}

fn select_device(host: &cpal::Host, name: &Option<String>) -> Option<cpal::Device> {
    match name {
        Some(target) => {
            if let Ok(devices) = host.input_devices() {
                for device in devices {
                    if &device.to_string() == target {
                        return Some(device);
                    }
                }
            }
            host.default_input_device()
        }
        None => host.default_input_device(),
    }
}

fn build_stream(
    device_name: &Option<String>,
    recording: &Arc<AtomicBool>,
    buffer: &Arc<Mutex<Vec<f32>>>,
    sample_rate: &Arc<AtomicU32>,
    channels: &Arc<AtomicU32>,
    level: &Arc<AtomicU32>,
) -> AppResult<cpal::Stream> {
    let host = cpal::default_host();
    let device = select_device(&host, device_name)
        .ok_or_else(|| AppError::Audio("no input device available".to_string()))?;

    let supported = device
        .default_input_config()
        .map_err(|e| AppError::Audio(format!("default input config failed: {e}")))?;

    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    sample_rate.store(config.sample_rate, Ordering::Release);
    channels.store(config.channels as u32, Ordering::Release);

    let err_fn = |err| tracing::error!("audio stream error: {err}");

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

    Ok(stream)
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
