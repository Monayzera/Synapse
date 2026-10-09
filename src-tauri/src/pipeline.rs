use crate::cleanup::{FailureKind, LlmContext, Notice};
use crate::config::{LlmBackend, Settings, TranscriptionBackend};
use crate::error::{AppError, AppResult};
use crate::state::{AppState, LlmState, SharedState, Status};
use crate::{dictionary, history, inject};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

pub static CANCEL: AtomicBool = AtomicBool::new(false);
static RECORDING_GEN: AtomicU64 = AtomicU64::new(0);

const MAX_RECORDING: Duration = Duration::from_secs(600);
const LOCAL_LLM_WAIT: Duration = Duration::from_secs(3);

#[derive(Serialize, Clone)]
struct CompletePayload {
    raw_text: String,
    final_text: String,
    word_count: i64,
    duration_ms: u64,
    on_gpu: bool,
    llm_used: bool,
}

#[derive(Serialize, Clone)]
struct PipelineError {
    stage: String,
    code: String,
    message: String,
    params: Value,
    kind: &'static str,
}

struct Issue {
    code: &'static str,
    message: String,
    params: Value,
}

pub fn emit_error(app: &AppHandle, stage: &str, code: &str, message: &str) {
    emit_event(app, stage, code, message, json!({}), "error");
}

#[cfg(target_os = "linux")]
pub fn emit_error_with_params(app: &AppHandle, stage: &str, code: &str, message: &str, params: Value) {
    emit_event(app, stage, code, message, params, "error");
}

fn emit_issue(app: &AppHandle, stage: &str, issue: Issue) {
    emit_event(app, stage, issue.code, &issue.message, issue.params, "error");
}

fn emit_notice(app: &AppHandle, stage: &str, notice: Notice) {
    emit_event(app, stage, notice.code, &notice.message, notice.params, "info");
}

fn emit_event(app: &AppHandle, stage: &str, code: &str, message: &str, params: Value, kind: &'static str) {
    tracing::info!("pipeline-error [{stage}/{code}] {kind}: {message}");
    let payload = PipelineError {
        stage: stage.to_string(),
        code: code.to_string(),
        message: message.to_string(),
        params,
        kind,
    };
    if let Err(err) = app.emit("pipeline-error", payload) {
        tracing::warn!("pipeline-error emit failed: {err}");
    }
}

fn busy_error(app: &AppHandle) {
    emit_error(app, "busy", "busy", "Still processing the previous dictation.");
}

pub fn begin_recording(state: &SharedState) {
    if crate::updater::install_committed() {
        emit_error(
            &state.app,
            "update",
            "updating",
            "Synapse is installing an update; please wait.",
        );
        return;
    }
    if state.busy.load(Ordering::Acquire) {
        busy_error(&state.app);
        return;
    }
    if state.recording.swap(true, Ordering::AcqRel) {
        return;
    }
    if state.busy.load(Ordering::Acquire) {
        state.recording.store(false, Ordering::Release);
        busy_error(&state.app);
        return;
    }
    CANCEL.store(false, Ordering::Release);
    let backend = state.settings.read().transcription_backend;
    let not_ready: Option<(&str, String)> = match backend {
        TranscriptionBackend::Groq => {
            if state.settings.read().groq_api_key.trim().is_empty() {
                Some((
                    "groq_key_missing",
                    "Groq API key not set. Open Settings to add it.".to_string(),
                ))
            } else {
                None
            }
        }
        TranscriptionBackend::Local => {
            let meta = state.engine_meta.read();
            if meta.ready {
                None
            } else if meta.missing {
                Some((
                    "model_missing",
                    "Voice model not downloaded. Open Settings to download it.".to_string(),
                ))
            } else if let Some(other) = meta.error.as_deref() {
                Some(("engine_error", format!("Voice model failure: {other}")))
            } else {
                Some((
                    "engine_loading",
                    "The voice model is still loading, please wait.".to_string(),
                ))
            }
        }
    };
    if let Some((code, message)) = not_ready {
        state.recording.store(false, Ordering::Release);
        emit_error(&state.app, "engine", code, &message);
        return;
    }
    if let Some((code, message)) = crate::permissions::mic_block(state) {
        state.recording.store(false, Ordering::Release);
        emit_error(&state.app, "audio", code, message);
        return;
    }
    if !state.audio.is_available() {
        state.recording.store(false, Ordering::Release);
        state.audio.refresh();
        emit_error(
            &state.app,
            "audio",
            "mic_unavailable",
            "No working microphone was found; retrying automatically.",
        );
        return;
    }
    crate::sound::play(true);
    state.audio.start();
    state.set_status(Status::Recording);
    arm_max_duration(state);
}

