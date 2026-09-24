use crate::error::{AppError, AppResult};
use std::path::Path;

const WINDOW: usize = 512;
const CONTEXT: usize = 64;
const FRAME: usize = WINDOW + CONTEXT;
const SR: i64 = 16000;
const SILENCE_PEAK: f32 = 0.02;

pub struct Vad {
    #[cfg(feature = "vad")]
    session: parking_lot::Mutex<ort::session::Session>,
}

impl Vad {
    #[cfg(feature = "vad")]
    pub fn load(model_path: &Path) -> AppResult<Vad> {
        if !model_path.exists() {
            return Err(AppError::Vad(format!(
                "silero model not found: {}",
                model_path.display()
            )));
        }
        let session = ort::session::Session::builder()
            .map_err(|e| AppError::Vad(e.to_string()))?
            .commit_from_file(model_path)
            .map_err(|e| AppError::Vad(e.to_string()))?;
        Ok(Vad {
            session: parking_lot::Mutex::new(session),
        })
    }

    #[cfg(not(feature = "vad"))]
    pub fn load(_model_path: &Path) -> AppResult<Vad> {
        Err(AppError::Vad("vad feature disabled".to_string()))
    }

    pub fn trim(
        &self,
        samples: &[f32],
        threshold: f32,
        min_silence_ms: u32,
        speech_pad_ms: u32,
    ) -> Vec<f32> {
        let probs = match self.speech_probs(samples) {
            Ok(probs) => probs,
            Err(err) => {
                tracing::warn!("vad trim skipped ({err})");
                return samples.to_vec();
            }
        };

        let peak = samples.iter().fold(0f32, |acc, s| acc.max(s.abs()));
        let max_prob = probs.iter().copied().fold(0f32, f32::max);

        let speech: Vec<usize> = probs
            .iter()
            .enumerate()
            .filter(|(_, p)| **p >= threshold)
            .map(|(i, _)| i)
            .collect();
        tracing::info!(
            "vad: {} windows, max_prob {:.3}, threshold {:.2}, {} above, peak {:.3}",
            probs.len(),
            max_prob,
            threshold,
            speech.len(),
            peak
        );
        if speech.is_empty() {
            if peak < SILENCE_PEAK {
                return Vec::new();
            }
            tracing::warn!(
                "vad found no speech but audio peak is {peak:.3}; keeping full audio"
            );
            return samples.to_vec();
        }

        let pad_samples = speech_pad_ms as usize * 16;
        let gap_windows = ((min_silence_ms as usize * 16 + WINDOW - 1) / WINDOW).max(1);

        let mut segments: Vec<(usize, usize)> = Vec::new();
        let mut seg_start = speech[0];
        let mut seg_end = speech[0];
        for &window in speech.iter().skip(1) {
            if window - seg_end <= gap_windows {
                seg_end = window;
            } else {
                segments.push((seg_start, seg_end));
                seg_start = window;
                seg_end = window;
            }
        }
        segments.push((seg_start, seg_end));

        let mut out = Vec::with_capacity(samples.len());
        let mut last_to = 0usize;
        for (start_window, end_window) in segments {
            let speech_from = (start_window * WINDOW).min(samples.len());
            let speech_to = ((end_window + 1) * WINDOW).min(samples.len());
            let mut from = speech_from.saturating_sub(pad_samples);
            let to = (speech_to + pad_samples).min(samples.len());
            if from < last_to {
                from = last_to;
            }
            if to > from {
                out.extend_from_slice(&samples[from..to]);
                last_to = to;
            }
        }

        if peak >= SILENCE_PEAK && out.len() * 5 < samples.len() {
            tracing::warn!(
                "vad kept only {} of {} samples on loud audio; using full audio",
                out.len(),
                samples.len()
            );
            return samples.to_vec();
        }

        out
    }

    #[cfg(feature = "vad")]
    fn speech_probs(&self, samples: &[f32]) -> AppResult<Vec<f32>> {
        use ort::value::Tensor;

        let mut session = self.session.lock();
        let mut state = vec![0f32; 2 * 1 * 128];
        let mut context = vec![0f32; CONTEXT];
        let mut probs = Vec::with_capacity(samples.len() / WINDOW + 1);

        let mut offset = 0;
        while offset < samples.len() {
            let end = (offset + WINDOW).min(samples.len());
            let mut frame = vec![0f32; FRAME];
            frame[..CONTEXT].copy_from_slice(&context);
            frame[CONTEXT..CONTEXT + (end - offset)].copy_from_slice(&samples[offset..end]);
            context.copy_from_slice(&frame[WINDOW..FRAME]);
            offset += WINDOW;

            let input = Tensor::from_array(([1usize, FRAME], frame))
                .map_err(|e| AppError::Vad(e.to_string()))?;
            let state_tensor = Tensor::from_array(([2usize, 1usize, 128usize], state.clone()))
                .map_err(|e| AppError::Vad(e.to_string()))?;
            let sr_tensor =
                Tensor::from_array(([1usize], vec![SR])).map_err(|e| AppError::Vad(e.to_string()))?;

            let outputs = session
                .run(ort::inputs![
                    "input" => input,
                    "state" => state_tensor,
                    "sr" => sr_tensor,
                ])
                .map_err(|e| AppError::Vad(e.to_string()))?;

            let prob = outputs["output"]
                .try_extract_tensor::<f32>()
                .map_err(|e| AppError::Vad(e.to_string()))?
                .1
                .first()
                .copied()
                .unwrap_or(0.0);
            probs.push(prob);

            let new_state = outputs["stateN"]
                .try_extract_tensor::<f32>()
                .map_err(|e| AppError::Vad(e.to_string()))?
                .1
                .to_vec();
            if new_state.len() == state.len() {
                state = new_state;
            }
        }

        Ok(probs)
    }

    #[cfg(not(feature = "vad"))]
    fn speech_probs(&self, _samples: &[f32]) -> AppResult<Vec<f32>> {
        Err(AppError::Vad("vad feature disabled".to_string()))
    }
}
