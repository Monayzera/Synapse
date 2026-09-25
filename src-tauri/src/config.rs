use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecordMode {
    PushToTalk,
    Toggle,
}

impl Default for RecordMode {
    fn default() -> Self {
        RecordMode::PushToTalk
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LlmBackend {
    Local,
    OpenAiCompatible,
    Anthropic,
    Ollama,
    Groq,
}

impl Default for LlmBackend {
    fn default() -> Self {
        LlmBackend::Local
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionBackend {
    Local,
    Groq,
}

impl Default for TranscriptionBackend {
    fn default() -> Self {
        TranscriptionBackend::Local
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub hotkey_ptt: String,
    pub record_mode: RecordMode,
    pub language: String,
    pub whisper_model: String,
    pub transcription_backend: TranscriptionBackend,
    pub groq_api_key: String,
    pub groq_model: String,
    pub groq_llm_model: String,
    pub groq_llm_api_key: String,
    pub audio_device: Option<String>,
    pub vad_enabled: bool,
    pub vad_threshold: f32,
    pub min_silence_ms: u32,
    pub speech_pad_ms: u32,
    pub filler_removal: bool,
    pub filler_words: Vec<String>,
    pub dictionary: BTreeMap<String, String>,
    pub vocabulary: Vec<String>,
    pub llm_enabled: bool,
    pub llm_format_paragraphs: bool,
    pub translation_enabled: bool,
    pub translation_target: String,
    pub llm_backend: LlmBackend,
    pub llm_local_model: String,
    pub llm_endpoint: String,
    pub llm_api_key: String,
    pub llm_model_name: String,
    pub llm_timeout_ms: u64,
    pub llm_temperature: f32,
    pub llm_gpu_layers: i32,
    pub autostart: bool,
    pub restore_clipboard: bool,
    pub paste_delay_ms: u64,
    pub prefer_gpu: bool,
    pub ui_language: String,
    pub auto_update: bool,
    #[serde(skip)]
    pub transient_unreadable: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            hotkey_ptt: "Ctrl+Shift+Space".to_string(),
            record_mode: RecordMode::PushToTalk,
            language: "auto".to_string(),
            whisper_model: "large-v3-turbo".to_string(),
            transcription_backend: TranscriptionBackend::Local,
            groq_api_key: String::new(),
            groq_model: "whisper-large-v3-turbo".to_string(),
            groq_llm_model: crate::groq::DEFAULT_LLM_MODEL.to_string(),
            groq_llm_api_key: String::new(),
            audio_device: None,
            vad_enabled: true,
            vad_threshold: 0.5,
            min_silence_ms: 200,
            speech_pad_ms: 120,
            filler_removal: true,
            filler_words: default_fillers(),
            dictionary: BTreeMap::new(),
            vocabulary: Vec::new(),
            llm_enabled: false,
            llm_format_paragraphs: false,
            translation_enabled: false,
            translation_target: "English".to_string(),
            llm_backend: LlmBackend::Local,
            llm_local_model: "google_gemma-3-4b-it-Q4_K_M.gguf".to_string(),
            llm_endpoint: "http://127.0.0.1:8123/v1".to_string(),
            llm_api_key: String::new(),
            llm_model_name: "local".to_string(),
            llm_timeout_ms: 2000,
            llm_temperature: 0.1,
            llm_gpu_layers: 99,
            autostart: false,
            restore_clipboard: true,
            paste_delay_ms: 120,
            prefer_gpu: true,
            ui_language: "auto".to_string(),
            auto_update: true,
            transient_unreadable: false,
        }
    }
}

fn default_fillers() -> Vec<String> {
    [
        "aham", "ahn", "hum", "humm", "uhm", "uh", "uhh", "hmm", "ééé", "éé", "eh", "er", "mmm",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

pub enum LoadOutcome {
    Loaded(Box<Settings>, bool),
    Missing,
    Corrupt,
    Unreadable,
}

enum ReadResult {
    Parsed(Box<Settings>, bool),
    Missing,
    Invalid(String),
    Transient,
}

fn path_present(path: &Path) -> bool {
    matches!(path.try_exists(), Ok(true))
}

fn parse_settings(content: &str) -> Result<(Settings, bool), serde_json::Error> {
    let raw: serde_json::Value = serde_json::from_str(content)?;
    let mut settings = Settings::deserialize(&raw)?;
    let migrated = settings.migrate_legacy(&raw);
    Ok((settings, migrated))
}

fn try_read_parse(path: &Path) -> ReadResult {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            if content.trim().is_empty() {
                ReadResult::Transient
            } else {
                match parse_settings(&content) {
                    Ok((settings, migrated)) => ReadResult::Parsed(Box::new(settings), migrated),
                    Err(err) => {
                        tracing::warn!("settings parse failed: {err}");
                        ReadResult::Invalid(content)
                    }
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => ReadResult::Missing,
        Err(err) => {
            tracing::warn!("settings read failed: {err}");
            ReadResult::Transient
        }
    }
}

fn read_with_backoff(path: &Path, budget: std::time::Duration) -> ReadResult {
    let deadline = std::time::Instant::now() + budget;
    let mut delay = std::time::Duration::from_millis(50);
    let max_delay = std::time::Duration::from_millis(1500);
    loop {
        match try_read_parse(path) {
            ReadResult::Transient => {}
            settled => return settled,
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return ReadResult::Transient;
        }
        std::thread::sleep(delay.min(deadline - now));
        delay = (delay * 2).min(max_delay);
    }
}

impl Settings {
    pub fn load(path: &Path) -> LoadOutcome {
        let present = path_present(path) || path_present(&bak_path(path));
        let budget = if present {
            std::time::Duration::from_secs(20)
        } else {
            std::time::Duration::from_millis(250)
        };
        Self::load_with_budget(path, budget, present)
    }

    fn load_with_budget(path: &Path, budget: std::time::Duration, present: bool) -> LoadOutcome {
        let bak = bak_path(path);
        let bak_budget = budget / 4;
        let primary = read_with_backoff(path, budget - bak_budget);
        if let ReadResult::Parsed(settings, migrated) = primary {
            return LoadOutcome::Loaded(settings, migrated);
        }
        if let ReadResult::Parsed(settings, migrated) = read_with_backoff(&bak, bak_budget) {
            tracing::warn!("settings.json unusable; recovered from settings.bak");
            return LoadOutcome::Loaded(settings, migrated);
        }
        match primary {
            ReadResult::Invalid(content) => {
                backup_corrupt(path, &content);
                LoadOutcome::Corrupt
            }
            ReadResult::Missing if !present => LoadOutcome::Missing,
            _ => {
                tracing::error!(
                    "settings present on disk but could not be read at startup; running on in-memory defaults WITHOUT persisting"
                );
                LoadOutcome::Unreadable
            }
        }
    }

    pub fn save(&self, path: &Path) -> AppResult<()> {
        if self.transient_unreadable {
            return Err(AppError::Config(
                "refusing to save: settings were not readable at startup".to_string(),
            ));
        }
        let json = serde_json::to_string_pretty(self)?;
        crate::atomic_io::write_durable(path, json.as_bytes())
            .map_err(|err| AppError::Config(err.to_string()))?;
        if let Err(err) = crate::atomic_io::write_durable(&bak_path(path), json.as_bytes()) {
            tracing::warn!("settings.bak write failed: {err}");
        }
        Ok(())
    }

    fn migrate_legacy(&mut self, raw: &serde_json::Value) -> bool {
        if !self.groq_llm_api_key.is_empty() || raw.get("groq_llm_api_key").is_some() {
            return false;
        }
        let reuse = raw
            .get("groq_reuse_transcription_key")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        if reuse {
            if self.groq_api_key.is_empty() {
                return false;
            }
            self.groq_llm_api_key = self.groq_api_key.clone();
            return true;
        }
        if self.llm_backend == LlmBackend::Groq && !self.llm_api_key.is_empty() {
            self.groq_llm_api_key = std::mem::take(&mut self.llm_api_key);
            return true;
        }
        false
    }

    pub fn merged_with(
        &self,
        patch: &serde_json::Map<String, serde_json::Value>,
    ) -> AppResult<Settings> {
        let mut value = serde_json::to_value(self)?;
        let fields = value
            .as_object_mut()
            .ok_or_else(|| AppError::Config("settings are not an object".to_string()))?;
        for (key, entry) in patch {
            if fields.contains_key(key) {
                fields.insert(key.clone(), entry.clone());
            }
        }
        let mut merged = Settings::deserialize(&value)
            .map_err(|err| AppError::Config(format!("invalid settings: {err}")))?;
        merged.clamp_ranges();
        merged.transient_unreadable = self.transient_unreadable;
        Ok(merged)
    }

    fn clamp_ranges(&mut self) {
        self.vad_threshold = self.vad_threshold.clamp(0.1, 0.9);
        self.speech_pad_ms = self.speech_pad_ms.clamp(0, 400);
        self.min_silence_ms = self.min_silence_ms.clamp(50, 1000);
        self.paste_delay_ms = self.paste_delay_ms.clamp(40, 1000);
        self.llm_timeout_ms = self.llm_timeout_ms.clamp(500, 20000);
        self.llm_temperature = self.llm_temperature.clamp(0.0, 1.0);
    }

    pub fn language_code(&self) -> Option<String> {
        match self.language.as_str() {
            "auto" | "" => None,
            other => Some(other.to_string()),
        }
    }

    pub fn whisper_translate(&self) -> bool {
        self.translation_enabled && self.translation_target.trim().eq_ignore_ascii_case("english")
    }
}

fn bak_path(path: &Path) -> std::path::PathBuf {
    path.with_extension("bak")
}

fn backup_corrupt(path: &Path, content: &str) {
    if content.is_empty() {
        return;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = path.with_extension(format!("corrupt-{stamp}.json"));
    if let Err(err) = std::fs::write(&backup, content) {
        tracing::warn!("failed to save corrupt settings backup: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    fn tmp_path() -> std::path::PathBuf {
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("synapse_cfg_{}_{n}.json", std::process::id()))
    }

    fn cleanup(p: &Path) {
        let _ = std::fs::remove_file(p);
        let _ = std::fs::remove_file(bak_path(p));
    }

    #[test]
    fn save_then_load_roundtrip() {
        let p = tmp_path();
        let mut s = Settings::default();
        s.groq_api_key = "gsk_test".to_string();
        s.llm_format_paragraphs = true;
        s.save(&p).unwrap();
        match Settings::load(&p) {
            LoadOutcome::Loaded(loaded, _) => {
                assert_eq!(loaded.groq_api_key, "gsk_test");
                assert!(loaded.llm_format_paragraphs);
            }
            _ => panic!("expected Loaded"),
        }
        cleanup(&p);
    }

    #[test]
    fn missing_file_is_missing() {
        let p = tmp_path();
        assert!(matches!(Settings::load(&p), LoadOutcome::Missing));
    }

    #[test]
    fn corrupt_without_backup_preserves_original() {
        let p = tmp_path();
        std::fs::write(&p, b"{ not valid json ").unwrap();
        assert!(matches!(Settings::load(&p), LoadOutcome::Corrupt));
        let still = std::fs::read_to_string(&p).unwrap();
        assert!(still.contains("not valid"));
        cleanup(&p);
    }

    #[test]
    fn corrupt_primary_recovers_from_backup() {
        let p = tmp_path();
        let mut s = Settings::default();
        s.groq_api_key = "from_bak".to_string();
        s.save(&p).unwrap();
        std::fs::write(&p, b"garbage").unwrap();
        match Settings::load(&p) {
            LoadOutcome::Loaded(loaded, _) => assert_eq!(loaded.groq_api_key, "from_bak"),
            _ => panic!("expected recovery from settings.bak"),
        }
        cleanup(&p);
    }

    #[test]
    fn empty_file_is_unreadable_not_missing() {
        let p = tmp_path();
        std::fs::write(&p, b"   ").unwrap();
        let budget = std::time::Duration::from_millis(150);
        assert!(matches!(
            Settings::load_with_budget(&p, budget, true),
            LoadOutcome::Unreadable
        ));
        let still = std::fs::read_to_string(&p).unwrap();
        assert_eq!(still, "   ");
        cleanup(&p);
    }

    #[test]
    fn transient_unreadable_settings_refuse_to_save() {
        let p = tmp_path();
        let mut s = Settings::default();
        s.transient_unreadable = true;
        assert!(s.save(&p).is_err());
        assert!(!path_present(&p));
        cleanup(&p);
    }

    #[test]
    fn legacy_reuse_copies_voice_key() {
        let (s, migrated) =
            parse_settings(r#"{"groq_api_key":"gsk_voice","groq_reuse_transcription_key":true}"#)
                .unwrap();
        assert!(migrated);
        assert_eq!(s.groq_llm_api_key, "gsk_voice");
        assert_eq!(s.groq_api_key, "gsk_voice");
    }

    #[test]
    fn legacy_missing_flag_copies_voice_key() {
        let (s, migrated) = parse_settings(r#"{"groq_api_key":"gsk_voice"}"#).unwrap();
        assert!(migrated);
        assert_eq!(s.groq_llm_api_key, "gsk_voice");
    }

    #[test]
    fn legacy_separate_key_moves_llm_key_for_groq_backend() {
        let (s, migrated) = parse_settings(
            r#"{"groq_api_key":"gsk_voice","groq_reuse_transcription_key":false,"llm_backend":"groq","llm_api_key":"gsk_ai"}"#,
        )
        .unwrap();
        assert!(migrated);
        assert_eq!(s.groq_llm_api_key, "gsk_ai");
        assert_eq!(s.llm_api_key, "");
        assert_eq!(s.groq_api_key, "gsk_voice");
    }

    #[test]
    fn legacy_separate_key_other_backend_keeps_llm_key() {
        let (s, migrated) = parse_settings(
            r#"{"groq_api_key":"gsk_voice","groq_reuse_transcription_key":false,"llm_backend":"anthropic","llm_api_key":"sk_other"}"#,
        )
        .unwrap();
        assert!(!migrated);
        assert_eq!(s.groq_llm_api_key, "");
        assert_eq!(s.llm_api_key, "sk_other");
    }

    #[test]
    fn migration_is_idempotent_after_persist() {
        let p = tmp_path();
        std::fs::write(&p, br#"{"groq_api_key":"gsk_voice"}"#).unwrap();
        let first = match Settings::load(&p) {
            LoadOutcome::Loaded(loaded, migrated) => {
                assert!(migrated);
                *loaded
            }
            _ => panic!("expected Loaded"),
        };
        first.save(&p).unwrap();
        match Settings::load(&p) {
            LoadOutcome::Loaded(loaded, migrated) => {
                assert!(!migrated);
                assert_eq!(loaded.groq_llm_api_key, "gsk_voice");
            }
            _ => panic!("expected Loaded"),
        }
        let mut cleared = first.clone();
        cleared.groq_llm_api_key.clear();
        cleared.save(&p).unwrap();
        match Settings::load(&p) {
            LoadOutcome::Loaded(loaded, migrated) => {
                assert!(!migrated);
                assert_eq!(loaded.groq_llm_api_key, "");
            }
            _ => panic!("expected Loaded"),
        }
        cleanup(&p);
    }

    #[test]
    fn merge_applies_known_keys_and_ignores_unknown() {
        let base = Settings::default();
        let patch = serde_json::json!({
            "llm_enabled": true,
            "ui_language": "pt",
            "hotkey_toggle": "Ctrl+Shift+T",
            "transient_unreadable": true,
            "not_a_field": 5
        });
        let merged = base.merged_with(patch.as_object().unwrap()).unwrap();
        assert!(merged.llm_enabled);
        assert_eq!(merged.ui_language, "pt");
        assert!(!merged.transient_unreadable);
        assert_eq!(merged.hotkey_ptt, base.hotkey_ptt);
    }

    #[test]
    fn auto_update_defaults_on_and_follows_patches() {
        let (legacy, _) = parse_settings(r#"{"ui_language":"pt"}"#).unwrap();
        assert!(legacy.auto_update);
        let patch = serde_json::json!({ "auto_update": false });
        let merged = legacy.merged_with(patch.as_object().unwrap()).unwrap();
        assert!(!merged.auto_update);
        let bad = serde_json::json!({ "auto_update": "no" });
        assert!(legacy.merged_with(bad.as_object().unwrap()).is_err());
    }

    #[test]
    fn merge_rejects_invalid_types() {
        let base = Settings::default();
        let patch = serde_json::json!({ "vad_enabled": "yes" });
        assert!(base.merged_with(patch.as_object().unwrap()).is_err());
        let patch = serde_json::json!({ "record_mode": "hold" });
        assert!(base.merged_with(patch.as_object().unwrap()).is_err());
    }

    #[test]
    fn merge_clamps_numeric_ranges() {
        let base = Settings::default();
        let patch = serde_json::json!({
            "vad_threshold": 2.0,
            "speech_pad_ms": 900,
            "min_silence_ms": 1,
            "paste_delay_ms": 5000,
            "llm_timeout_ms": 10,
            "llm_temperature": -3.0
        });
        let merged = base.merged_with(patch.as_object().unwrap()).unwrap();
        assert!((merged.vad_threshold - 0.9).abs() < f32::EPSILON);
        assert_eq!(merged.speech_pad_ms, 400);
        assert_eq!(merged.min_silence_ms, 50);
        assert_eq!(merged.paste_delay_ms, 1000);
        assert_eq!(merged.llm_timeout_ms, 500);
        assert!(merged.llm_temperature.abs() < f32::EPSILON);
    }
}
