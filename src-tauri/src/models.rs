use crate::error::{AppError, AppResult};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Whisper,
    Llm,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub label: String,
    pub kind: ModelKind,
    pub filename: String,
    pub url: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelStatus {
    pub info: ModelInfo,
    pub present: bool,
    pub actual_bytes: u64,
}

#[derive(Clone, Serialize)]
struct DownloadProgress {
    id: String,
    downloaded: u64,
    total: u64,
    pct: f64,
    done: bool,
    error: Option<String>,
}

pub fn registry() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: "large-v3-turbo".to_string(),
            label: "Whisper large-v3-turbo (recommended)".to_string(),
            kind: ModelKind::Whisper,
            filename: "ggml-large-v3-turbo.bin".to_string(),
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin".to_string(),
            size_bytes: 1_624_555_275,
        },
        ModelInfo {
            id: "large-v3".to_string(),
            label: "Whisper large-v3 (max accuracy)".to_string(),
            kind: ModelKind::Whisper,
            filename: "ggml-large-v3.bin".to_string(),
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin".to_string(),
            size_bytes: 3_095_033_483,
        },
        ModelInfo {
            id: "large-v3-turbo-q5".to_string(),
            label: "Whisper large-v3-turbo Q5 (light, best for CPU)".to_string(),
            kind: ModelKind::Whisper,
            filename: "ggml-large-v3-turbo-q5_0.bin".to_string(),
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin".to_string(),
            size_bytes: 574_041_195,
        },
        ModelInfo {
            id: "medium".to_string(),
            label: "Whisper medium (CPU fallback)".to_string(),
            kind: ModelKind::Whisper,
            filename: "ggml-medium.bin".to_string(),
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.bin".to_string(),
            size_bytes: 1_533_763_059,
        },
        ModelInfo {
            id: "medium-q5".to_string(),
            label: "Whisper medium Q5 (light)".to_string(),
            kind: ModelKind::Whisper,
            filename: "ggml-medium-q5_0.bin".to_string(),
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium-q5_0.bin".to_string(),
            size_bytes: 539_212_467,
        },
        ModelInfo {
            id: "small".to_string(),
            label: "Whisper small (fast, good quality)".to_string(),
            kind: ModelKind::Whisper,
            filename: "ggml-small.bin".to_string(),
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin".to_string(),
            size_bytes: 487_601_967,
        },
        ModelInfo {
            id: "base".to_string(),
            label: "Whisper base (very fast, light)".to_string(),
            kind: ModelKind::Whisper,
            filename: "ggml-base.bin".to_string(),
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin".to_string(),
            size_bytes: 147_951_465,
        },
        ModelInfo {
            id: "gemma-3-4b-it".to_string(),
            label: "Gemma 3 4B Instruct Q4_K_M (cleanup)".to_string(),
            kind: ModelKind::Llm,
            filename: "google_gemma-3-4b-it-Q4_K_M.gguf".to_string(),
            url: "https://huggingface.co/bartowski/google_gemma-3-4b-it-GGUF/resolve/main/google_gemma-3-4b-it-Q4_K_M.gguf".to_string(),
            size_bytes: 2_489_894_016,
        },
        ModelInfo {
            id: "qwen2.5-3b-instruct".to_string(),
            label: "Qwen2.5 3B Instruct Q4_K_M (light, great)".to_string(),
            kind: ModelKind::Llm,
            filename: "Qwen2.5-3B-Instruct-Q4_K_M.gguf".to_string(),
            url: "https://huggingface.co/bartowski/Qwen2.5-3B-Instruct-GGUF/resolve/main/Qwen2.5-3B-Instruct-Q4_K_M.gguf".to_string(),
            size_bytes: 1_929_903_264,
        },
        ModelInfo {
            id: "qwen2.5-1.5b-instruct".to_string(),
            label: "Qwen2.5 1.5B Instruct Q4_K_M (very light)".to_string(),
            kind: ModelKind::Llm,
            filename: "Qwen2.5-1.5B-Instruct-Q4_K_M.gguf".to_string(),
            url: "https://huggingface.co/bartowski/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/Qwen2.5-1.5B-Instruct-Q4_K_M.gguf".to_string(),
            size_bytes: 986_048_768,
        },
        ModelInfo {
            id: "llama-3.2-3b-instruct".to_string(),
            label: "Llama 3.2 3B Instruct Q4_K_M (light, strong)".to_string(),
            kind: ModelKind::Llm,
            filename: "Llama-3.2-3B-Instruct-Q4_K_M.gguf".to_string(),
            url: "https://huggingface.co/bartowski/Llama-3.2-3B-Instruct-GGUF/resolve/main/Llama-3.2-3B-Instruct-Q4_K_M.gguf".to_string(),
            size_bytes: 2_019_377_696,
        },
        ModelInfo {
            id: "llama-3.2-1b-instruct".to_string(),
            label: "Llama 3.2 1B Instruct Q4_K_M (ultra light)".to_string(),
            kind: ModelKind::Llm,
            filename: "Llama-3.2-1B-Instruct-Q4_K_M.gguf".to_string(),
            url: "https://huggingface.co/bartowski/Llama-3.2-1B-Instruct-GGUF/resolve/main/Llama-3.2-1B-Instruct-Q4_K_M.gguf".to_string(),
            size_bytes: 807_694_464,
        },
    ]
}

