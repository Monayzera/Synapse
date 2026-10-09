use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static SEQ: AtomicU64 = AtomicU64::new(0);

pub fn write_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("data");
    let tmp = path.with_file_name(format!("{name}.tmp-{}-{seq}", std::process::id()));

    {
        let mut file = create_tmp(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }

    let mut last_err: Option<io::Error> = None;
    let mut renamed = false;
    for attempt in 0..6 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => {
                renamed = true;
                break;
            }
            Err(err) => {
                last_err = Some(err);
                std::thread::sleep(Duration::from_millis(30 * (attempt + 1)));
            }
        }
    }
    if !renamed {
        let _ = std::fs::remove_file(&tmp);
        return Err(last_err.unwrap_or_else(|| io::Error::new(io::ErrorKind::Other, "rename failed")));
    }

    if let Some(parent) = path.parent() {
        let _ = fsync_dir(parent);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn restrict_existing(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(target_os = "linux")]
fn create_tmp(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(not(target_os = "linux"))]
fn create_tmp(path: &Path) -> io::Result<std::fs::File> {
    std::fs::File::create(path)
}

#[cfg(windows)]
fn fsync_dir(dir: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(dir)?;
    dir.sync_all()
}

#[cfg(not(windows))]
fn fsync_dir(dir: &Path) -> io::Result<()> {
    let dir = std::fs::File::open(dir)?;
    dir.sync_all()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{create_tmp, restrict_existing, write_durable};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn written_files_are_private() {
        let dir = std::env::temp_dir().join(format!("synapse-atomic-io-{}", std::process::id()));
        let path = dir.join("settings.json");
        write_durable(&path, b"{}").expect("file written");
        let mode = std::fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn preexisting_temp_file_becomes_private() {
        let dir = std::env::temp_dir().join(format!("synapse-atomic-io-tmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir created");
        let path = dir.join("settings.json.tmp");
        std::fs::write(&path, b"stale").expect("file written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("mode set");
        drop(create_tmp(&path).expect("temp file opened"));
        let mode = std::fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn existing_file_becomes_private() {
        let dir =
            std::env::temp_dir().join(format!("synapse-atomic-io-restrict-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir created");
        let path = dir.join("settings.json");
        std::fs::write(&path, b"{}").expect("file written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("mode set");
        restrict_existing(&path).expect("file restricted");
        let mode = std::fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn missing_file_restriction_is_ok() {
        let dir =
            std::env::temp_dir().join(format!("synapse-atomic-io-absent-{}", std::process::id()));
        let path = dir.join("settings.bak");
        assert!(restrict_existing(&path).is_ok());
        assert!(!path.exists());
    }
}
