use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::f32::consts::PI;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub fn play(start: bool) {
    let spawned = std::thread::Builder::new()
        .name("synapse-cue".to_string())
        .spawn(move || {
            let _ = play_tone(start);
        });
    if let Err(err) = spawned {
        tracing::warn!("sound cue thread could not start: {err}");
    }
}

fn play_tone(start: bool) -> Result<(), ()> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or(())?;
    let supported = device.default_output_config().map_err(|_| ())?;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let sample_rate = config.sample_rate.max(1) as f32;
    let channels = config.channels.max(1) as usize;

    let samples = Arc::new(generate(start, sample_rate));
    let total = samples.len();
    if total == 0 {
        return Ok(());
    }
    let pos = Arc::new(AtomicUsize::new(0));
    let err_fn = |err| tracing::debug!("cue output error: {err}");

    let stream = match sample_format {
        cpal::SampleFormat::F32 => {
            let s = samples.clone();
            let p = pos.clone();
            device.build_output_stream(
                config.clone(),
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    fill(data, &s, &p, channels, |v| v);
                },
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let s = samples.clone();
            let p = pos.clone();
            device.build_output_stream(
                config.clone(),
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    fill(data, &s, &p, channels, |v| {
                        (v.clamp(-1.0, 1.0) * 32767.0) as i16
                    });
                },
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let s = samples.clone();
            let p = pos.clone();
            device.build_output_stream(
                config.clone(),
                move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                    fill(data, &s, &p, channels, |v| {
                        ((v.clamp(-1.0, 1.0) * 32767.0) as i32 + 32768) as u16
                    });
                },
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::I32 => {
            let s = samples.clone();
            let p = pos.clone();
            device.build_output_stream(
                config.clone(),
                move |data: &mut [i32], _: &cpal::OutputCallbackInfo| {
                    fill(data, &s, &p, channels, |v| {
                        (v.clamp(-1.0, 1.0) * 2_147_483_647.0) as i32
                    });
                },
                err_fn,
                None,
            )
        }
        _ => return Err(()),
    }
    .map_err(|_| ())?;

    stream.play().map_err(|_| ())?;

    let duration_ms = (total as f32 / sample_rate * 1000.0) as u64 + 80;
    std::thread::sleep(Duration::from_millis(duration_ms));
    drop(stream);
    Ok(())
}

fn fill<T, F>(data: &mut [T], samples: &Arc<Vec<f32>>, pos: &Arc<AtomicUsize>, channels: usize, conv: F)
where
    T: Copy,
    F: Fn(f32) -> T,
{
    let mut cursor = pos.load(Ordering::Acquire);
    for frame in data.chunks_mut(channels.max(1)) {
        let value = samples.get(cursor).copied().unwrap_or(0.0);
        let converted = conv(value);
        for slot in frame.iter_mut() {
            *slot = converted;
        }
        cursor += 1;
    }
    pos.store(cursor, Ordering::Release);
}

fn generate(start: bool, sample_rate: f32) -> Vec<f32> {
    let notes: [(f32, f32); 2] = if start {
        [(523.25, 0.07), (659.25, 0.09)]
    } else {
        [(659.25, 0.07), (523.25, 0.10)]
    };
    let mut out = Vec::new();
    for (freq, dur) in notes {
        let count = (dur * sample_rate) as usize;
        for i in 0..count {
            let t = i as f32 / sample_rate;
            let wave = (2.0 * PI * freq * t).sin();
            out.push(wave * 0.045 * envelope(i, count));
        }
    }
    out
}

fn envelope(i: usize, count: usize) -> f32 {
    if count == 0 {
        return 0.0;
    }
    let pos = i as f32 / count as f32;
    if pos < 0.3 {
        let x = pos / 0.3;
        x * x
    } else if pos > 0.55 {
        let x = (1.0 - pos) / 0.45;
        (x * x).max(0.0)
    } else {
        1.0
    }
}
