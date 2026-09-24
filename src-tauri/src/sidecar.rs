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
            .arg("--no-webui");

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

pub async fn wait_until_ready<F>(
    client: &reqwest::Client,
    port: u16,
    timeout: Duration,
    mut alive: F,
) -> bool
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if !alive() {
            tracing::warn!("llama-server exited while starting");
            return false;
        }
        if health_ok(client, port).await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
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
        .map(|n| (n.get() / 2).max(2))
        .unwrap_or(4)
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
