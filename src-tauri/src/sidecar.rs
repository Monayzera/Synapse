use crate::error::{AppError, AppResult};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;

#[cfg(windows)]
struct JobHandle(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
unsafe impl Send for JobHandle {}

pub struct Sidecar {
    child: Option<Child>,
    #[cfg(windows)]
    job: Option<JobHandle>,
}

impl Sidecar {
    pub fn spawn(
        exe: &Path,
        model: &Path,
        port: u16,
        n_gpu_layers: i32,
        ctx_size: u32,
        alias: &str,
        log_path: &Path,
    ) -> AppResult<Sidecar> {
        if !exe.exists() {
            return Err(AppError::Llm(format!(
                "llama-server binary missing: {}",
                exe.display()
            )));
        }
        if !model.exists() {
            return Err(AppError::Llm(format!("llm model missing: {}", model.display())));
        }

        let mut command = Command::new(exe);
        command
            .arg("--model")
            .arg(model)
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .arg("--ctx-size")
            .arg(ctx_size.to_string())
            .arg("--n-gpu-layers")
            .arg(n_gpu_layers.to_string())
            .arg("--threads")
            .arg(threads().to_string())
            .arg("-np")
            .arg("1")
            .arg("--swa-full")
            .arg("--no-ui")
            .arg("--alias")
            .arg(alias)
            .env_remove("CUDA_VISIBLE_DEVICES");

        if let Some(dir) = exe.parent() {
            let current = std::env::var("PATH").unwrap_or_default();
            let sep = if cfg!(windows) { ";" } else { ":" };
            command.env("PATH", format!("{}{sep}{current}", dir.display()));
        }

        configure_no_window(&mut command);
        command.stdin(Stdio::null());
        match open_log(log_path) {
            Ok((stdout, stderr)) => {
                command.stdout(stdout).stderr(stderr);
            }
            Err(err) => {
                tracing::warn!(
                    "llama-server log {} unavailable ({err}); output discarded",
                    log_path.display()
                );
                command.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }

        let child = command
            .spawn()
            .map_err(|e| AppError::Llm(format!("failed to start llama-server: {e}")))?;
        tracing::info!(
            "llama-server started (pid {}, port {port}, gpu layers {n_gpu_layers}, log {})",
            child.id(),
            log_path.display()
        );

        #[cfg(windows)]
        let job = assign_to_job(&child);

        Ok(Sidecar {
            child: Some(child),
            #[cfg(windows)]
            job,
        })
    }

    pub fn is_running(&mut self) -> bool {
        match self.child.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(None) => true,
                Ok(Some(status)) => {
                    tracing::warn!("llama-server exited ({status})");
                    false
                }
                Err(err) => {
                    tracing::debug!("llama-server status unknown: {err}");
                    true
                }
            },
            None => false,
        }
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        #[cfg(windows)]
        if let Some(job) = self.job.take() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(job.0);
            }
        }
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        self.stop();
    }
}

pub async fn health_ok(client: &reqwest::Client, port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/health");
    matches!(
        tokio::time::timeout(Duration::from_secs(2), client.get(&url).send()).await,
        Ok(Ok(response)) if response.status().is_success()
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Ours,
    Foreign,
    Unverified,
    Unknown,
}

pub async fn probe_owner(client: &reqwest::Client, port: u16, model: &Path, alias: &str) -> Owner {
    let url = format!("http://127.0.0.1:{port}/props");
    let expected = model.to_string_lossy();
    for attempt in 0..2 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let exchange = async {
            let response = client.get(&url).send().await?;
            let status = response.status();
            let body = response.bytes().await?;
            Ok::<_, reqwest::Error>((status, body))
        };
        match tokio::time::timeout(Duration::from_secs(3), exchange).await {
            Ok(Ok((status, body))) => {
                let props = serde_json::from_slice::<serde_json::Value>(&body).ok();
                let owner = judge_owner(status.as_u16(), props.as_ref(), &expected, alias);
                if owner != Owner::Unknown {
                    if owner == Owner::Foreign {
                        tracing::warn!(
                            "port {port} is answered by another server (HTTP {}, model {:?}, alias {:?})",
                            status.as_u16(),
                            props_text(props.as_ref(), "model_path"),
                            props_text(props.as_ref(), "model_alias")
                        );
                    } else if owner == Owner::Unverified {
                        tracing::warn!(
                            "llama-server props on port {port} list neither model_path nor model_alias (build {:?}); ownership not verifiable",
                            props_text(props.as_ref(), "build_info")
                        );
                    }
                    return owner;
                }
            }
            Ok(Err(err)) => tracing::debug!("llama-server props on port {port} unavailable: {err}"),
            Err(_) => tracing::debug!("llama-server props on port {port} timed out"),
        }
    }
    Owner::Unknown
}

