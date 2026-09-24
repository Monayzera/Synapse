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

    let prefer_gpu = state.settings_snapshot().prefer_gpu;

    emit(app, Stage::ResolveRelease, 0.0, 1.0, "Searching for the latest llama-server...");
    let assets = resolve_assets(&client, prefer_gpu).await?;

    state.stop_sidecar();
    state.sidecar_ready.store(false, Ordering::Release);

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

const RELEASES_API: &str = "https://api.github.com/repos/ggml-org/llama.cpp/releases?per_page=10";
const NIGHTLY_TAG_URL: &str =
    "https://github.com/ggml-org/llama.cpp/releases/latest/download/nightly-tag.txt";

async fn resolve_assets(client: &reqwest::Client, prefer_gpu: bool) -> AppResult<Vec<Asset>> {
    let mut errors = Vec::new();
    match resolve_via_api(client, prefer_gpu).await {
        Ok(Some(assets)) => return Ok(assets),
        Ok(None) => errors.push("api.github.com: no recent release has a build for this system".to_string()),
        Err(err) => errors.push(format!("api.github.com: {err}")),
    }
    match resolve_via_html(client).await {
        Ok(Some(assets)) => return Ok(assets),
        Ok(None) => errors.push("github.com: the nightly release has no build for this system".to_string()),
        Err(err) => errors.push(format!("github.com: {err}")),
    }
    Err(AppError::Download(format!(
        "could not fetch llama-server (check your connection): {}",
        errors.join(" | ")
    )))
}

async fn resolve_via_api(
    client: &reqwest::Client,
    prefer_gpu: bool,
) -> AppResult<Option<Vec<Asset>>> {
    let response = client
        .get(RELEASES_API)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| AppError::Download(e.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(AppError::Download(api_failure(status, response.headers())));
    }
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|e| AppError::Download(e.to_string()))?;
    let releases = value
        .as_array()
        .ok_or_else(|| AppError::Download("unexpected response".to_string()))?;
    Ok(select_release(releases, prefer_gpu).map(|(tag, assets)| {
        tracing::info!("llama.cpp release {tag} selected");
        assets
    }))
}

fn select_release(releases: &[serde_json::Value], prefer_gpu: bool) -> Option<(String, Vec<Asset>)> {
    let mut fallback = None;
    for release in releases {
        if release.get("draft").and_then(|v| v.as_bool()).unwrap_or(false) {
            continue;
        }
        let tag = release
            .get("tag_name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let assets = release
            .get("assets")
            .and_then(|a| a.as_array())
            .map(|list| api_assets(list))
            .unwrap_or_default();
        if has_wanted(&assets, prefer_gpu) {
            return Some((tag, assets));
        }
        if fallback.is_none() && cpu_asset(&assets).is_some() {
            fallback = Some((tag, assets));
        }
    }
    fallback
}

fn api_assets(list: &[serde_json::Value]) -> Vec<Asset> {
    let mut out = Vec::new();
    for asset in list {
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
    out
}

fn api_failure(status: reqwest::StatusCode, headers: &reqwest::header::HeaderMap) -> String {
    let limited = status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || (status == reqwest::StatusCode::FORBIDDEN
            && (header_value(headers, "x-ratelimit-remaining") == Some("0")
                || header_value(headers, "retry-after").is_some()));
    if !limited {
        return format!("GitHub returned HTTP {}", status.as_u16());
    }
    let wait_secs = header_value(headers, "retry-after")
        .and_then(|v| v.parse::<u64>().ok())
        .or_else(|| {
            let reset = header_value(headers, "x-ratelimit-reset")?.parse::<u64>().ok()?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs();
            Some(reset.saturating_sub(now))
        });
    match wait_secs {
        Some(secs) => format!(
            "GitHub rate limit reached, try again in about {} min",
            secs.div_ceil(60).max(1)
        ),
        None => "GitHub rate limit reached, try again later".to_string(),
    }
}

fn header_value<'a>(headers: &'a reqwest::header::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
}

async fn resolve_via_html(client: &reqwest::Client) -> AppResult<Option<Vec<Asset>>> {
    let body = client
        .get(NIGHTLY_TAG_URL)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| AppError::Download(e.to_string()))?
        .error_for_status()
        .map_err(|e| AppError::Download(e.to_string()))?
        .text()
        .await
        .map_err(|e| AppError::Download(e.to_string()))?;
    let tag = body.trim();
    if tag.is_empty()
        || tag.len() > 64
        || !tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(AppError::Download("could not identify the version".to_string()));
    }

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

    let out = html_assets(&html, tag)?;
    if cpu_asset(&out).is_some() {
        tracing::info!("llama.cpp release {tag} selected from the nightly tag");
        Ok(Some(out))
    } else {
        Ok(None)
    }
}

fn html_assets(html: &str, tag: &str) -> AppResult<Vec<Asset>> {
    let re = regex::Regex::new(&format!(
        r#"/releases/download/{}/([A-Za-z0-9._-]+)""#,
        regex::escape(tag)
    ))
    .map_err(|e| AppError::Download(e.to_string()))?;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for caps in re.captures_iter(html) {
        let Some(name) = caps.get(1).map(|m| m.as_str().to_string()) else {
            continue;
        };
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
    Ok(out)
}

fn pick<'a>(assets: &'a [Asset], must: &[&str], must_not: &[&str]) -> Option<&'a Asset> {
    assets.iter().find(|a| {
        let lower = a.name.to_ascii_lowercase();
        lower.starts_with("llama-")
            && must.iter().all(|m| lower.contains(m))
            && must_not.iter().all(|m| !lower.contains(m))
    })
}