fn arm_max_duration(state: &SharedState) {
    let generation = RECORDING_GEN.fetch_add(1, Ordering::AcqRel) + 1;
    let state = state.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(MAX_RECORDING).await;
        if RECORDING_GEN.load(Ordering::Acquire) != generation
            || !state.recording.load(Ordering::Acquire)
        {
            return;
        }
        tracing::warn!(
            "recording reached the maximum of {} s; stopping it",
            MAX_RECORDING.as_secs()
        );
        emit_error(
            &state.app,
            "recording",
            "max_duration",
            "Recording stopped automatically at the maximum duration.",
        );
        finish_recording(state.app.clone(), state.clone());
    });
}

pub fn cancel_recording(state: &SharedState) {
    CANCEL.store(true, Ordering::Release);
    if state.recording.swap(false, Ordering::AcqRel) {
        let _ = state.audio.stop();
        state.set_status(Status::Idle);
    }
}

pub fn finish_recording(app: AppHandle, state: SharedState) {
    if !state.recording.swap(false, Ordering::AcqRel) {
        return;
    }
    crate::sound::play(false);
    if state.busy.swap(true, Ordering::AcqRel) {
        let _ = state.audio.stop();
        busy_error(&app);
        return;
    }

    let captured = state.audio.stop();
    state.set_status(Status::Processing);

    tauri::async_runtime::spawn(async move {
        let _busy = BusyGuard(state.clone());
        if let Err(err) = run_pipeline(&app, &state, captured).await {
            tracing::error!("pipeline error: {err}");
            emit_error(&app, "pipeline", "transcription_failed", &err.to_string());
        }
    });
}

struct BusyGuard(SharedState);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
        self.0.set_status(Status::Idle);
    }
}

pub fn hotkey_toggle(app: AppHandle, state: SharedState) {
    if state.recording.load(Ordering::Acquire) {
        finish_recording(app, state);
    } else {
        begin_recording(&state);
    }
}

pub fn toggle_recording(app: AppHandle, state: SharedState) {
    if state.recording.load(Ordering::Acquire) {
        finish_recording(app, state);
    } else if state.busy.load(Ordering::Acquire) {
        cancel_recording(&state);
    } else {
        begin_recording(&state);
    }
}

