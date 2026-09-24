use crate::error::{AppError, AppResult};
use crate::pipeline::CANCEL;
use std::path::Path;
use std::sync::atomic::Ordering;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub struct TranscribeEngine {
    context: WhisperContext,
    pub on_gpu: bool,
}

impl TranscribeEngine {
    pub fn load(model_path: &Path, prefer_gpu: bool) -> AppResult<TranscribeEngine> {
        static SYSINFO_ONCE: std::sync::Once = std::sync::Once::new();
        SYSINFO_ONCE.call_once(|| {
            tracing::info!("whisper system_info: {}", whisper_rs::print_system_info());
        });
        if !model_path.exists() {
            return Err(AppError::Model(format!(
                "whisper model not found: {}",
                model_path.display()
            )));
        }
        let path_str = model_path.to_string_lossy().to_string();

        let gpu_capable = cfg!(feature = "cuda") || cfg!(feature = "metal");
        if prefer_gpu && gpu_capable {
            match Self::try_load(&path_str, true) {
                Ok(context) => {
                    return Ok(TranscribeEngine {
                        context,
                        on_gpu: true,
                    })
                }
                Err(err) => {
                    tracing::warn!("GPU whisper init failed ({err}); falling back to CPU");
                }
            }
        }

        let context = Self::try_load(&path_str, false)?;
        Ok(TranscribeEngine {
            context,
            on_gpu: false,
        })
    }

    fn try_load(path: &str, use_gpu: bool) -> AppResult<WhisperContext> {
        let mut params = WhisperContextParameters::default();
        params.use_gpu(use_gpu);
        WhisperContext::new_with_params(path, params)
            .map_err(|e| AppError::Transcribe(format!("whisper init failed: {e}")))
    }

    pub fn transcribe(
        &self,
        samples: &[f32],
        language: Option<&str>,
        n_threads: i32,
        initial_prompt: Option<&str>,
        translate: bool,
    ) -> AppResult<String> {
        if samples.is_empty() {
            return Ok(String::new());
        }

        let mut state = self
            .context
            .create_state()
            .map_err(|e| AppError::Transcribe(format!("create state failed: {e}")))?;

        let strategy = if self.on_gpu {
            SamplingStrategy::BeamSearch {
                beam_size: 5,
                patience: -1.0,
            }
        } else {
            SamplingStrategy::Greedy { best_of: 1 }
        };
        let mut params = FullParams::new(strategy);
        params.set_n_threads(n_threads.max(1));
        params.set_translate(translate);
        params.set_language(language);
        params.set_temperature(0.0);
        params.set_temperature_inc(0.0);
        params.set_no_context(true);
        params.set_n_max_text_ctx(64);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        params.set_single_segment(false);
        params.set_token_timestamps(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        if let Some(prompt) = initial_prompt {
            if !prompt.trim().is_empty() {
                params.set_initial_prompt(prompt);
            }
        }
        params.set_abort_callback_safe(|| CANCEL.load(Ordering::Relaxed));

        if let Err(err) = state.full(params, samples) {
            if CANCEL.load(Ordering::Relaxed) {
                return Ok(String::new());
            }
            return Err(AppError::Transcribe(format!("inference failed: {err}")));
        }

        if CANCEL.load(Ordering::Relaxed) {
            return Ok(String::new());
        }

        let segments = state.full_n_segments();

        let mut text = String::new();
        for index in 0..segments {
            if let Some(segment) = state.get_segment(index) {
                if let Ok(part) = segment.to_str_lossy() {
                    append_segment(&mut text, &part);
                }
            }
        }

        Ok(text.trim().to_string())
    }
}

fn append_segment(out: &mut String, segment: &str) {
    if segment.is_empty() {
        return;
    }
    if out.trim().is_empty() {
        out.push_str(segment);
        return;
    }
    if ends_sentence(out) {
        out.push_str(segment);
    } else {
        out.push_str(&lower_first_alpha(segment));
    }
}

fn ends_sentence(text: &str) -> bool {
    match text.trim_end().chars().last() {
        Some(c) => matches!(c, '.' | '!' | '?' | '\u{2026}' | ':' | '\n'),
        None => true,
    }
}

fn lower_first_alpha(segment: &str) -> String {
    let chars: Vec<char> = segment.chars().collect();
    let mut idx = 0;
    while idx < chars.len() && !chars[idx].is_alphabetic() {
        idx += 1;
    }
    if idx >= chars.len() || !chars[idx].is_uppercase() {
        return segment.to_string();
    }
    let next_is_upper_alpha = chars
        .get(idx + 1)
        .map(|c| c.is_alphabetic() && c.is_uppercase())
        .unwrap_or(false);
    if next_is_upper_alpha {
        return segment.to_string();
    }
    let mut result = String::with_capacity(segment.len());
    for (i, c) in chars.iter().enumerate() {
        if i == idx {
            result.extend(c.to_lowercase());
        } else {
            result.push(*c);
        }
    }
    result
}
