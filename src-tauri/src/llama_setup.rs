use crate::config::LlmBackend;
use crate::error::{AppError, AppResult};
use crate::services;
use crate::state::SharedState;
use crate::models;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

#[cfg(windows)]
const BIN_NAME: &str = "llama-server.exe";
#[cfg(not(windows))]
const BIN_NAME: &str = "llama-server";

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Stage {
    ResolveRelease,
    DownloadBinary,
    Unzip,
    DownloadModel,
    ConfigureStart,
}

#[derive(Serialize, Clone)]
struct Progress {
    stage: Stage,
    pct: f64,
    overall_pct: f64,
    message: String,
    done: bool,
    error: Option<String>,
}

struct Asset {
    name: String,
    url: String,
    size: u64,
}

fn emit(app: &AppHandle, stage: Stage, pct: f64, overall: f64, message: &str) {
    let _ = app.emit(
        "llama-setup-progress",
        Progress {
            stage,
            pct: (pct * 10.0).round() / 10.0,
            overall_pct: (overall.clamp(0.0, 100.0) * 10.0).round() / 10.0,
            message: message.to_string(),
            done: false,
            error: None,
        },
    );
}

fn emit_done(app: &AppHandle, message: &str) {
    let _ = app.emit(
        "llama-setup-progress",
        Progress {
            stage: Stage::ConfigureStart,
            pct: 100.0,
            overall_pct: 100.0,
            message: message.to_string(),
            done: true,
            error: None,
        },
    );
}

fn emit_error(app: &AppHandle, stage: Stage, message: &str) {
    let _ = app.emit(
        "llama-setup-progress",
        Progress {
            stage,
            pct: 0.0,
            overall_pct: 0.0,
            message: message.to_string(),
            done: false,
            error: Some(message.to_string()),
        },
    );
}

pub fn launch(app: AppHandle, state: SharedState) {
    if state.llama_setup_running.swap(true, Ordering::AcqRel) {
        let _ = app.emit(
            "llama-setup-progress",
            Progress {
                stage: Stage::ResolveRelease,
                pct: 0.0,
                overall_pct: 0.0,
                message: "Setup already in progress.".to_string(),
                done: false,
                error: Some("Setup already in progress.".to_string()),
            },
        );
        return;
    }
    tauri::async_runtime::spawn(async move {
        let result = run(&app, &state).await;
        state.llama_setup_running.store(false, Ordering::Release);
        if let Err(err) = result {
            tracing::warn!("llama auto-setup failed: {err}");
            emit_error(&app, Stage::ConfigureStart, &err.to_string());
        }
    });
}

async fn run(app: &AppHandle, state: &SharedState) -> AppResult<()> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .tcp_keepalive(Duration::from_secs(30))
        .user_agent("Synapse")
        .build()
        .map_err(|e| AppError::Download(e.to_string()))?;

    emit(app, Stage::ResolveRelease, 0.0, 1.0, "Searching for the latest llama-server...");
    let assets = resolve_assets(&client).await?;

    state.stop_sidecar();
    state.sidecar_ready.store(false, Ordering::Release);

    let prefer_gpu = state.settings_snapshot().prefer_gpu;
    let used_gpu = install_binary(app, &client, &assets, &state.bin_dir, prefer_gpu).await?;

    set_executable(&state.bin_dir.join(BIN_NAME));

    let gemma = models::find("gemma-3-4b-it")
        .ok_or_else(|| AppError::Model("unknown correction model".to_string()))?;
    let model_dest = models::model_path(&state.models_dir, &gemma.filename);
    if !models::is_present(&state.models_dir, &gemma) {
        emit(app, Stage::DownloadModel, 0.0, 30.0, "Downloading the AI correction model (Gemma 3 4B)...");
        let mut last = 0u64;
        models::download_to_file(&client, &gemma.url, &model_dest, gemma.size_bytes, |done, total| {
            if total > 0 && (done - last >= 8_000_000 || done == total) {
                last = done;
                let pct = done as f64 / total as f64 * 100.0;
                let overall = 30.0 + pct * 0.65;
                emit(app, Stage::DownloadModel, pct, overall, "Downloading the AI correction model (Gemma 3 4B)...");
            }
        })
        .await?;
    } else {
        emit(app, Stage::DownloadModel, 100.0, 95.0, "AI correction model already present.");
    }

    emit(app, Stage::ConfigureStart, 0.0, 95.0, "Configuring and starting the local server...");
    configure_settings(state, used_gpu, &gemma.filename)?;

    services::restart_sidecar(state).await;

    if !state.sidecar_ready.load(Ordering::Acquire) && used_gpu {
        tracing::warn!("gpu llama-server not ready; falling back to cpu build");
        emit(app, Stage::DownloadBinary, 0.0, 95.0, "GPU unavailable. Switching to the CPU version...");
        state.stop_sidecar();
        state.sidecar_ready.store(false, Ordering::Release);
        install_cpu_only(app, &client, &assets, &state.bin_dir).await?;
        set_executable(&state.bin_dir.join(BIN_NAME));
        configure_settings(state, false, &gemma.filename)?;
        services::restart_sidecar(state).await;
    }

    if state.sidecar_ready.load(Ordering::Acquire) {
        emit_done(app, "AI correction ready to use.");
    } else {
        emit_error(app, Stage::ConfigureStart, "The local server did not respond in time. Please try again.");
    }

    Ok(())
}

