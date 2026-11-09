//! `POST /api/speech/transcribe`: voice commands transcribed by a self-hosted
//! Whisper-compatible server, so audio never leaves your infrastructure.
//!
//! Works with both common server shapes, chosen from the configured URL:
//! * whisper.cpp `server` (`…/inference`): multipart `file`, `response_format=json`;
//! * OpenAI-compatible (`…/v1/audio/transcriptions`, faster-whisper-server, LocalAI…):
//!   multipart `file` + `model`.
//!
//! The request body is the raw audio (`audio/webm`, `audio/ogg`, `audio/wav`, `audio/mp4`),
//! capped at [`MAX_AUDIO_BYTES`].

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use boba_core::api::{SpeechProvider, Transcript};
use boba_core::Permission;
use serde_json::Value;

use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::state::SharedState;

pub const MAX_AUDIO_BYTES: usize = 10 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(60);

fn extension_for(content_type: &str) -> &'static str {
    let ct = content_type.split(';').next().unwrap_or("").trim();
    match ct {
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/ogg" => "ogg",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" => "m4a",
        "audio/mpeg" => "mp3",
        _ => "webm",
    }
}

fn is_whisper_cpp(url: &str) -> bool {
    url.trim_end_matches('/').ends_with("/inference")
}

fn extract_text(v: &Value) -> Option<String> {
    v.get("text").and_then(Value::as_str).map(|s| s.trim().to_string())
}

pub async fn transcribe(
    State(state): State<SharedState>,
    me: Auth,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Json<Transcript>> {
    me.require(Permission::View)?;
    let settings = state.db.workspace_settings(state.cfg.demo_mode).await?;
    let url = match settings.speech {
        SpeechProvider::Whisper { url } => url,
        SpeechProvider::Browser => {
            return Err(AppError::bad("speech is transcribed in the browser for this workspace"))
        }
        SpeechProvider::Disabled => return Err(AppError::bad("voice commands are disabled")),
    };
    if body.is_empty() {
        return Err(AppError::bad("empty audio"));
    }
    if body.len() > MAX_AUDIO_BYTES {
        return Err(AppError::bad("audio too long (max 10 MB)"));
    }
    let ct = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("audio/webm")
        .to_string();
    let ext = extension_for(&ct);
    let part = reqwest::multipart::Part::bytes(body.to_vec())
        .file_name(format!("command.{ext}"))
        .mime_str(ct.split(';').next().unwrap_or("audio/webm"))
        .map_err(|e| AppError::bad(format!("bad audio type: {e}")))?;
    let form = if is_whisper_cpp(&url) {
        reqwest::multipart::Form::new()
            .part("file", part)
            .text("response_format", "json")
            .text("temperature", "0.0")
    } else {
        reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", "whisper-1")
            .text("response_format", "json")
    };
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(anyhow::Error::from)?;
    let resp = client.post(&url).multipart(form).send().await.map_err(|e| {
        tracing::warn!(error = %e, "whisper request failed");
        AppError::Internal(anyhow::anyhow!("speech server unreachable"))
    })?;
    let status = resp.status();
    if !status.is_success() {
        tracing::warn!(%status, "whisper server returned an error");
        return Err(AppError::Internal(anyhow::anyhow!("speech server returned HTTP {status}")));
    }
    let v: Value = resp.json().await.map_err(|e| AppError::Internal(anyhow::anyhow!("bad speech server answer: {e}")))?;
    let text = extract_text(&v).ok_or_else(|| AppError::Internal(anyhow::anyhow!("speech server answer has no text")))?;
    Ok(Json(Transcript { text }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_extension() {
        assert_eq!(extension_for("audio/webm;codecs=opus"), "webm");
        assert_eq!(extension_for("audio/wav"), "wav");
        assert_eq!(extension_for("audio/mp4"), "m4a");
        assert_eq!(extension_for("weird"), "webm");
    }

    #[test]
    fn detects_server_shape() {
        assert!(is_whisper_cpp("http://127.0.0.1:8080/inference"));
        assert!(is_whisper_cpp("http://127.0.0.1:8080/inference/"));
        assert!(!is_whisper_cpp("http://127.0.0.1:8000/v1/audio/transcriptions"));
    }

    #[test]
    fn extracts_text() {
        assert_eq!(extract_text(&serde_json::json!({"text": "  hello boba \n"})).as_deref(), Some("hello boba"));
        assert_eq!(extract_text(&serde_json::json!({"segments": []})), None);
    }
}