fn props_text<'a>(props: Option<&'a serde_json::Value>, key: &str) -> Option<&'a str> {
    props?.get(key)?.as_str()
}

fn judge_owner(status: u16, props: Option<&serde_json::Value>, expected: &str, alias: &str) -> Owner {
    if status == 503 {
        return Owner::Unknown;
    }
    if !(200..300).contains(&status) {
        return Owner::Foreign;
    }
    let Some(props) = props.filter(|value| value.is_object()) else {
        return Owner::Foreign;
    };
    let served_path = props_text(Some(props), "model_path");
    let served_alias = props_text(Some(props), "model_alias");
    if served_path.is_none() && served_alias.is_none() {
        return Owner::Unverified;
    }
    let path_bad = served_path.is_some_and(|served| !same_model_path(served, expected, cfg!(windows)));
    let alias_bad = served_alias.is_some_and(|served| served != alias);
    if path_bad || alias_bad {
        Owner::Foreign
    } else {
        Owner::Ours
    }
}

fn same_model_path(served: &str, expected: &str, windows: bool) -> bool {
    let normalize = |path: &str| -> String {
        let path = path.trim();
        if windows {
            let path = path.strip_prefix(r"\\?\").unwrap_or(path);
            path.replace('/', "\\").trim_end_matches('\\').to_lowercase()
        } else {
            path.trim_end_matches('/').to_string()
        }
    };
    let served = normalize(served);
    !served.is_empty() && served == normalize(expected)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Alive,
    Exited,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Startup {
    Ready,
    Exited,
    Superseded,
    TimedOut,
}

pub async fn wait_until_ready<F>(
    client: &reqwest::Client,
    port: u16,
    timeout: Duration,
    mut alive: F,
) -> Startup
where
    F: FnMut() -> Liveness,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match alive() {
            Liveness::Alive => {}
            Liveness::Exited => {
                tracing::warn!("llama-server exited while starting");
                return Startup::Exited;
            }
            Liveness::Superseded => {
                tracing::info!("llama-server start superseded by a newer restart");
                return Startup::Superseded;
            }
        }
        if health_ok(client, port).await {
            return Startup::Ready;
        }
        if tokio::time::Instant::now() >= deadline {
            return Startup::TimedOut;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

pub fn run_capture(mut command: Command, timeout: Duration) -> Result<String, String> {
    use std::io::Read;

    configure_no_window(&mut command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|e| format!("could not start: {e}"))?;
    let (sender, receiver) = std::sync::mpsc::channel::<Vec<u8>>();
    match child.stdout.take() {
        Some(mut stdout) => {
            let reader = std::thread::Builder::new()
                .name("synapse-capture".to_string())
                .spawn(move || {
                    let mut buffer = Vec::new();
                    if let Err(err) = stdout.read_to_end(&mut buffer) {
                        tracing::debug!("process output read failed: {err}");
                    }
                    let _ = sender.send(buffer);
                });
            if let Err(err) = reader {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("could not read the output: {err}"));
            }
        }
        None => drop(sender),
    }
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("status unavailable: {err}"));
            }
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("no answer within {} s", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let bytes = receiver
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if status.success() {
        Ok(text)
    } else {
        let snippet: String = text.trim().chars().take(240).collect();
        Err(format!("exited with {status}: {snippet}"))
    }
}

fn open_log(path: &Path) -> std::io::Result<(std::fs::File, std::fs::File)> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > LOG_ROTATE_BYTES {
            let rotated = path.with_extension("1.log");
            let _ = std::fs::remove_file(&rotated);
            if let Err(err) = std::fs::rename(path, &rotated) {
                tracing::debug!("llama-server log rotation failed: {err}");
            }
        }
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let clone = file.try_clone()?;
    Ok((file, clone))
}

fn threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| physical_estimate(n.get()))
        .unwrap_or(4)
}

fn physical_estimate(logical: usize) -> usize {
    (logical / 2).max(2)
}

#[cfg(windows)]
fn assign_to_job(child: &Child) -> Option<JobHandle> {
    use std::os::windows::io::AsRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
        JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    unsafe {
        let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let _ = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        let process = HANDLE(child.as_raw_handle());
        if AssignProcessToJobObject(job, process).is_ok() {
            Some(JobHandle(job))
        } else {
            let _ = CloseHandle(job);
            None
        }
    }
}