async fn run_pipeline(
    app: &AppHandle,
    state: &SharedState,
    captured: crate::audio::CapturedAudio,
) -> AppResult<()> {
    let settings = state.settings_snapshot();
    let duration_ms = captured.duration_ms;
    let language = settings.language_code();
    let wants_translation = settings.translation_enabled;

    let backend = settings.transcription_backend;

    let prep_state = state.clone();
    let prep = tokio::task::spawn_blocking(move || -> AppResult<Option<Vec<f32>>> {
        if CANCEL.load(Ordering::Acquire) {
            return Ok(None);
        }
        let mono = captured.to_mono_16k();
        let peak = mono.iter().fold(0f32, |acc, s| acc.max(s.abs()));
        let mono_len = mono.len();
        let vad_start = std::time::Instant::now();
        let trimmed = maybe_trim(&prep_state, mono);
        tracing::info!(
            "pipeline audio: mono {} samples (peak {:.4}), after vad {} samples, vad took {} ms",
            mono_len,
            peak,
            trimmed.len(),
            vad_start.elapsed().as_millis()
        );
        if CANCEL.load(Ordering::Acquire) {
            return Ok(None);
        }
        Ok(Some(trimmed))
    })
    .await;

    let trimmed = match prep {
        Ok(Ok(Some(samples))) => samples,
        Ok(Ok(None)) => {
            let _ = app.emit("transcription-cancelled", ());
            return Ok(());
        }
        Ok(Err(err)) => return Err(err),
        Err(_) => {
            return Err(AppError::Transcribe(
                "audio preprocessing crashed".to_string(),
            ))
        }
    };

    let reserve_samples = if reserve_possible(&settings) {
        Some(trimmed.clone())
    } else {
        None
    };

    let (raw, on_gpu) = match backend {
        TranscriptionBackend::Groq => {
            let key = settings.groq_api_key.trim().to_string();
            if key.is_empty() {
                return Err(AppError::Transcribe(
                    "Groq API key not set. Open Settings to add it.".to_string(),
                ));
            }
            let prompt = state.vocabulary_prompt();
            let lang = language.clone();
            let samples = trimmed;
            let groq_start = std::time::Instant::now();
            let call = crate::groq::transcribe(
                state.llm.http(),
                &key,
                &samples,
                lang.as_deref(),
                prompt.as_deref(),
                false,
                &settings.groq_model,
            );
            tokio::pin!(call);
            let text = loop {
                tokio::select! {
                    out = &mut call => break out?,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(120)) => {
                        if CANCEL.load(Ordering::Acquire) {
                            let _ = app.emit("transcription-cancelled", ());
                            return Ok(());
                        }
                    }
                }
            };
            tracing::info!("groq took {} ms", groq_start.elapsed().as_millis());
            (text, false)
        }
        TranscriptionBackend::Local => {
            let blocking_state = state.clone();
            let lang_for_blocking = language.clone();
            let join = tokio::task::spawn_blocking(move || {
                if CANCEL.load(Ordering::Acquire) {
                    return Ok((String::new(), false));
                }
                let whisper_start = std::time::Instant::now();
                let result = blocking_state.transcribe_blocking(
                    &trimmed,
                    lang_for_blocking.as_deref(),
                );
                tracing::info!("whisper took {} ms", whisper_start.elapsed().as_millis());
                result
            })
            .await;
            match join {
                Ok(Ok(value)) => value,
                Ok(Err(err)) => return Err(err),
                Err(join_err) => {
                    if join_err.is_panic() {
                        tracing::error!("transcription panicked; reloading whisper on CPU");
                        let recovery = state.clone();
                        let _ =
                            tokio::task::spawn_blocking(move || recovery.load_engine(false)).await;
                    }
                    return Err(AppError::Transcribe(
                        "transcription crashed; switched to CPU mode".to_string(),
                    ));
                }
            }
        }
    };

    tracing::info!("whisper returned {} chars", raw.len());

    if CANCEL.load(Ordering::Acquire) {
        let _ = app.emit("transcription-cancelled", ());
        return Ok(());
    }

    if raw.trim().is_empty() {
        let _ = app.emit("transcription-empty", ());
        return Ok(());
    }

    let processed = dictionary::process(
        &raw,
        settings.filler_removal,
        &settings.filler_words,
        &settings.dictionary,
    );

    let want_llm = settings.llm_enabled || wants_translation;
    let stage = if wants_translation { "translation" } else { "ai" };
    let gate = if want_llm {
        llm_gate(state, &settings, wants_translation).await
    } else {
        LlmGate::Skip(None)
    };
    let (mut final_text, llm_used, mut llm_issue, mut notices) = match gate {
        LlmGate::Cancelled => {
            let _ = app.emit("transcription-cancelled", ());
            return Ok(());
        }
        LlmGate::Skip(issue) => (
            processed.clone(),
            false,
            issue.map(|(code, message)| Issue {
                code,
                message,
                params: json!({}),
            }),
            Vec::new(),
        ),
        LlmGate::Run => {
            let ctx = LlmContext {
                local_gpu: state.local_ai_gpu(),
            };
            let llm_start = std::time::Instant::now();
            let Some(mut outcome) = cancellable(state.llm.cleanup(&settings, &processed, &ctx)).await
            else {
                let _ = app.emit("transcription-cancelled", ());
                return Ok(());
            };
            let model_missing = matches!(settings.llm_backend, LlmBackend::Groq)
                && outcome
                    .error
                    .as_ref()
                    .is_some_and(|err| err.kind == FailureKind::ModelMissing);
            if model_missing {
                let failed = settings.groq_llm_model.clone();
                let Some(next) = cancellable(crate::groq::recover_model(state, &failed)).await
                else {
                    let _ = app.emit("transcription-cancelled", ());
                    return Ok(());
                };
                if let Some(model) = next {
                    let mut retry: Settings = settings.clone();
                    retry.groq_llm_model = model;
                    let Some(again) =
                        cancellable(state.llm.cleanup(&retry, &processed, &ctx)).await
                    else {
                        let _ = app.emit("transcription-cancelled", ());
                        return Ok(());
                    };
                    outcome = again;
                }
            }
            crate::groq::persist_learned(state);
            tracing::info!("llm cleanup took {} ms", llm_start.elapsed().as_millis());
            let issue = outcome.error.map(|err| Issue {
                code: err.code(),
                params: err.params(),
                message: err.message,
            });
            (outcome.text, outcome.applied, issue, outcome.notices)
        }
    };

    if use_whisper_reserve(&settings, llm_used, llm_issue.is_some()) {
        if let Some(samples) = reserve_samples.as_deref() {
            let reason = llm_issue.as_ref().map_or("llm_failed", |issue| issue.code);
            let key = settings.groq_api_key.trim().to_string();
            let reserve_start = std::time::Instant::now();
            let call = crate::groq::transcribe(
                state.llm.http(),
                &key,
                samples,
                None,
                None,
                true,
                crate::groq::TRANSLATE_MODEL,
            );
            let Some(result) = cancellable(call).await else {
                let _ = app.emit("transcription-cancelled", ());
                return Ok(());
            };
            match result {
                Ok(text) => {
                    let translated = dictionary::process(
                        &text,
                        settings.filler_removal,
                        &settings.filler_words,
                        &settings.dictionary,
                    );
                    if translated.trim().is_empty() {
                        tracing::warn!("whisper English translation returned no text; pasting untranslated");
                    } else {
                        tracing::info!(
                            "AI translation unavailable ({reason}); whisper translated to English in {} ms",
                            reserve_start.elapsed().as_millis()
                        );
                        final_text = translated;
                        llm_issue = None;
                        notices.push(Notice {
                            code: "translation_whisper_fallback",
                            message: "The AI translation was unavailable; Whisper translated the text to English.".to_string(),
                            params: json!({ "reason_code": reason }),
                        });
                    }
                }
                Err(err) => {
                    tracing::warn!("whisper English translation failed ({err}); pasting untranslated")
                }
            }
        }
    }

    if CANCEL.load(Ordering::Acquire) {
        let _ = app.emit("transcription-cancelled", ());
        return Ok(());
    }

    let inject_text = final_text.clone();
    let restore = settings.restore_clipboard;
    let delay = settings.paste_delay_ms;
    let inject_result =
        tokio::task::spawn_blocking(move || inject::inject_text(&inject_text, restore, delay)).await;

    match inject_result {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::warn!("injection failed: {err}");
            let code = if paste_needs_accessibility() {
                "accessibility_needed"
            } else if paste_input_denied() {
                "input_denied"
            } else {
                "inject_failed"
            };
            emit_error(
                app,
                "inject",
                code,
                &format!("{err}. {}", inject_block_hint()),
            );
        }
        Err(join_err) => {
            tracing::error!("injection task failed: {join_err}");
            if let Err(err) = inject::copy_to_clipboard(&final_text) {
                tracing::warn!("clipboard fallback failed: {err}");
            }
        }
    }

    let entry = history::NewEntry {
        created_at: now_millis(),
        duration_ms: duration_ms as i64,
        word_count: dictionary::word_count(&final_text),
        raw_text: raw.clone(),
        final_text: final_text.clone(),
        language: settings.language.clone(),
        on_gpu,
        llm_used,
        cloud: matches!(backend, TranscriptionBackend::Groq),
    };
    if let Err(err) = state.db_insert(&entry) {
        tracing::warn!("history insert failed: {err}");
    }

    let _ = app.emit(
        "transcription-complete",
        CompletePayload {
            raw_text: raw,
            final_text,
            word_count: entry.word_count,
            duration_ms,
            on_gpu,
            llm_used,
        },
    );

    for notice in notices {
        emit_notice(app, stage, notice);
    }
    if let Some(issue) = llm_issue {
        emit_issue(app, stage, issue);
    }

    Ok(())
}

