use crate::error::{AppError, AppResult};
use crate::state::SharedState;
use parking_lot::{Mutex, RwLock};
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::Emitter;

const TRANSCRIBE_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const TRANSLATE_URL: &str = "https://api.groq.com/openai/v1/audio/translations";
pub const TRANSLATE_MODEL: &str = "whisper-large-v3";
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(120);
const MODELS_URL: &str = "https://api.groq.com/openai/v1/models";
const MODELS_TIMEOUT: Duration = Duration::from_secs(10);
const CATALOG_FILE: &str = "groq_models.json";
const MODELS_EVENT: &str = "groq-models";
const EXCLUDED: [&str; 6] = ["whisper", "guard", "tts", "orpheus", "playai", "compound"];
const MIN_CONTEXT: u64 = 2048;
const AUTO_REFRESH: Duration = Duration::from_secs(6 * 60 * 60);
const LIMIT_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const LEARNED_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const MINUTE_HOLD: Duration = Duration::from_secs(60);
const DAILY_HOLD: Duration = Duration::from_secs(60 * 60);
const EFFORTS: [&str; 4] = ["none", "low", "medium", "high"];

pub const DEFAULT_LLM_MODEL: &str = "openai/gpt-oss-20b";
const PREFERRED: [&str; 3] = [DEFAULT_LLM_MODEL, "openai/gpt-oss-120b", "qwen/qwen3.8-27b"];

static CATALOG: RwLock<Vec<GroqModel>> = RwLock::new(Vec::new());
static UNAVAILABLE: Mutex<Vec<String>> = Mutex::new(Vec::new());
static LEARNED: Mutex<BTreeMap<String, LearnedParams>> = Mutex::new(BTreeMap::new());
static LIMITS: Mutex<Vec<GroqLimit>> = Mutex::new(Vec::new());
static HELD: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());
static WRITE_LOCK: Mutex<()> = Mutex::new(());
static DIRTY: AtomicBool = AtomicBool::new(false);
static LIMITS_CHANGED: AtomicBool = AtomicBool::new(false);
static AUTO_REFRESH_STARTED: AtomicBool = AtomicBool::new(false);

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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LearnedParams {
    pub effort: Option<String>,
    pub skip_include_reasoning: bool,
    pub no_reasoning_params: bool,
    pub at: i64,
}

impl LearnedParams {
    pub fn effort(&self) -> &str {
        self.effort.as_deref().unwrap_or(EFFORTS[0])
    }