#[cfg(windows)]
fn configure_no_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_no_window(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_core_estimate_halves_logical_with_floor() {
        assert_eq!(physical_estimate(16), 8);
        assert_eq!(physical_estimate(12), 6);
        assert_eq!(physical_estimate(8), 4);
        assert_eq!(physical_estimate(5), 2);
        assert_eq!(physical_estimate(4), 2);
        assert_eq!(physical_estimate(2), 2);
        assert_eq!(physical_estimate(1), 2);
        assert_eq!(physical_estimate(0), 2);
    }

    #[test]
    fn model_paths_compare_like_the_platform() {
        let real = r"C:\Users\puhlm\Documents\Synapse\data\models\google_gemma-3-4b-it-Q4_K_M.gguf";
        assert!(same_model_path(real, real, true));
        assert!(same_model_path(
            r"c:/users/PUHLM/Documents/Synapse/data/models/google_gemma-3-4b-it-Q4_K_M.gguf",
            real,
            true
        ));
        assert!(same_model_path(&format!(r"\\?\{real}"), real, true));
        assert!(same_model_path(
            r"C:\Users\João\Documents\Synapse\models\Gemma.gguf",
            r"C:\Users\JOÃO\Documents\Synapse\models\gemma.gguf",
            true
        ));
        assert!(!same_model_path(
            r"C:\Users\puhlm\Documents\Synapse\data\models\other.gguf",
            real,
            true
        ));
        assert!(!same_model_path("", "", true));
        assert!(!same_model_path("/home/a/Model.gguf", "/home/a/model.gguf", false));
        assert!(same_model_path("/home/a/model.gguf", "/home/a/model.gguf", false));
    }

    #[test]
    fn server_ownership_needs_our_alias_and_model() {
        let model = r"C:\Users\puhlm\Documents\Synapse\data\models\google_gemma-3-4b-it-Q4_K_M.gguf";
        let ours = serde_json::json!({
            "model_path": model,
            "model_alias": "synapse-1-2-3",
            "build_info": "b11433-50569eb87"
        });
        assert_eq!(judge_owner(200, Some(&ours), model, "synapse-1-2-3"), Owner::Ours);
        assert_eq!(judge_owner(200, Some(&ours), model, "synapse-1-2-4"), Owner::Foreign);
        let default_alias = serde_json::json!({ "model_path": model, "model_alias": model });
        assert_eq!(
            judge_owner(200, Some(&default_alias), model, "synapse-1-2-3"),
            Owner::Foreign
        );
        let other_model = serde_json::json!({
            "model_path": r"C:\models\qwen.gguf",
            "model_alias": "synapse-1-2-3"
        });
        assert_eq!(
            judge_owner(200, Some(&other_model), model, "synapse-1-2-3"),
            Owner::Foreign
        );
        assert_eq!(judge_owner(200, None, model, "synapse-1-2-3"), Owner::Foreign);
        let future = serde_json::json!({ "build_info": "b20000-abcdef", "total_slots": 1 });
        assert_eq!(
            judge_owner(200, Some(&future), model, "synapse-1-2-3"),
            Owner::Unverified
        );
        let alias_only = serde_json::json!({ "model_alias": "synapse-1-2-3" });
        assert_eq!(judge_owner(200, Some(&alias_only), model, "synapse-1-2-3"), Owner::Ours);
        let foreign_alias_only = serde_json::json!({ "model_alias": "other" });
        assert_eq!(
            judge_owner(200, Some(&foreign_alias_only), model, "synapse-1-2-3"),
            Owner::Foreign
        );
        let path_only = serde_json::json!({ "model_path": model });
        assert_eq!(judge_owner(200, Some(&path_only), model, "synapse-1-2-3"), Owner::Ours);
        let foreign_path_only = serde_json::json!({ "model_path": r"C:\models\qwen.gguf" });
        assert_eq!(
            judge_owner(200, Some(&foreign_path_only), model, "synapse-1-2-3"),
            Owner::Foreign
        );
        let not_object = serde_json::json!(["model_path"]);
        assert_eq!(judge_owner(200, Some(&not_object), model, "synapse-1-2-3"), Owner::Foreign);
        assert_eq!(judge_owner(404, None, model, "synapse-1-2-3"), Owner::Foreign);
        assert_eq!(judge_owner(503, None, model, "synapse-1-2-3"), Owner::Unknown);
    }
}
