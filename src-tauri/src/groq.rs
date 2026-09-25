use crate::error::{AppError, AppResult};
use crate::state::SharedState;
use parking_lot::{Mutex, RwLock};
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;
use tauri::Emitter;

const TRANSCRIBE_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const TRANSLATE_URL: &str = "https://api.groq.com/openai/v1/audio/translations";
const TRANSLATE_MODEL: &str = "whisper-large-v3";
const MODELS_URL: &str = "https://api.groq.com/openai/v1/models";
const MODELS_TIMEOUT: Duration = Duration::from_secs(10);
const CATALOG_FILE: &str = "groq_models.json";
const MODELS_EVENT: &str = "groq-models";
const EXCLUDED: [&str; 6] = ["whisper", "guard", "tts", "orpheus", "playai", "compound"];
const MIN_CONTEXT: u64 = 2048;

pub const DEFAULT_LLM_MODEL: &str = "qwen/qwen3.8-27b";
const PREFERRED: [&str; 3] = [DEFAULT_LLM_MODEL, "openai/gpt-oss-120b", "openai/gpt-oss-20b"];

static CATALOG: RwLock<Vec<GroqModel>> = RwLock::new(Vec::new());
static UNAVAILABLE: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[derive(Deserialize)]
struct GroqText {
    text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroqModel {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub context_window: Option<u32>,
    #[serde(default)]
    pub max_completion_tokens: Option<u32>,
    #[serde(default)]
    pub created: i64,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct CatalogFile {
    models: Vec<GroqModel>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroqModels {
    pub models: Vec<GroqModel>,
    pub error: Option<String>,
}

fn refresh_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn catalog_path(state: &SharedState) -> PathBuf {
    state.config_path.with_file_name(CATALOG_FILE)
}

pub fn init(state: &SharedState) {
    let path = catalog_path(state);
    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str::<CatalogFile>(&content) {
            Ok(file) => *CATALOG.write() = normalized(file.models),
            Err(err) => tracing::warn!("groq model cache unreadable ({err}); ignoring it"),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!("groq model cache read failed: {err}"),
    }
    if !state.settings.read().groq_llm_api_key.trim().is_empty() {
        spawn_refresh(state);
    }
}

pub fn spawn_refresh(state: &SharedState) {
    let state = state.clone();
    tauri::async_runtime::spawn(async move {
        refresh(&state).await;
    });
}

pub fn cached() -> GroqModels {
    GroqModels {
        models: CATALOG.read().clone(),
        error: None,
    }
}

pub fn model_info(id: &str) -> Option<GroqModel> {
    let id = id.trim();
    CATALOG.read().iter().find(|m| m.id == id).cloned()
}

pub fn is_model_missing(error: &str) -> bool {
    error.contains("model_not_found")
        || error.contains("model_decommissioned")
        || (error.contains("The model") && error.contains("decommissioned"))
}

pub async fn refresh(state: &SharedState) -> GroqModels {
    let _guard = refresh_lock().lock().await;
    let key = state.settings.read().groq_llm_api_key.trim().to_string();
    let error = if key.is_empty() {
        Some("key_missing".to_string())
    } else {
        match fetch(state.llm.http(), &key).await {
            Ok(models) => {
                let worker = state.clone();
                let saved = tokio::task::spawn_blocking(move || {
                    store(&worker, &models);
                    ensure_model(&worker, &models);
                })
                .await;
                if let Err(err) = saved {
                    tracing::warn!("groq model catalog update failed: {err}");
                }
                None
            }
            Err(code) => Some(code),
        }
    };
    let payload = GroqModels {
        models: CATALOG.read().clone(),
        error,
    };
    if let Err(err) = state.app.emit(MODELS_EVENT, payload.clone()) {
        tracing::warn!("groq-models emit failed: {err}");
    }
    payload
}

pub async fn recover_model(state: &SharedState, failed: &str) -> Option<String> {
    let failed = failed.trim().to_string();
    {
        let mut unavailable = UNAVAILABLE.lock();
        if !unavailable.contains(&failed) {
            unavailable.push(failed.clone());
        }
    }
    refresh(state).await;
    let current = state.settings.read().groq_llm_model.trim().to_string();
    if !current.is_empty() && !UNAVAILABLE.lock().contains(&current) {
        return Some(current);
    }
    let excluded = excluded_ids(&current);
    let next = pick_replacement(&CATALOG.read(), &excluded)?;
    let worker = state.clone();
    let target = next.clone();
    let switched = tokio::task::spawn_blocking(move || switch_model(&worker, &current, &target)).await;
    if let Err(err) = switched {
        tracing::warn!("groq AI model switch failed: {err}");
    }
    Some(next)
}

fn excluded_ids(current: &str) -> Vec<String> {
    let mut excluded = UNAVAILABLE.lock().clone();
    if !excluded.iter().any(|id| id == current) {
        excluded.push(current.to_string());
    }
    excluded
}

async fn fetch(http: &reqwest::Client, key: &str) -> Result<Vec<GroqModel>, String> {
    let request = async {
        let response = http
            .get(MODELS_URL)
            .bearer_auth(key)
            .send()
            .await
            .map_err(|err| {
                tracing::warn!("groq model list request failed: {err}");
                "network".to_string()
            })?;
        let status = response.status();
        let body = response.text().await.map_err(|err| {
            tracing::warn!("groq model list read failed: {err}");
            "network".to_string()
        })?;
        if status == reqwest::StatusCode::UNAUTHORIZED
            || status == reqwest::StatusCode::FORBIDDEN
        {
            return Err("invalid_key".to_string());
        }
        if !status.is_success() {
            let snippet: String = body.chars().take(200).collect();
            tracing::warn!("groq model list returned {status}: {snippet}");
            return Err("unavailable".to_string());
        }
        parse_models(&body).ok_or_else(|| {
            tracing::warn!("groq model list had no usable chat models");
            "unavailable".to_string()
        })
    };
    match tokio::time::timeout(MODELS_TIMEOUT, request).await {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!("groq model list timed out");
            Err("network".to_string())
        }
    }
}

fn store(state: &SharedState, models: &[GroqModel]) {
    let changed = {
        let mut catalog = CATALOG.write();
        let changed = catalog.as_slice() != models;
        *catalog = models.to_vec();
        changed
    };
    let path = catalog_path(state);
    if changed {
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        tracing::info!("groq chat models: {}", ids.join(", "));
    } else if path.exists() {
        return;
    }
    let file = CatalogFile {
        models: models.to_vec(),
    };
    match serde_json::to_string_pretty(&file) {
        Ok(json) => {
            if let Err(err) = crate::atomic_io::write_durable(&path, json.as_bytes()) {
                tracing::warn!("groq model cache write failed: {err}");
            }
        }
        Err(err) => tracing::warn!("groq model cache encode failed: {err}"),
    }
}

fn ensure_model(state: &SharedState, models: &[GroqModel]) {
    let current = state.settings.read().groq_llm_model.trim().to_string();
    if models.iter().any(|m| m.id == current) {
        return;
    }
    if let Some(next) = pick_replacement(models, &excluded_ids(&current)) {
        switch_model(state, &current, &next);
    }
}

fn switch_model(state: &SharedState, from: &str, to: &str) {
    let result = state.mutate_settings(|settings| {
        if settings.groq_llm_model.trim() == from {
            settings.groq_llm_model = to.to_string();
        }
        Ok(())
    });
    match result {
        Ok((old, new)) => {
            if old.groq_llm_model != new.groq_llm_model {
                tracing::info!("groq AI model '{from}' is gone; switched to '{to}'");
                state.emit_settings_changed();
            }
        }
        Err(err) => tracing::warn!("groq AI model switch not saved: {err}"),
    }
}

fn pick_replacement(models: &[GroqModel], exclude: &[String]) -> Option<String> {
    let allowed = |id: &str| !exclude.iter().any(|e| e == id);
    if models.is_empty() {
        return PREFERRED
            .iter()
            .find(|id| allowed(id))
            .map(|id| id.to_string());
    }
    PREFERRED
        .iter()
        .find(|id| allowed(id) && models.iter().any(|m| m.id == **id))
        .map(|id| id.to_string())
        .or_else(|| {
            models
                .iter()
                .filter(|m| allowed(&m.id))
                .max_by_key(|m| m.created)
                .map(|m| m.id.clone())
        })
}

fn parse_models(body: &str) -> Option<Vec<GroqModel>> {
    let value: Value = serde_json::from_str(body).ok()?;
    let models: Vec<GroqModel> = value
        .get("data")?
        .as_array()?
        .iter()
        .filter_map(chat_model)
        .collect();
    let models = normalized(models);
    if models.is_empty() {
        None
    } else {
        Some(models)
    }
}

fn normalized(mut models: Vec<GroqModel>) -> Vec<GroqModel> {
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    models.sort_by(|a, b| {
        a.label
            .to_lowercase()
            .cmp(&b.label.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    models
}

fn has(list: Option<&Value>, wanted: &str) -> Option<bool> {
    let items = list?.as_array()?;
    Some(items.iter().any(|v| v.as_str() == Some(wanted)))
}

fn chat_model(item: &Value) -> Option<GroqModel> {
    let id = item.get("id")?.as_str()?.trim();
    if id.is_empty() {
        return None;
    }
    if item.get("active").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    let lower = id.to_ascii_lowercase();
    if EXCLUDED.iter().any(|word| lower.contains(word)) {
        return None;
    }
    if has(item.get("output_modalities"), "text") == Some(false)
        || has(item.get("input_modalities"), "text") == Some(false)
    {
        return None;
    }
    let context = item.get("context_window").and_then(Value::as_u64);
    if context.is_some_and(|c| c < MIN_CONTEXT) {
        return None;
    }
    let to_u32 = |v: u64| u32::try_from(v).unwrap_or(u32::MAX);
    Some(GroqModel {
        id: id.to_string(),
        label: clean_label(item.get("name").and_then(Value::as_str), id),
        reasoning: has(item.get("supported_features"), "reasoning") == Some(true),
        context_window: context.map(to_u32),
        max_completion_tokens: item
            .get("max_completion_tokens")
            .and_then(Value::as_u64)
            .map(to_u32),
        created: item.get("created").and_then(Value::as_i64).unwrap_or(0),
    })
}

fn tail(text: &str) -> &str {
    text.rsplit('/').next().unwrap_or(text).trim()
}

fn clean_label(name: Option<&str>, id: &str) -> String {
    let named = name.map(tail).filter(|n| !n.is_empty());
    let short = named.unwrap_or_else(|| tail(id));
    if short.is_empty() {
        id.to_string()
    } else {
        short.to_string()
    }
}

pub async fn transcribe(
    http: &reqwest::Client,
    api_key: &str,
    samples: &[f32],
    language: Option<&str>,
    prompt: Option<&str>,
    translate: bool,
    model: &str,
) -> AppResult<String> {
    if samples.is_empty() {
        return Ok(String::new());
    }
    let wav = wav_16k_mono(samples);
    let (url, model) = if translate {
        (TRANSLATE_URL, TRANSLATE_MODEL)
    } else {
        (TRANSCRIBE_URL, model)
    };
    let part = Part::bytes(wav)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| AppError::Transcribe(e.to_string()))?;
    let mut form = Form::new()
        .part("file", part)
        .text("model", model.to_string())
        .text("response_format", "json")
        .text("temperature", "0");
    if !translate {
        if let Some(lang) = language {
            form = form.text("language", lang.to_string());
        }
    }
    if let Some(p) = prompt {
        if !p.trim().is_empty() {
            form = form.text("prompt", p.to_string());
        }
    }
    let response = http
        .post(url)
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| AppError::Transcribe(format!("Groq request failed: {e}")))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| AppError::Transcribe(e.to_string()))?;
    if !status.is_success() {
        let snippet: String = body.chars().take(300).collect();
        return Err(AppError::Transcribe(format!("Groq {status}: {snippet}")));
    }
    let parsed: GroqText = serde_json::from_str(&body)
        .map_err(|e| AppError::Transcribe(format!("Groq response parse failed: {e}")))?;
    Ok(parsed.text.trim().to_string())
}

fn wav_16k_mono(samples: &[f32]) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let mut buf = Vec::with_capacity(44 + data_len);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&16000u32.to_le_bytes());
    buf.extend_from_slice(&32000u32.to_le_bytes());
    buf.extend_from_slice(&2u16.to_le_bytes());
    buf.extend_from_slice(&16u16.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&(data_len as u32).to_le_bytes());
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        buf.extend_from_slice(&v.to_le_bytes());
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE_LIST: &str = r#"{"object":"list","data":[
{"id":"allam-2-7b","object":"model","created":1737672203,"owned_by":"SDAIA","active":true,"context_window":4096,"public_apps":null,"max_completion_tokens":4096,"name":"ALLaM-2-7b","input_modalities":["text"],"output_modalities":["text"],"supported_features":["json_mode"]},
{"id":"canopylabs/orpheus-v1-english","object":"model","created":1766186316,"owned_by":"Canopy Labs","active":true,"context_window":4000,"max_completion_tokens":50000,"name":"Canopy Labs Orpheus V1 English","input_modalities":["text"],"output_modalities":["speech"]},
{"id":"meta-llama/llama-prompt-guard-2-86m","object":"model","created":1748632165,"owned_by":"Meta","active":true,"context_window":512,"max_completion_tokens":512,"name":"Prompt Guard 2 86M","input_modalities":["text"],"output_modalities":["text"]},
{"id":"openai/gpt-oss-120b","object":"model","created":1754408224,"owned_by":"OpenAI","active":true,"context_window":131072,"max_completion_tokens":65536,"name":"GPT OSS 120B","input_modalities":["text"],"output_modalities":["text"],"supported_features":["tools","json_mode","structured_outputs","reasoning"]},
{"id":"openai/gpt-oss-20b","object":"model","created":1754407957,"owned_by":"OpenAI","active":true,"context_window":131072,"max_completion_tokens":65536,"name":"GPT OSS 20B","input_modalities":["text"],"output_modalities":["text"],"supported_features":["tools","json_mode","structured_outputs","reasoning"]},
{"id":"openai/gpt-oss-safeguard-20b","object":"model","created":1761708789,"owned_by":"OpenAI","active":true,"context_window":131072,"max_completion_tokens":65536,"name":"Safety GPT OSS 20B","input_modalities":["text"],"output_modalities":["text"],"supported_features":["reasoning"]},
{"id":"qwen/qwen3.8-27b","object":"model","created":1786984846,"owned_by":"Alibaba Cloud","active":true,"context_window":131072,"max_completion_tokens":16384,"name":"Qwen/Qwen3.8-27B","input_modalities":["text","image"],"output_modalities":["text"],"supported_features":["tools","json_mode","reasoning"]},
{"id":"whisper-large-v3","object":"model","created":1693721698,"owned_by":"OpenAI","active":true,"context_window":448,"max_completion_tokens":448,"name":"Whisper","input_modalities":["audio"],"output_modalities":["transcription"]},
{"id":"whisper-large-v3-turbo","object":"model","created":1728413088,"owned_by":"OpenAI","active":true,"context_window":448,"max_completion_tokens":448,"name":"Whisper Large V3 Turbo","input_modalities":["audio"],"output_modalities":["transcription"]}
]}"#;

    fn ids(models: &[GroqModel]) -> Vec<&str> {
        models.iter().map(|m| m.id.as_str()).collect()
    }

    fn model(id: &str, created: i64) -> GroqModel {
        GroqModel {
            id: id.to_string(),
            label: id.to_string(),
            reasoning: false,
            context_window: None,
            max_completion_tokens: None,
            created,
        }
    }

    #[test]
    fn live_list_keeps_only_chat_models() {
        let models = parse_models(LIVE_LIST).unwrap();
        assert_eq!(
            ids(&models),
            vec!["allam-2-7b", "openai/gpt-oss-120b", "openai/gpt-oss-20b", "qwen/qwen3.8-27b"]
        );
    }

    #[test]
    fn live_list_reads_labels_and_features() {
        let models = parse_models(LIVE_LIST).unwrap();
        let labels: Vec<&str> = models.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(labels, vec!["ALLaM-2-7b", "GPT OSS 120B", "GPT OSS 20B", "Qwen3.8-27B"]);
        let qwen = models.iter().find(|m| m.id == "qwen/qwen3.8-27b").unwrap();
        assert!(qwen.reasoning);
        assert_eq!(qwen.max_completion_tokens, Some(16384));
        let allam = models.iter().find(|m| m.id == "allam-2-7b").unwrap();
        assert!(!allam.reasoning);
        assert_eq!(allam.context_window, Some(4096));
    }

    #[test]
    fn minimal_entries_are_accepted() {
        let models = parse_models(r#"{"data":[{"id":"vendor/new-model-9b"},{"id":"x","active":false},{"id":""}]}"#).unwrap();
        assert_eq!(ids(&models), vec!["vendor/new-model-9b"]);
        assert_eq!(models[0].label, "new-model-9b");
        assert!(!models[0].reasoning);
    }

    #[test]
    fn distilled_chat_models_are_kept_and_duplicates_removed() {
        let models = parse_models(r#"{"data":[{"id":"deepseek-r1-distill-llama-70b","name":"B"},{"id":"a/m","name":"Z"},{"id":"a/m","name":"A"}]}"#).unwrap();
        assert_eq!(ids(&models), vec!["deepseek-r1-distill-llama-70b", "a/m"]);
    }

    #[test]
    fn unusable_payloads_are_rejected() {
        assert!(parse_models("not json").is_none());
        assert!(parse_models(r#"{"data":[]}"#).is_none());
        assert!(parse_models(r#"{"data":[{"id":"whisper-large-v3"}]}"#).is_none());
        assert!(parse_models(r#"{"error":{"message":"Invalid API Key"}}"#).is_none());
    }

    #[test]
    fn replacement_follows_preference_then_newest() {
        let live = parse_models(LIVE_LIST).unwrap();
        let skip = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        assert_eq!(pick_replacement(&live, &skip(&["qwen/qwen3-32b"])).as_deref(), Some(DEFAULT_LLM_MODEL));
        assert_eq!(pick_replacement(&live, &skip(&[DEFAULT_LLM_MODEL])).as_deref(), Some("openai/gpt-oss-120b"));
        assert_eq!(
            pick_replacement(&live, &skip(&[DEFAULT_LLM_MODEL, "openai/gpt-oss-120b"])).as_deref(),
            Some("openai/gpt-oss-20b")
        );
        let others = vec![model("old-model", 10), model("new-model", 20)];
        assert_eq!(pick_replacement(&others, &skip(&["gone"])).as_deref(), Some("new-model"));
        assert_eq!(pick_replacement(&others, &skip(&["new-model"])).as_deref(), Some("old-model"));
        assert_eq!(pick_replacement(&others, &skip(&["new-model", "old-model"])), None);
        assert_eq!(pick_replacement(&[], &skip(&["gone"])).as_deref(), Some(DEFAULT_LLM_MODEL));
        assert_eq!(pick_replacement(&[], &skip(&[DEFAULT_LLM_MODEL])).as_deref(), Some("openai/gpt-oss-120b"));
    }

    #[test]
    fn missing_model_errors_are_detected() {
        assert!(is_model_missing(r#"llm error: 404 Not Found: {"error":{"message":"The model `qwen/qwen3-32b` does not exist or you do not have access to it.","type":"invalid_request_error","code":"model_not_found"}}"#));
        assert!(is_model_missing(r#"400 Bad Request: {"error":{"code":"model_decommissioned"}}"#));
        assert!(is_model_missing(r#"400 Bad Request: {"error":{"message":"The model `x` has been decommissioned and is no longer supported.","code":"model_decommissi"#));
        assert!(!is_model_missing(r#"400 Bad Request: {"error":{"message":"the `functions` parameter is decommissioned"}}"#));
        assert!(!is_model_missing("the AI request timed out"));
        assert!(!is_model_missing(r#"429 Too Many Requests: {"error":{"code":"rate_limit_exceeded"}}"#));
    }

    #[test]
    fn labels_fall_back_to_the_id() {
        assert_eq!(clean_label(Some("Qwen/Qwen3.8-27B"), "qwen/qwen3.8-27b"), "Qwen3.8-27B");
        assert_eq!(clean_label(Some("  "), "openai/gpt-oss-20b"), "gpt-oss-20b");
        assert_eq!(clean_label(None, "allam-2-7b"), "allam-2-7b");
        assert_eq!(clean_label(Some("trailing/"), "vendor/id"), "id");
    }
}
