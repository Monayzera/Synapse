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
        let mut file = std::fs::File::create(&tmp)?;
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
