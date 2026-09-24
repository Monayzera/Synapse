use crate::error::{AppError, AppResult};
use crate::models::{ModelInfo, ModelKind};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomModel {
    pub id: String,
    pub label: String,
    pub kind: ModelKind,
    pub filename: String,
    pub url: String,
    pub size_bytes: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomStore {
    pub models: Vec<CustomModel>,
}

impl CustomStore {
    pub fn load(path: &Path) -> CustomStore {
        match std::fs::read_to_string(path) {
            Ok(content) => match serde_json::from_str::<CustomStore>(&content) {
                Ok(store) => store,
                Err(err) => {
                    tracing::warn!("custom models parse failed ({err}); using empty store");
                    if path.exists() {
                        let backup = path.with_extension("json.corrupt");
                        let _ = std::fs::copy(path, backup);
                    }
                    CustomStore::default()
                }
            },
            Err(_) => CustomStore::default(),
        }
    }

    pub fn save(&self, path: &Path) -> AppResult<()> {
        let json = serde_json::to_string_pretty(self)?;
        crate::atomic_io::write_durable(path, json.as_bytes())
            .map_err(|err| AppError::Config(err.to_string()))
    }

    pub fn find(&self, id: &str) -> Option<&CustomModel> {
        self.models.iter().find(|m| m.id == id)
    }

    pub fn has_filename(&self, filename: &str) -> bool {
        self.models.iter().any(|m| m.filename == filename)
    }

    pub fn upsert(&mut self, model: CustomModel) {
        if let Some(existing) = self.models.iter_mut().find(|m| m.id == model.id) {
            *existing = model;
        } else {
            self.models.push(model);
        }
    }

    pub fn remove(&mut self, id: &str) -> Option<CustomModel> {
        if let Some(pos) = self.models.iter().position(|m| m.id == id) {
            Some(self.models.remove(pos))
        } else {
            None
        }
    }
}

pub fn to_info(model: &CustomModel) -> ModelInfo {
    ModelInfo {
        id: model.id.clone(),
        label: model.label.clone(),
        kind: model.kind,
        filename: model.filename.clone(),
        url: model.url.clone(),
        size_bytes: model.size_bytes,
    }
}

pub fn sanitize_filename(raw: &str) -> Option<String> {
    let base = raw
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if base.is_empty() || base == "." || base == ".." || base.contains('\0') {
        return None;
    }
    Some(base)
}

pub fn make_id(filename: &str, url: &str) -> String {
    let slug: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug: String = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug: String = slug.chars().take(32).collect();
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    let hash = hasher.finish();
    format!("custom-{slug}-{hash:08x}")
}
