use std::path::{Path, PathBuf};

fn main() {
    tauri_build::build();
    delay_load_cuda();
}

fn delay_load_cuda() {
    println!("cargo:rerun-if-env-changed=CUDA_PATH");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows"
        || target_env != "msvc"
        || std::env::var_os("CARGO_FEATURE_CUDA").is_none()
    {
        return;
    }

    let cuda_path = std::env::var_os("CUDA_PATH").map(PathBuf::from);
    let mut dlls: Vec<String> = Vec::new();
    for prefix in ["cudart64_", "cublas64_", "cublasLt64_"] {
        let found = cuda_path
            .as_deref()
            .map(|root| runtime_dlls(root, prefix))
            .unwrap_or_default();
        if found.is_empty() {
            let major = cuda_path.as_deref().and_then(cuda_major).unwrap_or(12);
            let name = format!("{prefix}{major}.dll");
            println!("cargo:warning={prefix}*.dll not found under CUDA_PATH; delay-loading {name}");
            dlls.push(name);
        } else {
            dlls.extend(found);
        }
    }
    dlls.push("nvcuda.dll".to_string());

    for dll in &dlls {
        println!("cargo:rustc-link-arg=/DELAYLOAD:{dll}");
    }
    println!("cargo:rustc-link-arg=delayimp.lib");
}

fn runtime_dlls(root: &Path, prefix: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for dir in [root.join("bin"), root.join("bin").join("x64")] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if is_runtime_dll(&name, prefix)
                && !out.iter().any(|known| known.eq_ignore_ascii_case(&name))
            {
                out.push(name);
            }
        }
    }
    out
}

fn is_runtime_dll(name: &str, prefix: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix(&prefix.to_ascii_lowercase()) else {
        return false;
    };
    let Some(version) = rest.strip_suffix(".dll") else {
        return false;
    };
    !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit())
}

fn cuda_major(root: &Path) -> Option<u32> {
    let name = root.file_name()?.to_str()?;
    name.trim_start_matches(['v', 'V'])
        .split('.')
        .next()?
        .parse()
        .ok()
}