async fn resolve_assets(client: &reqwest::Client) -> AppResult<Vec<Asset>> {
    let mut errors = Vec::new();
    match resolve_via_html(client).await {
        Ok(assets) if !assets.is_empty() => return Ok(assets),
        Ok(_) => errors.push("github.com: no binaries listed".to_string()),
        Err(err) => errors.push(format!("github.com: {err}")),
    }
    match resolve_via_api(client).await {
        Ok(assets) if !assets.is_empty() => return Ok(assets),
        Ok(_) => errors.push("api.github.com: empty".to_string()),
        Err(err) => errors.push(format!("api.github.com: {err}")),
    }
    Err(AppError::Download(format!(
        "could not fetch llama-server (check your connection): {}",
        errors.join(" | ")
    )))
}

async fn resolve_via_html(client: &reqwest::Client) -> AppResult<Vec<Asset>> {
    let latest = client
        .get("https://github.com/ggml-org/llama.cpp/releases/latest")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| AppError::Download(e.to_string()))?;
    let final_url = latest.url().as_str().to_string();
    let tag = final_url
        .rsplit('/')
        .find(|s| !s.is_empty())
        .ok_or_else(|| AppError::Download("could not identify the version".to_string()))?
        .to_string();

    let assets_url =
        format!("https://github.com/ggml-org/llama.cpp/releases/expanded_assets/{tag}");
    let html = client
        .get(&assets_url)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| AppError::Download(e.to_string()))?
        .error_for_status()
        .map_err(|e| AppError::Download(e.to_string()))?
        .text()
        .await
        .map_err(|e| AppError::Download(e.to_string()))?;

    let re = regex::Regex::new(r"llama-[A-Za-z0-9._-]+?\.(?:zip|tar\.gz)")
        .map_err(|e| AppError::Download(e.to_string()))?;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for m in re.find_iter(&html) {
        let name = m.as_str().to_string();
        if !seen.insert(name.clone()) {
            continue;
        }
        let url = format!(
            "https://github.com/ggml-org/llama.cpp/releases/download/{tag}/{name}"
        );
        out.push(Asset {
            name,
            url,
            size: 0,
        });
    }
    if out.iter().any(|a| a.name.contains(&tag)) {
        out.retain(|a| a.name.contains(&tag));
    }
    Ok(out)
}

async fn resolve_via_api(client: &reqwest::Client) -> AppResult<Vec<Asset>> {
    let url = "https://api.github.com/repos/ggml-org/llama.cpp/releases/latest";
    let value: serde_json::Value = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| AppError::Download(e.to_string()))?
        .error_for_status()
        .map_err(|e| AppError::Download(e.to_string()))?
        .json()
        .await
        .map_err(|e| AppError::Download(e.to_string()))?;
    let assets = value
        .get("assets")
        .and_then(|a| a.as_array())
        .ok_or_else(|| AppError::Download("unexpected response".to_string()))?;
    let mut out = Vec::new();
    for asset in assets {
        let name = asset.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let url = asset
            .get("browser_download_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let size = asset.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
        if !name.is_empty() && !url.is_empty() {
            out.push(Asset {
                name: name.to_string(),
                url: url.to_string(),
                size,
            });
        }
    }
    Ok(out)
}

fn pick<'a>(assets: &'a [Asset], must: &[&str], must_not: &[&str]) -> Option<&'a Asset> {
    assets.iter().find(|a| {
        let lower = a.name.to_ascii_lowercase();
        must.iter().all(|m| lower.contains(m)) && must_not.iter().all(|m| !lower.contains(m))
    })
}

async fn install_binary(
    app: &AppHandle,
    client: &reqwest::Client,
    assets: &[Asset],
    bin_dir: &Path,
    prefer_gpu: bool,
) -> AppResult<bool> {
    #[cfg(windows)]
    {
        if prefer_gpu {
            if let Some(asset) =
                pick(assets, &["win", "cuda-13", "x64", ".zip"], &["cudart"])
            {
                fetch_and_extract(app, client, asset, bin_dir, true).await?;
                return Ok(true);
            }
        }
        install_cpu_only(app, client, assets, bin_dir).await?;
        Ok(false)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = prefer_gpu;
        let asset = pick(assets, &["macos-arm64"], &[])
            .ok_or_else(|| AppError::Download("macOS binary not found".to_string()))?;
        let is_zip = asset.name.to_ascii_lowercase().ends_with(".zip");
        fetch_and_extract(app, client, asset, bin_dir, is_zip).await?;
        Ok(true)
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let _ = prefer_gpu;
        install_cpu_only(app, client, assets, bin_dir).await?;
        Ok(false)
    }
}