fn reserve_possible(settings: &Settings) -> bool {
    settings.translation_to_english() && !settings.groq_api_key.trim().is_empty()
}

fn use_whisper_reserve(settings: &Settings, llm_used: bool, llm_failed: bool) -> bool {
    reserve_possible(settings) && !llm_used && llm_failed
}

async fn cancellable<F: std::future::Future>(future: F) -> Option<F::Output> {
    tokio::pin!(future);
    loop {
        tokio::select! {
            out = &mut future => return Some(out),
            _ = tokio::time::sleep(Duration::from_millis(120)) => {
                if CANCEL.load(Ordering::Acquire) {
                    return None;
                }
            }
        }
    }
}

enum LlmGate {
    Run,
    Skip(Option<(&'static str, String)>),
    Cancelled,
}

async fn llm_gate(state: &SharedState, settings: &Settings, wants_translation: bool) -> LlmGate {
    let not_configured = || {
        let message = if wants_translation {
            "Translation needs an AI provider; the text was pasted untranslated."
        } else {
            "AI correction needs an AI provider; the text was pasted without AI."
        };
        LlmGate::Skip(Some(("llm_not_configured", message.to_string())))
    };
    match settings.llm_backend {
        LlmBackend::Groq => {
            if settings.groq_llm_api_key.trim().is_empty() {
                LlmGate::Skip(Some((
                    "groq_llm_key_missing",
                    "Groq AI key not set; the text was pasted without AI.".to_string(),
                )))
            } else {
                LlmGate::Run
            }
        }
        LlmBackend::Local => {
            let deadline = tokio::time::Instant::now() + LOCAL_LLM_WAIT;
            loop {
                match state.local_llm_state() {
                    LlmState::Ready => return LlmGate::Run,
                    LlmState::Off => return not_configured(),
                    LlmState::Failed => {
                        return LlmGate::Skip(Some((
                            "llm_local_stopped",
                            "The local AI server is not running; the text was pasted without AI."
                                .to_string(),
                        )))
                    }
                    LlmState::Starting => {}
                }
                if CANCEL.load(Ordering::Acquire) {
                    return LlmGate::Cancelled;
                }
                if tokio::time::Instant::now() >= deadline {
                    tracing::info!("local AI server still warming up; pasting raw text");
                    return LlmGate::Skip(Some((
                        "llm_not_ready",
                        "The local AI is still starting; the text was pasted without AI."
                            .to_string(),
                    )));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        _ => {
            if settings.llm_endpoint.trim().is_empty() {
                not_configured()
            } else {
                LlmGate::Run
            }
        }
    }
}

fn maybe_trim(state: &AppState, mono: Vec<f32>) -> Vec<f32> {
    let settings = state.settings_snapshot();
    if !settings.vad_enabled {
        return mono;
    }
    let guard = state.vad.read();
    match guard.as_ref() {
        Some(vad) => vad.trim(
            &mono,
            settings.vad_threshold,
            settings.min_silence_ms,
            settings.speech_pad_ms,
        ),
        None => mono,
    }
}

fn inject_block_hint() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Grant Accessibility permission in System Settings to paste automatically; the text is on the clipboard."
    }
    #[cfg(target_os = "linux")]
    {
        "On Linux, allow Synapse to control the keyboard when the system asks; the text is on the clipboard."
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        "Elevated windows (run as administrator) block pasting; the text is on the clipboard."
    }
}

fn paste_needs_accessibility() -> bool {
    #[cfg(target_os = "macos")]
    {
        !crate::inject::accessibility_trusted()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

fn paste_input_denied() -> bool {
    #[cfg(target_os = "linux")]
    {
        crate::inject::take_input_denied()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translating(enabled: bool, target: &str, voice_key: &str) -> Settings {
        Settings {
            translation_enabled: enabled,
            translation_target: target.to_string(),
            groq_api_key: voice_key.to_string(),
            ..Settings::default()
        }
    }

    #[test]
    fn whisper_reserve_needs_english_target_and_voice_key() {
        assert!(reserve_possible(&translating(true, "English", "gsk_voice")));
        assert!(reserve_possible(&translating(true, "  english ", "gsk_voice")));
        assert!(!reserve_possible(&translating(false, "English", "gsk_voice")));
        assert!(!reserve_possible(&translating(true, "German", "gsk_voice")));
        assert!(!reserve_possible(&translating(true, "English", "   ")));
    }

    #[test]
    fn whisper_reserve_runs_only_after_the_llm_did_not_apply() {
        let english = translating(true, "English", "gsk_voice");
        assert!(use_whisper_reserve(&english, false, true));
        assert!(!use_whisper_reserve(&english, true, false));
        assert!(!use_whisper_reserve(&english, false, false));
        assert!(!use_whisper_reserve(&translating(true, "Spanish", "gsk_voice"), false, true));
        assert!(!use_whisper_reserve(&translating(true, "English", ""), false, true));
    }
}
