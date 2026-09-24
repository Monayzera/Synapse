use crate::models::ModelKind;
use serde::Serialize;
use std::time::Duration;

#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HfParse {
    File {
        repo: Option<String>,
        filename: String,
        url: String,
        guessed: Option<ModelKind>,
    },
    Repo {
        repo: String,
    },
    Invalid {
        reason: String,
    },
}

#[derive(Serialize, Clone)]
pub struct HfFile {
    pub filename: String,
    pub url: String,
    pub size_bytes: u64,
    pub guessed: Option<ModelKind>,
}

pub fn guess_kind(filename: &str) -> Option<ModelKind> {
    let lower = filename.to_ascii_lowercase();
    if lower.ends_with(".gguf") {
        Some(ModelKind::Llm)
    } else if lower.ends_with(".bin") {
        Some(ModelKind::Whisper)
    } else {
        None
    }
}

fn strip_host(url: &str) -> Option<&str> {
    let trimmed = url.trim();
    let no_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let no_www = no_scheme.strip_prefix("www.").unwrap_or(no_scheme);
    let rest = no_www.strip_prefix("huggingface.co/")?;
    Some(rest.split(['?', '#']).next().unwrap_or(rest))
}

pub fn detect(url: &str) -> HfParse {
    let path = match strip_host(url) {
        Some(p) if !p.trim().is_empty() => p.trim_matches('/'),
        _ => {
            return HfParse::Invalid {
                reason: "Use a Hugging Face link (huggingface.co/...).".to_string(),
            }
        }
    };

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 2 {
        return HfParse::Invalid {
            reason: "Incomplete link. Paste the repository or file link.".to_string(),
        };
    }

    let org = segments[0];
    let repo = segments[1];
    let repo_id = format!("{org}/{repo}");

    if let Some(marker) = segments.iter().position(|s| *s == "resolve" || *s == "blob") {
        let after = &segments[marker + 1..];
        if after.len() < 2 {
            return HfParse::Invalid {
                reason: "Incomplete file link.".to_string(),
            };
        }
        let branch = after[0];
        let filepath = after[1..].join("/");
        let filename = after.last().copied().unwrap_or("").to_string();
        if filename.is_empty() {
            return HfParse::Invalid {
                reason: "Could not identify the file in the link.".to_string(),
            };
        }
        let download_url =
            format!("https://huggingface.co/{org}/{repo}/resolve/{branch}/{filepath}");
        return HfParse::File {
            repo: Some(repo_id),
            filename: filename.clone(),
            url: download_url,
            guessed: guess_kind(&filename),
        };
    }

    HfParse::Repo { repo: repo_id }
}

#[tauri::command]
pub async fn hf_detect(url: String) -> Result<HfParse, String> {
    Ok(detect(&url))
}

#[tauri::command]
pub async fn hf_list_files(repo: String) -> Result<Vec<HfFile>, String> {
    let repo = repo.trim().trim_matches('/').to_string();
    if repo.split('/').filter(|s| !s.is_empty()).count() != 2 {
        return Err("Invalid repository.".to_string());
    }

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .user_agent("Synapse")
        .build()
        .map_err(|e| e.to_string())?;

    let api = format!("https://huggingface.co/api/models/{repo}");
    let response = client
        .get(&api)
        .send()
        .await
        .map_err(|e| format!("No connection or repository unreachable: {e}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "Repository not found on Hugging Face ({}).",
            response.status()
        ));
    }

    let value: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    let siblings = value
        .get("siblings")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();

    let mut files = Vec::new();
    for sibling in siblings {
        let rfilename = match sibling.get("rfilename").and_then(|v| v.as_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };
        let lower = rfilename.to_ascii_lowercase();
        if !(lower.ends_with(".gguf") || lower.ends_with(".bin")) {
            continue;
        }
        let size_bytes = sibling
            .pointer("/lfs/size")
            .and_then(|v| v.as_u64())
            .or_else(|| sibling.get("size").and_then(|v| v.as_u64()))
            .unwrap_or(0);
        let leaf = rfilename
            .rsplit('/')
            .next()
            .unwrap_or(&rfilename)
            .to_string();
        files.push(HfFile {
            url: format!("https://huggingface.co/{repo}/resolve/main/{rfilename}"),
            guessed: guess_kind(&leaf),
            filename: leaf,
            size_bytes,
        });
    }

    if files.is_empty() {
        return Err("No .gguf or .bin files in this repository.".to_string());
    }

    Ok(files)
}