    pub fn reasoning_stays_on(&self) -> bool {
        self.no_reasoning_params || self.effort() != EFFORTS[0]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroqLimit {
    pub model: String,
    pub kind: String,
    pub limit: u64,
    pub at: i64,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct CatalogFile {
    models: Vec<GroqModel>,
    learned: BTreeMap<String, LearnedParams>,
    limits: Vec<GroqLimit>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroqModels {
    pub models: Vec<GroqModel>,
    pub error: Option<String>,
    pub limits: Vec<GroqLimit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    Rpm,
    Rpd,
    Tpm,
    Tpd,
    Itpm,
    Otpm,
}

impl LimitKind {
    fn parse(tag: &str) -> Option<LimitKind> {
        match tag.to_ascii_uppercase().as_str() {
            "RPM" => Some(LimitKind::Rpm),
            "RPD" => Some(LimitKind::Rpd),
            "TPM" => Some(LimitKind::Tpm),
            "TPD" => Some(LimitKind::Tpd),
            "ITPM" => Some(LimitKind::Itpm),
            "OTPM" => Some(LimitKind::Otpm),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            LimitKind::Rpm => "rpm",
            LimitKind::Rpd => "rpd",
            LimitKind::Tpm => "tpm",
            LimitKind::Tpd => "tpd",
            LimitKind::Itpm => "itpm",
            LimitKind::Otpm => "otpm",
        }
    }

    pub fn daily(self) -> bool {
        matches!(self, LimitKind::Rpd | LimitKind::Tpd)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RateLimit {
    pub too_large: bool,
    pub kind: Option<LimitKind>,
    pub limit: Option<u64>,
    pub used: Option<u64>,
    pub requested: Option<u64>,
    pub wait: Option<Duration>,
}

impl RateLimit {
    pub fn daily(&self) -> bool {
        self.kind.is_some_and(LimitKind::daily)
    }
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
            Ok(file) => {
                let now = now_millis();
                *CATALOG.write() = normalized(file.models);
                *LEARNED.lock() = file.learned;
                *LIMITS.lock() = file.limits.into_iter().filter(|l| fresh(l, now)).collect();
            }
            Err(err) => tracing::warn!("groq model cache unreadable ({err}); ignoring it"),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!("groq model cache read failed: {err}"),
    }
    if !state.settings.read().groq_llm_api_key.trim().is_empty() {
        spawn_refresh(state);
    }
    spawn_auto_refresh(state);
}

pub fn spawn_refresh(state: &SharedState) {
    let state = state.clone();
    tauri::async_runtime::spawn(async move {
        refresh(&state).await;
    });
}

fn spawn_auto_refresh(state: &SharedState) {
    if AUTO_REFRESH_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let state = state.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(AUTO_REFRESH).await;
            let has_key = !state.settings.read().groq_llm_api_key.trim().is_empty();
            if has_key {
                tracing::info!("refreshing the groq model catalog (periodic)");
                refresh(&state).await;
            }
        }
    });
}

pub fn cached() -> GroqModels {
    GroqModels {
        models: CATALOG.read().clone(),
        error: None,
        limits: limits_snapshot(),
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
                UNAVAILABLE
                    .lock()
                    .retain(|id| !models.iter().any(|m| m.id == *id));
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
        limits: limits_snapshot(),
    };
    if let Err(err) = state.app.emit(MODELS_EVENT, payload.clone()) {
        tracing::warn!("groq-models emit failed: {err}");
    }
    payload
}

pub async fn recover_model(state: &SharedState, failed: &str) -> Option<String> {
    let failed = failed.trim().to_string();
    mark_unavailable(&failed);
    refresh(state).await;
    mark_unavailable(&failed);
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

fn mark_unavailable(id: &str) {
    let mut unavailable = UNAVAILABLE.lock();
    if !unavailable.iter().any(|known| known == id) {
        unavailable.push(id.to_string());
    }
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
        if let Some(code) = list_failure(status, &body) {
            let snippet: String = body.chars().take(200).collect();
            tracing::warn!("groq model list returned {status} ({code}): {snippet}");
            return Err(code.to_string());
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

fn list_failure(status: reqwest::StatusCode, body: &str) -> Option<&'static str> {
    if status == reqwest::StatusCode::FORBIDDEN && is_network_blocked(body) {
        Some("network_blocked")
    } else if status == reqwest::StatusCode::UNAUTHORIZED
        || status == reqwest::StatusCode::FORBIDDEN
    {
        Some("invalid_key")
    } else if !status.is_success() {
        Some("unavailable")
    } else {
        None
    }
}

fn store(state: &SharedState, models: &[GroqModel]) {
    let changed = {
        let mut catalog = CATALOG.write();
        let changed = catalog.as_slice() != models;
        *catalog = models.to_vec();
        changed
    };
    let pruned = prune_learned(models);
    let path = catalog_path(state);
    if changed {
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        tracing::info!("groq chat models: {}", ids.join(", "));
    }
    if changed || pruned || DIRTY.load(Ordering::Acquire) || !path.exists() {
        write_catalog(&path);
    }
}

fn write_catalog(path: &Path) {
    let _write = WRITE_LOCK.lock();
    DIRTY.store(false, Ordering::Release);
    let models = CATALOG.read().clone();
    let learned = LEARNED.lock().clone();
    let limits = LIMITS.lock().clone();
    let file = CatalogFile {
        models,
        learned,
        limits,
    };
    match serde_json::to_string_pretty(&file) {
        Ok(json) => {
            if let Err(err) = crate::atomic_io::write_durable(path, json.as_bytes()) {
                DIRTY.store(true, Ordering::Release);
                tracing::warn!("groq model cache write failed: {err}");
            }
        }
        Err(err) => tracing::warn!("groq model cache encode failed: {err}"),
    }
}

fn prune_learned(models: &[GroqModel]) -> bool {
    let listed = |id: &str| models.iter().any(|m| m.id == id);
    let now = now_millis();
    let learned_removed = {
        let mut learned = LEARNED.lock();
        let before = learned.len();
        learned.retain(|id, params| listed(id) && learned_fresh(params, now));
        before != learned.len()
    };
    let limits_removed = {
        let mut limits = LIMITS.lock();
        let before = limits.len();
        limits.retain(|l| listed(&l.model) && fresh(l, now));
        before != limits.len()
    };
    learned_removed || limits_removed
}

pub fn persist_learned(state: &SharedState) {
    if LIMITS_CHANGED.swap(false, Ordering::AcqRel) {
        if let Err(err) = state.app.emit(MODELS_EVENT, cached()) {
            tracing::warn!("groq-models emit failed: {err}");
        }
    }
    if !DIRTY.load(Ordering::Acquire) {
        return;
    }
    let path = catalog_path(state);
    let _ = tokio::task::spawn_blocking(move || write_catalog(&path));
}

pub fn learned(model: &str) -> LearnedParams {
    current_learned(&LEARNED.lock(), model.trim(), now_millis())
}

fn learned_fresh(params: &LearnedParams, now: i64) -> bool {
    now.saturating_sub(params.at) <= LEARNED_TTL_MS
}

fn current_learned(map: &BTreeMap<String, LearnedParams>, model: &str, now: i64) -> LearnedParams {
    map.get(model)
        .filter(|params| learned_fresh(params, now))
        .cloned()
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rejection {
    Ignored,
    Learned,
    Raced,
}

fn apply_rejection(
    map: &mut BTreeMap<String, LearnedParams>,
    model: &str,
    used: &LearnedParams,
    message: &str,
    now: i64,
) -> Rejection {
    let Some(mut next) = next_params(used, message) else {
        return Rejection::Ignored;
    };
    let current = current_learned(map, model, now);
    if current != *used {
        return Rejection::Raced;
    }
    if next == current {
        return Rejection::Ignored;
    }
    next.at = now;
    map.insert(model.to_string(), next);
    Rejection::Learned
}

pub fn learn_rejection(model: &str, used: &LearnedParams, message: &str) -> bool {
    let model = model.trim();
    let mut learned = LEARNED.lock();
    match apply_rejection(&mut learned, model, used, message, now_millis()) {
        Rejection::Ignored => false,
        Rejection::Raced => {
            tracing::info!(
                "groq model {model} reasoning parameters changed during the request; retrying with the current ones"
            );
            true
        }
        Rejection::Learned => {
            if let Some(next) = learned.get(model) {
                tracing::info!(
                    "groq model {model} rejected its reasoning parameters; now effort {}, include_reasoning {}, reasoning parameters {}",
                    next.effort(),
                    if next.skip_include_reasoning { "omitted" } else { "false" },
                    if next.no_reasoning_params { "off" } else { "on" }
                );
            }
            DIRTY.store(true, Ordering::Release);
            true
        }
    }
}

fn next_params(current: &LearnedParams, message: &str) -> Option<LearnedParams> {
    static ALLOWED: OnceLock<regex::Regex> = OnceLock::new();
    static VALUE: OnceLock<regex::Regex> = OnceLock::new();
    if current.no_reasoning_params {
        return None;
    }
    let drop_all = LearnedParams {
        no_reasoning_params: true,
        ..current.clone()
    };
    let allowed = ALLOWED.get_or_init(|| {
        regex::Regex::new(r"(?i)`reasoning_effort`\s+must be one of\s+(.+)").unwrap()
    });
    if let Some(list) = allowed.captures(message).and_then(|c| c.get(1)) {
        let value = VALUE.get_or_init(|| regex::Regex::new(r"`([A-Za-z_]+)`").unwrap());
        let offered: Vec<String> = value
            .captures_iter(list.as_str())
            .filter_map(|c| c.get(1))
            .map(|m| m.as_str().to_ascii_lowercase())
            .collect();
        let pick = EFFORTS
            .iter()
            .find(|effort| offered.iter().any(|o| o == *effort));
        return match pick {
            Some(effort) if *effort != current.effort() => Some(LearnedParams {
                effort: Some(effort.to_string()),
                ..current.clone()
            }),
            _ => Some(drop_all),
        };
    }
    let lower = message.to_ascii_lowercase();
    if lower.contains("include_reasoning") {
        if current.skip_include_reasoning {
            return Some(drop_all);
        }
        return Some(LearnedParams {
            skip_include_reasoning: true,
            ..current.clone()
        });
    }
    if lower.contains("reasoning") {
        return Some(drop_all);
    }
    None
}

pub fn record_limit(model: &str, limit: &RateLimit) {
    let model = model.trim();
    tracing::warn!(
        "groq model {model} hit a limit: {} {}, limit {:?}, used {:?}, requested {:?}, wait {:?}",
        if limit.too_large { "request too large" } else { "rate limit" },
        limit.kind.map_or("unknown", LimitKind::as_str),
        limit.limit,
        limit.used,
        limit.requested,
        limit.wait
    );
    if let (Some(kind), Some(value)) = (limit.kind, limit.limit) {
        let entry = GroqLimit {
            model: model.to_string(),
            kind: kind.as_str().to_string(),
            limit: value,
            at: now_millis(),
        };
        upsert_limit(&mut LIMITS.lock(), entry);
        DIRTY.store(true, Ordering::Release);
        LIMITS_CHANGED.store(true, Ordering::Release);
    }
    if !limit.too_large {
        let fallback = if limit.daily() { DAILY_HOLD } else { MINUTE_HOLD };
        hold_model(model, limit.wait.unwrap_or(fallback));
    }
}

fn upsert_limit(limits: &mut Vec<GroqLimit>, entry: GroqLimit) {
    let now = entry.at;
    limits.retain(|l| fresh(l, now) && !(l.model == entry.model && l.kind == entry.kind));
    limits.push(entry);
}

fn fresh(limit: &GroqLimit, now: i64) -> bool {
    now.saturating_sub(limit.at) <= LIMIT_TTL_MS
}

fn limits_snapshot() -> Vec<GroqLimit> {
    let now = now_millis();
    LIMITS.lock().iter().filter(|l| fresh(l, now)).cloned().collect()
}

pub fn output_cap(model: &str, prompt_tokens: u32) -> Option<u32> {
    let limits = LIMITS.lock();
    output_cap_from(&limits, model.trim(), prompt_tokens, now_millis())
}

fn output_cap_from(limits: &[GroqLimit], model: &str, prompt_tokens: u32, now: i64) -> Option<u32> {
    limits
        .iter()
        .filter(|l| l.model == model && fresh(l, now))
        .filter_map(|l| match l.kind.as_str() {
            "otpm" => Some(l.limit),
            "tpm" => Some(l.limit.saturating_sub(u64::from(prompt_tokens))),
            _ => None,
        })
        .map(|tokens| u32::try_from(tokens.saturating_mul(9) / 10).unwrap_or(u32::MAX))
        .min()
}

fn hold_model(model: &str, hold: Duration) {
    let now = Instant::now();
    let Some(until) = now.checked_add(hold) else {
        return;
    };
    let mut held = HELD.lock();
    held.retain(|(id, end)| *end > now && id != model);
    held.push((model.to_string(), until));
}

fn held_models() -> Vec<String> {
    let now = Instant::now();
    HELD.lock()
        .iter()
        .filter(|(_, end)| *end > now)
        .map(|(id, _)| id.clone())
        .collect()
}

pub fn fallback_model(failed: &str) -> Option<String> {
    let mut excluded = UNAVAILABLE.lock().clone();
    excluded.extend(held_models());
    excluded.push(failed.trim().to_string());
    let catalog = CATALOG.read();
    pick_fallback(&catalog, &excluded)
}

fn pick_fallback(models: &[GroqModel], exclude: &[String]) -> Option<String> {
    if models.is_empty() {
        return None;
    }
    pick_replacement(models, exclude)
}

pub fn label(id: &str) -> String {
    model_info(id)
        .map(|m| m.label)
        .unwrap_or_else(|| id.trim().to_string())
}

pub fn parse_rate_limit(message: &str, retry_after: Option<Duration>) -> RateLimit {
    static KIND: OnceLock<regex::Regex> = OnceLock::new();
    static NUMBER: OnceLock<regex::Regex> = OnceLock::new();
    static WAIT: OnceLock<regex::Regex> = OnceLock::new();
    let kind_re = KIND.get_or_init(|| {
        regex::Regex::new(r"(?i)\((RPM|RPD|TPM|TPD|ITPM|OTPM)\)").unwrap()
    });
    let number_re = NUMBER.get_or_init(|| {
        regex::Regex::new(r"(?i)\b(limit|used|requested)\s+(\d+)").unwrap()
    });
    let wait_re = WAIT.get_or_init(|| {
        regex::Regex::new(r"(?i)try again in\s+((?:\d+(?:\.\d+)?(?:ms|h|m|s))+)").unwrap()
    });
    let kind = kind_re
        .captures(message)
        .and_then(|c| c.get(1))
        .and_then(|m| LimitKind::parse(m.as_str()));
    let mut limit = None;
    let mut used = None;
    let mut requested = None;
    for caps in number_re.captures_iter(message) {
        let (Some(name), Some(value)) = (caps.get(1), caps.get(2)) else {
            continue;
        };
        let Ok(value) = value.as_str().parse::<u64>() else {
            continue;
        };
        let slot = match name.as_str().to_ascii_lowercase().as_str() {
            "limit" => &mut limit,
            "used" => &mut used,
            _ => &mut requested,
        };
        if slot.is_none() {
            *slot = Some(value);
        }
    }
    let wait = wait_re
        .captures(message)
        .and_then(|c| c.get(1))
        .and_then(|m| parse_wait(m.as_str()))
        .or(retry_after);
    RateLimit {
        too_large: message.to_ascii_lowercase().contains("request too large"),
        kind,
        limit,
        used,
        requested,
        wait,
    }
}

fn parse_wait(text: &str) -> Option<Duration> {
    static PART: OnceLock<regex::Regex> = OnceLock::new();
    let part = PART.get_or_init(|| regex::Regex::new(r"(\d+(?:\.\d+)?)(ms|h|m|s)").unwrap());
    let mut total = 0f64;
    let mut found = false;
    for caps in part.captures_iter(text) {
        let (Some(value), Some(unit)) = (caps.get(1), caps.get(2)) else {
            continue;
        };
        let Ok(value) = value.as_str().parse::<f64>() else {
            continue;
        };
        total += match unit.as_str() {
            "ms" => value / 1000.0,
            "h" => value * 3600.0,
            "m" => value * 60.0,
            _ => value,
        };
        found = true;
    }
    if !found {
        return None;
    }
    Duration::try_from_secs_f64(total).ok()
}

pub fn is_network_blocked(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("access denied") && lower.contains("network")
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
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
    let timeout =
        TRANSCRIBE_TIMEOUT.saturating_add(Duration::from_secs((samples.len() / 16000 / 4) as u64));
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
        .timeout(timeout)
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
        assert_eq!(DEFAULT_LLM_MODEL, "openai/gpt-oss-20b");
        assert_eq!(pick_replacement(&live, &skip(&["qwen/qwen3-32b"])).as_deref(), Some(DEFAULT_LLM_MODEL));
        assert_eq!(pick_replacement(&live, &skip(&[DEFAULT_LLM_MODEL])).as_deref(), Some("openai/gpt-oss-120b"));
        assert_eq!(
            pick_replacement(&live, &skip(&[DEFAULT_LLM_MODEL, "openai/gpt-oss-120b"])).as_deref(),
            Some("qwen/qwen3.8-27b")
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

    const OTPM_RATE: &str = "Rate limit reached for model `qwen/qwen3.8-27b` in organization `org_x` service tier `on_demand` on output tokens per minute (OTPM): Limit 1000, Used 72, Requested 981. Please try again in 3.179999999s. Need more tokens? ...";
    const OTPM_TOO_LARGE: &str = "Request too large for model `qwen/qwen3.8-27b` ... on output tokens per minute (OTPM): Limit 1000, Requested 1295. The request's expected output tokens exceed the enforced limit; reduce max_tokens ...";
    const TPM_TOO_LARGE: &str = "Request too large for model `qwen/qwen3.8-27b` in organization `org_x` service tier `on_demand` on tokens per minute (TPM): Limit 6000, Requested 18339, please reduce your message size and try again. Need more tokens? ...";
    const RPD_RATE: &str = "Rate limit reached for model `openai/gpt-oss-20b` in organization `org_x` service tier `on_demand` on requests per day (RPD): Limit 1000, Used 1000, Requested 1. Please try again in 1m26.4s. Need more tokens? ...";
    const EFFORT_REJECTED: &str = "`reasoning_effort` must be one of `low`, `medium`, or `high`";

    #[test]
    fn rate_limit_reached_otpm_is_parsed() {
        let parsed = parse_rate_limit(OTPM_RATE, Some(Duration::from_secs(4)));
        assert!(!parsed.too_large);
        assert_eq!(parsed.kind, Some(LimitKind::Otpm));
        assert_eq!(parsed.limit, Some(1000));
        assert_eq!(parsed.used, Some(72));
        assert_eq!(parsed.requested, Some(981));
        assert!(!parsed.daily());
        let wait = parsed.wait.unwrap();
        assert!((wait.as_secs_f64() - 3.179999999).abs() < 1e-6);
    }

    #[test]
    fn request_too_large_variants_are_parsed() {
        let otpm = parse_rate_limit(OTPM_TOO_LARGE, None);
        assert!(otpm.too_large);
        assert_eq!(otpm.kind, Some(LimitKind::Otpm));
        assert_eq!(otpm.limit, Some(1000));
        assert_eq!(otpm.used, None);
        assert_eq!(otpm.requested, Some(1295));
        assert_eq!(otpm.wait, None);
        let tpm = parse_rate_limit(TPM_TOO_LARGE, Some(Duration::from_secs(2)));
        assert!(tpm.too_large);
        assert_eq!(tpm.kind, Some(LimitKind::Tpm));
        assert_eq!(tpm.limit, Some(6000));
        assert_eq!(tpm.requested, Some(18339));
        assert_eq!(tpm.wait, Some(Duration::from_secs(2)));
    }

    #[test]
    fn daily_limits_are_detected() {
        let parsed = parse_rate_limit(RPD_RATE, None);
        assert_eq!(parsed.kind, Some(LimitKind::Rpd));
        assert!(parsed.daily());
        assert!((parsed.wait.unwrap().as_secs_f64() - 86.4).abs() < 1e-6);
        assert!(parse_rate_limit("on tokens per day (TPD): Limit 5, Used 5", None).daily());
        let unknown = parse_rate_limit("Too many requests", Some(Duration::from_secs(4)));
        assert_eq!(unknown.kind, None);
        assert_eq!(unknown.wait, Some(Duration::from_secs(4)));
        assert!(!unknown.daily());
    }

    #[test]
    fn waits_follow_the_go_duration_format() {
        assert_eq!(parse_wait("450ms"), Some(Duration::from_millis(450)));
        assert_eq!(parse_wait("1m2.5s"), Some(Duration::from_millis(62500)));
        assert_eq!(parse_wait("2h3m4s"), Some(Duration::from_secs(7384)));
        assert_eq!(parse_wait("soon"), None);
        assert_eq!(parse_wait(""), None);
    }

    #[test]
    fn network_block_is_not_an_invalid_key() {
        assert!(is_network_blocked("Access denied. Please check your network settings."));
        assert!(!is_network_blocked("Invalid API Key"));
        assert!(!is_network_blocked("Access denied for this model"));
    }

    #[test]
    fn model_list_403_network_block_is_not_an_invalid_key() {
        let blocked = r#"{"error":{"message":"Access denied. Please check your network settings."}}"#;
        assert_eq!(list_failure(reqwest::StatusCode::FORBIDDEN, blocked), Some("network_blocked"));
        assert_eq!(list_failure(reqwest::StatusCode::FORBIDDEN, r#"{"error":{"message":"Forbidden"}}"#), Some("invalid_key"));
        assert_eq!(list_failure(reqwest::StatusCode::UNAUTHORIZED, r#"{"error":{"message":"Invalid API Key"}}"#), Some("invalid_key"));
        assert_eq!(list_failure(reqwest::StatusCode::UNAUTHORIZED, blocked), Some("invalid_key"));
        assert_eq!(list_failure(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "oops"), Some("unavailable"));
        assert_eq!(list_failure(reqwest::StatusCode::OK, r#"{"data":[]}"#), None);
    }

    #[test]
    fn rejections_only_update_the_params_they_were_sent_with() {
        let now = 10 * LEARNED_TTL_MS;
        let mut map = BTreeMap::new();
        let fresh = LearnedParams::default();
        assert_eq!(apply_rejection(&mut map, "m", &fresh, EFFORT_REJECTED, now), Rejection::Learned);
        let low = map.get("m").cloned().unwrap();
        assert_eq!(low.effort.as_deref(), Some("low"));
        assert_eq!(low.at, now);
        assert_eq!(apply_rejection(&mut map, "m", &fresh, EFFORT_REJECTED, now + 1), Rejection::Raced);
        assert_eq!(map.get("m"), Some(&low));
        assert_eq!(apply_rejection(&mut map, "m", &fresh, "Please reduce the length of the messages", now + 1), Rejection::Ignored);
        assert_eq!(map.get("m"), Some(&low));
        assert_eq!(apply_rejection(&mut map, "m", &low, EFFORT_REJECTED, now + 2), Rejection::Learned);
        let off = map.get("m").cloned().unwrap();
        assert!(off.no_reasoning_params);
        assert_eq!(off.at, now + 2);
        assert_eq!(apply_rejection(&mut map, "m", &off, EFFORT_REJECTED, now + 3), Rejection::Ignored);
    }

    #[test]
    fn learned_params_expire_after_a_week() {
        let now = 100 * LEARNED_TTL_MS;
        let low = |at: i64| LearnedParams {
            effort: Some("low".to_string()),
            at,
            ..LearnedParams::default()
        };
        let mut map = BTreeMap::new();
        map.insert("recent".to_string(), low(now - LEARNED_TTL_MS));
        map.insert("stale".to_string(), low(now - LEARNED_TTL_MS - 1));
        map.insert("legacy".to_string(), low(0));
        assert_eq!(current_learned(&map, "recent", now), low(now - LEARNED_TTL_MS));
        assert_eq!(current_learned(&map, "stale", now), LearnedParams::default());
        assert_eq!(current_learned(&map, "legacy", now), LearnedParams::default());
        assert_eq!(current_learned(&map, "missing", now), LearnedParams::default());
        assert_eq!(apply_rejection(&mut map, "stale", &LearnedParams::default(), EFFORT_REJECTED, now), Rejection::Learned);
        assert_eq!(current_learned(&map, "stale", now), low(now));
        let legacy: CatalogFile = serde_json::from_str(r#"{"models":[],"learned":{"a/b":{"effort":"low","skip_include_reasoning":false,"no_reasoning_params":false}}}"#).unwrap();
        assert_eq!(legacy.learned.get("a/b").map(|p| p.at), Some(0));
        assert_eq!(legacy.learned.get("a/b").and_then(|p| p.effort.as_deref()), Some("low"));
    }

    #[test]
    fn reasoning_effort_learning_picks_the_lowest_offer() {
        let fresh = LearnedParams::default();
        assert_eq!(fresh.effort(), "none");
        assert!(!fresh.reasoning_stays_on());
        let low = next_params(&fresh, EFFORT_REJECTED).unwrap();
        assert_eq!(low.effort.as_deref(), Some("low"));
        assert!(!low.skip_include_reasoning);
        assert!(!low.no_reasoning_params);
        assert!(low.reasoning_stays_on());
        let stuck = next_params(&low, EFFORT_REJECTED).unwrap();
        assert!(stuck.no_reasoning_params);
        let medium = next_params(&fresh, "`reasoning_effort` must be one of `high` or `medium`").unwrap();
        assert_eq!(medium.effort.as_deref(), Some("medium"));
        let strange = next_params(&fresh, "`reasoning_effort` must be one of `minimal` or `max`").unwrap();
        assert!(strange.no_reasoning_params);
    }

    #[test]
    fn include_reasoning_and_unknown_rejections_degrade_safely() {
        let fresh = LearnedParams::default();
        let skip = next_params(&fresh, "property 'include_reasoning' is unsupported").unwrap();
        assert!(skip.skip_include_reasoning);
        assert!(!skip.no_reasoning_params);
        let both = next_params(&fresh, "cannot specify both `include_reasoning` and `reasoning_format`").unwrap();
        assert!(both.skip_include_reasoning);
        let again = next_params(&skip, "property 'include_reasoning' is unsupported").unwrap();
        assert!(again.no_reasoning_params);
        let unknown = next_params(&fresh, "`reasoning_format` is not supported with this model").unwrap();
        assert!(unknown.no_reasoning_params);
        assert!(unknown.reasoning_stays_on());
        assert_eq!(next_params(&fresh, "Please reduce the length of the messages"), None);
        assert_eq!(next_params(&unknown, EFFORT_REJECTED), None);
    }

    #[test]
    fn old_catalog_files_still_load() {
        let file: CatalogFile = serde_json::from_str(r#"{"models":[{"id":"a/b","label":"b"}]}"#).unwrap();
        assert_eq!(file.models.len(), 1);
        assert!(file.learned.is_empty());
        assert!(file.limits.is_empty());
        let mut learned = BTreeMap::new();
        learned.insert(
            "openai/gpt-oss-20b".to_string(),
            LearnedParams {
                effort: Some("low".to_string()),
                ..LearnedParams::default()
            },
        );
        let saved = CatalogFile {
            models: file.models,
            learned,
            limits: vec![GroqLimit {
                model: "qwen/qwen3.8-27b".to_string(),
                kind: "otpm".to_string(),
                limit: 1000,
                at: 5,
            }],
        };
        let json = serde_json::to_string(&saved).unwrap();
        let back: CatalogFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.learned.get("openai/gpt-oss-20b").and_then(|p| p.effort.clone()).as_deref(), Some("low"));
        assert_eq!(back.limits, saved.limits);
    }

    fn limit(model: &str, kind: &str, value: u64, at: i64) -> GroqLimit {
        GroqLimit {
            model: model.to_string(),
            kind: kind.to_string(),
            limit: value,
            at,
        }
    }

    #[test]
    fn limits_are_upserted_and_expire() {
        let day = 24 * 60 * 60 * 1000;
        let mut list = vec![
            limit("a", "otpm", 1000, 10 * day),
            limit("a", "rpd", 1000, 10 * day),
            limit("b", "tpm", 6000, 0),
        ];
        upsert_limit(&mut list, limit("a", "otpm", 800, 11 * day));
        assert_eq!(list.len(), 2);
        assert!(list.contains(&limit("a", "otpm", 800, 11 * day)));
        assert!(list.contains(&limit("a", "rpd", 1000, 10 * day)));
        assert!(!list.iter().any(|l| l.model == "b"));
        assert!(fresh(&limit("x", "rpm", 1, 0), LIMIT_TTL_MS));
        assert!(!fresh(&limit("x", "rpm", 1, 0), LIMIT_TTL_MS + 1));
    }

    #[test]
    fn learned_limits_cap_the_output() {
        let now = 1_000_000;
        let list = vec![
            limit("q", "otpm", 1000, now),
            limit("q", "rpd", 1000, now),
            limit("t", "tpm", 6000, now),
            limit("old", "otpm", 1000, now - LIMIT_TTL_MS - 1),
        ];
        assert_eq!(output_cap_from(&list, "q", 500, now), Some(900));
        assert_eq!(output_cap_from(&list, "t", 1000, now), Some(4500));
        assert_eq!(output_cap_from(&list, "t", 7000, now), Some(0));
        assert_eq!(output_cap_from(&list, "old", 10, now), None);
        assert_eq!(output_cap_from(&list, "missing", 10, now), None);
        let both = vec![limit("m", "otpm", 1000, now), limit("m", "tpm", 1200, now)];
        assert_eq!(output_cap_from(&both, "m", 400, now), Some(720));
    }

    #[test]
    fn fallback_needs_a_catalog_and_skips_excluded_models() {
        let live = parse_models(LIVE_LIST).unwrap();
        let skip = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        assert_eq!(pick_fallback(&[], &skip(&["x"])), None);
        assert_eq!(pick_fallback(&live, &skip(&["openai/gpt-oss-20b"])).as_deref(), Some("openai/gpt-oss-120b"));
        assert_eq!(
            pick_fallback(&live, &skip(&["openai/gpt-oss-20b", "openai/gpt-oss-120b"])).as_deref(),
            Some("qwen/qwen3.8-27b")
        );
        assert_eq!(
            pick_fallback(&live, &skip(&["openai/gpt-oss-20b", "openai/gpt-oss-120b", "qwen/qwen3.8-27b"])).as_deref(),
            Some("allam-2-7b")
        );
        assert_eq!(
            pick_fallback(&live, &skip(&["openai/gpt-oss-20b", "openai/gpt-oss-120b", "qwen/qwen3.8-27b", "allam-2-7b"])),
            None
        );
    }
}