#[cfg(windows)]
fn gpu_asset(assets: &[Asset]) -> Option<&Asset> {
    pick(assets, &["win", "cuda-12", "x64", ".zip"], &["cudart"])
}

fn cpu_asset(assets: &[Asset]) -> Option<&Asset> {
    #[cfg(windows)]
    let asset = pick(assets, &["win-cpu", "x64", ".zip"], &[]);
    #[cfg(target_os = "macos")]
    let asset = pick(assets, &["macos-arm64"], &[]);
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let asset = pick(assets, &["ubuntu", "x64"], &["cuda"]);
    asset
}

fn has_wanted(assets: &[Asset], prefer_gpu: bool) -> bool {
    #[cfg(windows)]
    {
        if prefer_gpu && gpu_asset(assets).is_none() {
            return false;
        }
    }
    #[cfg(not(windows))]
    {
        let _ = prefer_gpu;
    }
    cpu_asset(assets).is_some()
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
            if let Some(asset) = gpu_asset(assets) {
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
    let asset = cpu_asset(assets)
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
        let kind = entry.file_type()?;
        let dest = bin_dir.join(entry.file_name());
        #[cfg(unix)]
        {
            if kind.is_symlink() || kind.is_file() {
                remove_existing(&dest)?;
            }
            if kind.is_symlink() {
                std::os::unix::fs::symlink(std::fs::read_link(entry.path())?, &dest)?;
                continue;
            }
        }
        if kind.is_file() {
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

#[cfg(unix)]
fn remove_existing(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Err(std::io::Error::other(format!(
            "{} is a directory",
            path.display()
        ))),
        Ok(_) => std::fs::remove_file(path),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NAMES: [&str; 9] = [
        "cudart-llama-bin-win-cuda-12.4-x64.zip",
        "cudart-llama-bin-win-cuda-13.4-x64.zip",
        "llama-b11159-bin-macos-arm64.tar.gz",
        "llama-b11159-bin-ubuntu-x64.tar.gz",
        "llama-b11159-bin-win-cpu-arm64.zip",
        "llama-b11159-bin-win-cpu-x64.zip",
        "llama-b11159-bin-win-cuda-12.4-x64.zip",
        "llama-b11159-bin-win-cuda-13.4-arm64.zip",
        "llama-b11159-bin-win-cuda-13.4-x64.zip",
    ];

    fn release(tag: &str, draft: bool, names: &[&str]) -> serde_json::Value {
        let assets: Vec<serde_json::Value> = names
            .iter()
            .map(|name| {
                json!({
                    "name": name,
                    "browser_download_url": format!("https://github.com/ggml-org/llama.cpp/releases/download/{tag}/{name}"),
                    "size": 1u64
                })
            })
            .collect();
        json!({ "tag_name": tag, "draft": draft, "prerelease": true, "assets": assets })
    }

    #[test]
    fn select_release_skips_release_without_binaries_and_drafts() {
        let releases = vec![
            release("v0.5.0", false, &["nightly-tag.txt"]),
            release("b11160", true, &NAMES),
            release("b11159", false, &NAMES),
        ];
        let selected = select_release(&releases, false).map(|(tag, _)| tag);
        assert_eq!(selected.as_deref(), Some("b11159"));
        assert!(select_release(&[release("v0.5.0", false, &["nightly-tag.txt"])], true).is_none());
    }

    #[test]
    fn pick_never_returns_cudart_archive() {
        let assets = html_assets(
            &NAMES
                .iter()
                .map(|name| format!("<a href=\"/ggml-org/llama.cpp/releases/download/b11159/{name}\" rel=\"nofollow\">"))
                .collect::<String>(),
            "b11159",
        )
        .unwrap_or_default();
        assert_eq!(assets.len(), NAMES.len());
        assert!(assets.iter().any(|a| a.name == "cudart-llama-bin-win-cuda-13.4-x64.zip"));
        assert!(pick(&assets, &["cuda"], &[]).is_some_and(|a| a.name.starts_with("llama-")));
        assert!(pick(&assets, &["cudart"], &[]).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_assets_match_expected_archives() {
        let (_, assets) = select_release(&[release("b11159", false, &NAMES)], true).unwrap_or_default();
        assert_eq!(
            gpu_asset(&assets).map(|a| a.name.as_str()),
            Some("llama-b11159-bin-win-cuda-12.4-x64.zip")
        );
        assert_eq!(
            cpu_asset(&assets).map(|a| a.name.as_str()),
            Some("llama-b11159-bin-win-cpu-x64.zip")
        );
    }

    #[cfg(windows)]
    #[test]
    fn gpu_preference_prefers_complete_release_then_cpu_fallback() {
        let cpu_only = ["llama-b11161-bin-win-cpu-x64.zip"];
        let complete = release("b11159", false, &NAMES);
        let partial = release("b11161", false, &cpu_only);
        let selected = select_release(&[partial.clone(), complete], true).map(|(tag, _)| tag);
        assert_eq!(selected.as_deref(), Some("b11159"));
        let fallback = select_release(&[partial], true).map(|(tag, _)| tag);
        assert_eq!(fallback.as_deref(), Some("b11161"));
    }

    #[test]
    fn api_failure_reports_rate_limit() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-ratelimit-remaining", reqwest::header::HeaderValue::from_static("0"));
        headers.insert("retry-after", reqwest::header::HeaderValue::from_static("125"));
        let message = api_failure(reqwest::StatusCode::FORBIDDEN, &headers);
        assert!(message.contains("rate limit"));
        assert!(message.contains("3 min"));
        let other = api_failure(reqwest::StatusCode::NOT_FOUND, &reqwest::header::HeaderMap::new());
        assert_eq!(other, "GitHub returned HTTP 404");
    }
}