async fn install_cpu_only(
    app: &AppHandle,
    client: &reqwest::Client,
    assets: &[Asset],
    bin_dir: &Path,
) -> AppResult<()> {
    #[cfg(windows)]
    let asset = pick(assets, &["win-cpu", "x64", ".zip"], &[]);
    #[cfg(target_os = "macos")]
    let asset = pick(assets, &["macos-arm64"], &[]);
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let asset = pick(assets, &["ubuntu", "x64"], &["cuda"]);

    let asset = asset
        .ok_or_else(|| AppError::Download("llama-server CPU binary not found".to_string()))?;
    let is_zip = asset.name.to_ascii_lowercase().ends_with(".zip");
    fetch_and_extract(app, client, asset, bin_dir, is_zip).await
}

async fn fetch_and_extract(
    app: &AppHandle,
    client: &reqwest::Client,
    asset: &Asset,
    bin_dir: &Path,
    is_zip: bool,
) -> AppResult<()> {
    std::fs::create_dir_all(bin_dir)?;
    let archive = bin_dir.join(if is_zip { "llama-archive.zip" } else { "llama-archive.tar.gz" });

    emit(app, Stage::DownloadBinary, 0.0, 2.0, "Downloading llama-server...");
    let mut last = 0u64;
    models::download_to_file(client, &asset.url, &archive, asset.size, |done, total| {
        if total > 0 && (done - last >= 4_000_000 || done == total) {
            last = done;
            let pct = done as f64 / total as f64 * 100.0;
            let overall = 2.0 + pct * 0.23;
            emit(app, Stage::DownloadBinary, pct, overall, "Downloading llama-server...");
        }
    })
    .await?;

    emit(app, Stage::Unzip, 0.0, 25.0, "Extracting llama-server...");
    let extract_dir = bin_dir.join(".extract");
    let _ = std::fs::remove_dir_all(&extract_dir);
    std::fs::create_dir_all(&extract_dir)?;

    if is_zip {
        extract_zip(&archive, &extract_dir)?;
    } else {
        extract_targz(&archive, &extract_dir)?;
    }
    let _ = std::fs::remove_file(&archive);

    let exe = find_binary(&extract_dir, BIN_NAME)
        .ok_or_else(|| AppError::Download("llama-server not found in the package".to_string()))?;
    let src_dir = exe
        .parent()
        .ok_or_else(|| AppError::Download("invalid package structure".to_string()))?;

    for entry in std::fs::read_dir(src_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            let dest = bin_dir.join(entry.file_name());
            std::fs::copy(entry.path(), dest)?;
        }
    }
    let _ = std::fs::remove_dir_all(&extract_dir);

    emit(app, Stage::Unzip, 100.0, 30.0, "llama-server installed.");
    Ok(())
}

fn extract_zip(archive: &Path, out_dir: &Path) -> AppResult<()> {
    let file = std::fs::File::open(archive)?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| AppError::Download(e.to_string()))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| AppError::Download(e.to_string()))?;
        let rel = match entry.enclosed_name() {
            Some(path) => path,
            None => continue,
        };
        let out = out_dir.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut dest = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut dest)?;
    }
    Ok(())
}

fn extract_targz(archive: &Path, out_dir: &Path) -> AppResult<()> {
    let file = std::fs::File::open(archive)?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);
    tar.unpack(out_dir)
        .map_err(|e| AppError::Download(e.to_string()))?;
    Ok(())
}

fn find_binary(dir: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(path);
        }
    }
    for sub in dirs {
        if let Some(found) = find_binary(&sub, name) {
            return Some(found);
        }
    }
    None
}

fn set_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

fn configure_settings(state: &SharedState, gpu: bool, model_filename: &str) -> AppResult<()> {
    state.mutate_settings(|settings| {
        settings.llm_enabled = true;
        settings.llm_backend = LlmBackend::Local;
        settings.llm_local_model = model_filename.to_string();
        settings.llm_endpoint = "http://127.0.0.1:8123/v1".to_string();
        settings.llm_model_name = "local".to_string();
        settings.llm_temperature = 0.1;
        settings.llm_gpu_layers = if gpu { 99 } else { 0 };
        settings.llm_timeout_ms = if gpu { 4000 } else { 8000 };
        Ok(())
    })?;
    state.emit_settings_changed();
    Ok(())
}
