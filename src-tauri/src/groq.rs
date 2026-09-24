use crate::error::{AppError, AppResult};
use reqwest::multipart::{Form, Part};
use serde::Deserialize;

const TRANSCRIBE_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const TRANSLATE_URL: &str = "https://api.groq.com/openai/v1/audio/translations";
const TRANSLATE_MODEL: &str = "whisper-large-v3";

#[derive(Deserialize)]
struct GroqText {
    text: String,
}

pub async fn transcribe(
    http: &reqwest::Client,
    api_key: &str,
    samples: &[f32],
    language: Option<&str>,
    prompt: Option<&str>,
    translate: bool,
    model: &str,
) -> AppResult<String> {
    if samples.is_empty() {
        return Ok(String::new());
    }
    let wav = wav_16k_mono(samples);
    let (url, model) = if translate {
        (TRANSLATE_URL, TRANSLATE_MODEL)
    } else {
        (TRANSCRIBE_URL, model)
    };
    let part = Part::bytes(wav)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| AppError::Transcribe(e.to_string()))?;
    let mut form = Form::new()
        .part("file", part)
        .text("model", model.to_string())
        .text("response_format", "json")
        .text("temperature", "0");
    if !translate {
        if let Some(lang) = language {
            form = form.text("language", lang.to_string());
        }
    }
    if let Some(p) = prompt {
        if !p.trim().is_empty() {
            form = form.text("prompt", p.to_string());
        }
    }
    let response = http
        .post(url)
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| AppError::Transcribe(format!("Groq request failed: {e}")))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| AppError::Transcribe(e.to_string()))?;
    if !status.is_success() {
        let snippet: String = body.chars().take(300).collect();
        return Err(AppError::Transcribe(format!("Groq {status}: {snippet}")));
    }
    let parsed: GroqText = serde_json::from_str(&body)
        .map_err(|e| AppError::Transcribe(format!("Groq response parse failed: {e}")))?;
    Ok(parsed.text.trim().to_string())
}

fn wav_16k_mono(samples: &[f32]) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let mut buf = Vec::with_capacity(44 + data_len);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&16000u32.to_le_bytes());
    buf.extend_from_slice(&32000u32.to_le_bytes());
    buf.extend_from_slice(&2u16.to_le_bytes());
    buf.extend_from_slice(&16u16.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&(data_len as u32).to_le_bytes());
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        buf.extend_from_slice(&v.to_le_bytes());
    }
    buf
}
