use crate::error::{AppError, AppResult};
use crate::hardware::GpuMark;
use crate::pipeline::CANCEL;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

static CRASH_PATHS: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
static ARMED: AtomicU32 = AtomicU32::new(0);

pub struct TranscribeEngine {
    context: WhisperContext,
    pub on_gpu: bool,
    pub backend: String,
    pub gpu_check: parking_lot::Mutex<Option<GpuMark>>,
}

pub fn set_crash_paths(pending: PathBuf, crashed: PathBuf) {
    if CRASH_PATHS.set((pending, crashed)).is_err() {
        tracing::warn!("whisper crash record paths were already set");
    }
}

pub fn mirror_armed(count: u32) {
    ARMED.store(count, Ordering::Release);
}

fn record_crash() -> bool {
    if ARMED.load(Ordering::Acquire) == 0 {
        return false;
    }
    match CRASH_PATHS.get() {
        Some((pending, crashed)) => std::fs::rename(pending, crashed).is_ok(),
        None => false,
    }
}

extern "C" fn log_ggml_abort(message: *const std::os::raw::c_char) {
    let recorded = record_crash();
    if recorded {
        tracing::error!("whisper graphics card crash recorded");
    }
    let text = if message.is_null() {
        "no reason given".to_string()
    } else {
        unsafe { std::ffi::CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned()
    };
    tracing::error!("whisper engine aborted the process: {text}");
}

fn install_abort_logger() {
    static ABORT_LOGGER: std::sync::Once = std::sync::Once::new();
    ABORT_LOGGER.call_once(|| {
        let callback: unsafe extern "C" fn(*const std::os::raw::c_char) = log_ggml_abort;
        unsafe {
            whisper_rs::whisper_rs_sys::ggml_set_abort_callback(Some(callback));
        }
    });
}

fn gpu_device_name() -> Option<String> {
    use whisper_rs::whisper_rs_sys as sys;
    unsafe {
        let count = sys::ggml_backend_dev_count();
        for index in 0..count {
            let device = sys::ggml_backend_dev_get(index);
            if device.is_null() {
                continue;
            }
            let kind = sys::ggml_backend_dev_type(device);
            if kind == sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_GPU
                || kind == sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_IGPU
            {
                let name = sys::ggml_backend_dev_name(device);
                if name.is_null() {
                    return Some("GPU".to_string());
                }
                return Some(
                    std::ffi::CStr::from_ptr(name)
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    None
}

impl TranscribeEngine {
    pub fn load(model_path: &Path, prefer_gpu: bool) -> AppResult<TranscribeEngine> {
        static SYSINFO_ONCE: std::sync::Once = std::sync::Once::new();
        SYSINFO_ONCE.call_once(|| {
            tracing::info!("whisper system_info: {}", whisper_rs::print_system_info());
        });
        if crate::hardware::whisper_gpu_guarded() {
            install_abort_logger();
        }
        match model_path.try_exists() {
            Ok(true) => {}
            Ok(false) => {
                return Err(AppError::Model(format!(
                    "whisper model not found: {}",
                    model_path.display()
                )))
            }
            Err(err) => {
                return Err(AppError::Io(format!(
                    "whisper model not accessible: {}: {err}",
                    model_path.display()
                )))
            }
        }
        let path_str = model_path.to_string_lossy().to_string();

        let gpu_capable = cfg!(feature = "cuda") || cfg!(feature = "metal") || cfg!(feature = "vulkan");
        if prefer_gpu && gpu_capable {
            match Self::try_load(&path_str, true) {
                Ok(context) => {
                    return Ok(match gpu_device_name() {
                        Some(device) => TranscribeEngine {
                            context,
                            on_gpu: true,
                            backend: format!("GPU ({device})"),
                            gpu_check: parking_lot::Mutex::new(None),
                        },
                        None => {
                            tracing::warn!(
                                "GPU whisper requested but no GPU device is available; running on CPU"
                            );
                            TranscribeEngine {
                                context,
                                on_gpu: false,
                                backend: "CPU".to_string(),
                                gpu_check: parking_lot::Mutex::new(None),
                            }
                        }
                    });
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
            backend: "CPU".to_string(),
            gpu_check: parking_lot::Mutex::new(None),
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
        params.set_translate(false);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_is_recorded_only_while_armed() {
        let dir = std::env::temp_dir().join(format!("synapse-whisper-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(std::fs::create_dir_all(&dir).is_ok());
        let pending = dir.join(crate::hardware::WHISPER_GPU_PENDING);
        let crashed = dir.join(crate::hardware::WHISPER_GPU_CRASHED);
        set_crash_paths(pending.clone(), crashed.clone());
        assert!(std::fs::write(&pending, b"{}").is_ok());
        mirror_armed(0);
        assert!(!record_crash());
        assert!(pending.exists());
        assert!(!crashed.exists());
        mirror_armed(1);
        assert!(record_crash());
        assert!(!pending.exists());
        assert!(crashed.exists());
        assert!(!record_crash());
        mirror_armed(0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
