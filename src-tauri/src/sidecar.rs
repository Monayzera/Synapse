use crate::error::{AppError, AppResult};
use std::path::Path;
use std::process::{Child, Command};
use std::time::Duration;

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

        let child = command
            .spawn()
            .map_err(|e| AppError::Llm(format!("failed to start llama-server: {e}")))?;

        #[cfg(windows)]
        let job = assign_to_job(&child);

        Ok(Sidecar {
            child: Some(child),
            #[cfg(windows)]
            job,
        })
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

pub async fn wait_until_ready(client: &reqwest::Client, port: u16, timeout: Duration) -> bool {
    let url = format!("http://127.0.0.1:{port}/health");
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(Ok(response)) =
            tokio::time::timeout(Duration::from_secs(2), client.get(&url).send()).await
        {
            if response.status().is_success() {
                return true;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
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
