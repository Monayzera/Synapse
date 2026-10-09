use crate::atomic_io;
use ashpd::desktop::{Session, SessionPortal};
use ashpd::AppID;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::OnceCell;

const APP_ID: &str = "com.synapse.voice";
const ICON_PNG: &[u8] = include_bytes!("../icons/128x128.png");
const REGISTER_TIMEOUT: Duration = Duration::from_secs(15);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(15);

static REGISTRATION: OnceCell<Result<(), String>> = OnceCell::const_new();

pub fn is_wayland() -> bool {
    non_empty_env("WAYLAND_DISPLAY")
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value == "wayland")
}

pub fn has_x11() -> bool {
    non_empty_env("DISPLAY")
}

fn non_empty_env(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

pub async fn register() -> Result<(), String> {
    REGISTRATION.get_or_init(perform_registration).await.clone()
}

pub async fn close_abandoned<T: SessionPortal>(session: Session<T>) {
    match tokio::time::timeout(CLOSE_TIMEOUT, session.close()).await {
        Ok(Ok(())) => tracing::info!("closed a portal session whose setup was abandoned"),
        Ok(Err(err)) => tracing::warn!("closing an abandoned portal session failed: {err}"),
        Err(_) => tracing::warn!("closing an abandoned portal session did not answer in time"),
    }
}

pub async fn perform_registration() -> Result<(), String> {
    let mut problems = Vec::new();
    if let Err(err) = install_desktop_entry() {
        problems.push(err);
    }
    if let Err(err) = install_icon() {
        problems.push(err);
    }
    match AppID::try_from(APP_ID) {
        Ok(app_id) => {
            match tokio::time::timeout(REGISTER_TIMEOUT, ashpd::register_host_app(app_id)).await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => problems.push(format!(
                    "the desktop portal did not register {APP_ID}: {err}"
                )),
                Err(_) => problems.push(format!(
                    "the desktop portal did not answer the registration of {APP_ID} within {} s",
                    REGISTER_TIMEOUT.as_secs()
                )),
            }
        }
        Err(err) => problems.push(format!("invalid application id {APP_ID}: {err}")),
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

const DELETED_SUFFIX: &str = " (deleted)";

pub fn appimage_path() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn strip_deleted_suffix(path: PathBuf) -> PathBuf {
    match path
        .to_str()
        .and_then(|text| text.strip_suffix(DELETED_SUFFIX))
    {
        Some(stripped) => PathBuf::from(stripped),
        None => path,
    }
}

pub fn live_executable() -> std::io::Result<PathBuf> {
    let path = match appimage_path() {
        Some(path) => path,
        None => std::env::current_exe()?,
    };
    let live = strip_deleted_suffix(path);
    if live.try_exists()? {
        Ok(live)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{} is no longer on disk", live.display()),
        ))
    }
}

pub fn exec_value(exe: &str) -> String {
    let mut quoted = String::with_capacity(exe.len() + 2);
    quoted.push('"');
    for ch in exe.chars() {
        match ch {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(ch);
            }
            '%' => quoted.push_str("%%"),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted.replace('\\', "\\\\")
}

fn install_desktop_entry() -> Result<(), String> {
    let exe =
        live_executable().map_err(|err| format!("could not resolve the executable path: {err}"))?;
    let exe_text = exe
        .to_str()
        .ok_or_else(|| format!("the executable path is not valid UTF-8: {}", exe.display()))?;
    let contents = format!(
        "[Desktop Entry]\nType=Application\nName=Synapse\nExec={}\nIcon={APP_ID}\nTerminal=false\nNoDisplay=true\nCategories=Utility;\nStartupWMClass=synapse\n",
        exec_value(exe_text)
    );
    let path = xdg_data_home()?
        .join("applications")
        .join(format!("{APP_ID}.desktop"));
    write_if_changed(&path, contents.as_bytes())
}

fn install_icon() -> Result<(), String> {
    let path = xdg_data_home()?
        .join("icons/hicolor/128x128/apps")
        .join(format!("{APP_ID}.png"));
    write_if_changed(&path, ICON_PNG)
}

pub(crate) fn xdg_data_home() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
    {
        return Ok(dir);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .map(|home| home.join(".local/share"))
        .ok_or_else(|| "neither XDG_DATA_HOME nor HOME is set".to_string())
}

fn write_if_changed(path: &Path, contents: &[u8]) -> Result<(), String> {
    if std::fs::read(path).is_ok_and(|existing| existing == contents) {
        return Ok(());
    }
    atomic_io::write_durable(path, contents)
        .map_err(|err| format!("could not write {}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{exec_value, strip_deleted_suffix};
    use std::path::PathBuf;

    #[test]
    fn strip_deleted_suffix_restores_replaced_executable_path() {
        assert_eq!(
            strip_deleted_suffix(PathBuf::from("/usr/bin/synapse (deleted)")),
            PathBuf::from("/usr/bin/synapse")
        );
    }

    #[test]
    fn strip_deleted_suffix_keeps_plain_path() {
        assert_eq!(
            strip_deleted_suffix(PathBuf::from("/usr/bin/synapse")),
            PathBuf::from("/usr/bin/synapse")
        );
    }

    #[test]
    fn exec_value_quotes_plain_path() {
        assert_eq!(
            exec_value("/opt/synapse/synapse"),
            "\"/opt/synapse/synapse\""
        );
    }

    #[test]
    fn exec_value_escapes_quote_and_backslash_for_both_layers() {
        assert_eq!(exec_value("/a\"b"), "\"/a\\\\\"b\"");
    }

    #[test]
    fn exec_value_escapes_dollar_backtick_and_percent() {
        assert_eq!(exec_value("/x$y`z%w"), "\"/x\\\\$y\\\\`z%%w\"");
    }
}