pub fn find(id: &str) -> Option<ModelInfo> {
    registry().into_iter().find(|m| m.id == id)
}

pub fn model_path(models_dir: &Path, filename: &str) -> PathBuf {
    models_dir.join(filename)
}

pub fn is_present(models_dir: &Path, info: &ModelInfo) -> bool {
    let path = model_path(models_dir, &info.filename);
    match std::fs::metadata(&path) {
        Ok(meta) => meta.len() > 1_000_000,
        Err(_) => false,
    }
}

pub fn status_for(models_dir: &Path, info: &ModelInfo) -> ModelStatus {
    let path = model_path(models_dir, &info.filename);
    let actual_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let present = is_present(models_dir, info);
    ModelStatus {
        info: info.clone(),
        present,
        actual_bytes,
    }
}

pub async fn download_to_file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    fallback_total: u64,
    mut on_progress: impl FnMut(u64, u64),
) -> AppResult<u64> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| AppError::Download(e.to_string()))?;
    }
    let part = dest.with_extension("part");

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| AppError::Download(e.to_string()))?
        .error_for_status()
        .map_err(|e| AppError::Download(e.to_string()))?;

    let reported = response.content_length();
    let total = reported.unwrap_or(fallback_total);

    let mut file = tokio::fs::File::create(&part)
        .await
        .map_err(|e| AppError::Download(e.to_string()))?;

    let mut downloaded: u64 = 0;
    let mut stream = response.bytes_stream();
    let stall = std::time::Duration::from_secs(120);

    loop {
        let next = match tokio::time::timeout(stall, stream.next()).await {
            Ok(item) => item,
            Err(_) => {
                drop(file);
                let _ = tokio::fs::remove_file(&part).await;
                return Err(AppError::Download(
                    "stalled download: no data for 120s".to_string(),
                ));
            }
        };
        let chunk = match next {
            Some(Ok(chunk)) => chunk,
            Some(Err(err)) => {
                drop(file);
                let _ = tokio::fs::remove_file(&part).await;
                return Err(AppError::Download(err.to_string()));
            }
            None => break,
        };
        if let Err(err) = file.write_all(&chunk).await {
            drop(file);
            let _ = tokio::fs::remove_file(&part).await;
            return Err(AppError::Download(err.to_string()));
        }
        downloaded += chunk.len() as u64;
        on_progress(downloaded, total);
    }

    file.flush()
        .await
        .map_err(|e| AppError::Download(e.to_string()))?;
    drop(file);

    if reported.is_some() && total > 0 && downloaded != total {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(AppError::Download(format!(
            "incomplete download: {downloaded} of {total} bytes"
        )));
    }

    tokio::fs::rename(&part, dest)
        .await
        .map_err(|e| AppError::Download(e.to_string()))?;

    Ok(total.max(downloaded))
}

pub async fn download(app: &AppHandle, models_dir: &Path, info: &ModelInfo) -> AppResult<()> {
    let dest = model_path(models_dir, &info.filename);

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::Download(e.to_string()))?;

    let mut last_emit: u64 = 0;
    let total = download_to_file(&client, &info.url, &dest, info.size_bytes, |downloaded, total| {
        if downloaded - last_emit >= 4_000_000 || downloaded == total {
            last_emit = downloaded;
            emit_progress(app, info, downloaded, total, false, None);
        }
    })
    .await?;

    emit_progress(app, info, total, total, true, None);
    Ok(())
}

fn emit_progress(
    app: &AppHandle,
    info: &ModelInfo,
    downloaded: u64,
    total: u64,
    done: bool,
    error: Option<String>,
) {
    let pct = if total > 0 {
        (downloaded as f64 / total as f64 * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let _ = app.emit(
        "model-download-progress",
        DownloadProgress {
            id: info.id.clone(),
            downloaded,
            total,
            pct: (pct * 10.0).round() / 10.0,
            done,
            error,
        },
    );
}
