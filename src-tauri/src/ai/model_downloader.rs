use crate::ai::{model_manifest::ManifestEntry, DownloadProgressEvent};
use dirs;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};

/// Root directory where all models are cached on disk.
/// `%APPDATA%\zelara-core\models\` on Windows.
pub fn models_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("zelara-core")
        .join("models")
}

/// Path where a model's ONNX file will be stored.
/// Layout: `<models_dir>/<model_id>/model.onnx`
pub fn model_path(entry: &ManifestEntry) -> PathBuf {
    models_dir().join(&entry.id).join("model.onnx")
}

/// Returns true if the model file exists on disk with a verified SHA-256.
/// During development, placeholder hashes (`TO_BE_LOCKED_IN_*`) always pass.
pub fn is_model_downloaded(entry: &ManifestEntry) -> bool {
    let path = model_path(entry);
    if !path.exists() {
        return false;
    }
    verify_sha256(&path, &entry.sha256)
}

fn verify_sha256(path: &Path, expected: &str) -> bool {
    if expected.starts_with("TO_BE_LOCKED") {
        return true; // placeholder — skip verification in dev
    }
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let hash = format!("{:x}", Sha256::digest(&bytes));
    hash == expected.to_lowercase()
}

/// Download a model from its manifest URL, verify SHA-256, and persist to disk.
///
/// Emits Tauri events (for Desktop React UI):
///   - `model-download-progress` → `{ model_id, capability, progress: f64 }`
///   - `model-download-complete` → `{ model_id, capability }`
///   - `model-download-error`    → `{ model_id, error: string }`
///
/// Sends on `progress_tx` broadcast (for WSS push to mobile clients).
/// `progress == 1.0` signals completion; `-1.0` signals error.
pub async fn download_model(
    entry: ManifestEntry,
    app_handle: AppHandle,
    progress_tx: tokio::sync::broadcast::Sender<DownloadProgressEvent>,
) -> Result<(), String> {
    let path = model_path(&entry);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Cannot create model directory: {e}"))?;
    }

    println!(
        "[ModelDownloader] Downloading {} from {}",
        entry.id, entry.download_url
    );

    let client = reqwest::Client::new();
    let response = client
        .get(&entry.download_url)
        .send()
        .await
        .map_err(|e| format!("Download request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let _ = emit_error(&app_handle, &progress_tx, &entry, &format!("HTTP {status}"));
        return Err(format!("Download failed with HTTP {status}"));
    }

    let total_bytes = response.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut hasher = Sha256::new();
    let mut buffer: Vec<u8> = Vec::with_capacity(total_bytes as usize + 1024);

    let mut stream = response.bytes_stream();
    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| format!("Download stream error: {e}"))?;
        hasher.update(&chunk);
        buffer.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;

        let progress = if total_bytes > 0 {
            (downloaded as f64 / total_bytes as f64).min(0.99)
        } else {
            0.5
        };

        emit_progress(&app_handle, &progress_tx, &entry, progress);
    }

    // Verify SHA-256 before writing
    if !entry.sha256.starts_with("TO_BE_LOCKED") {
        let hash = format!("{:x}", hasher.finalize());
        if hash != entry.sha256.to_lowercase() {
            let msg = format!(
                "SHA-256 mismatch for {} — download may be corrupted",
                entry.id
            );
            let _ = emit_error(&app_handle, &progress_tx, &entry, &msg);
            return Err(msg);
        }
    }

    std::fs::write(&path, &buffer).map_err(|e| format!("Write failed: {e}"))?;

    println!("[ModelDownloader] {} saved to {:?}", entry.id, path);

    // Signal 100% complete
    emit_progress(&app_handle, &progress_tx, &entry, 1.0);
    let _ = app_handle.emit(
        "model-download-complete",
        serde_json::json!({ "model_id": entry.id, "capability": entry.capability }),
    );

    Ok(())
}

fn emit_progress(
    app_handle: &AppHandle,
    tx: &tokio::sync::broadcast::Sender<DownloadProgressEvent>,
    entry: &ManifestEntry,
    progress: f64,
) {
    let _ = app_handle.emit(
        "model-download-progress",
        serde_json::json!({
            "model_id": entry.id,
            "capability": entry.capability,
            "progress": progress,
        }),
    );
    let _ = tx.send(DownloadProgressEvent {
        model_id: entry.id.clone(),
        capability: entry.capability.clone(),
        progress,
    });
}

fn emit_error(
    app_handle: &AppHandle,
    tx: &tokio::sync::broadcast::Sender<DownloadProgressEvent>,
    entry: &ManifestEntry,
    error: &str,
) -> Result<(), ()> {
    let _ = app_handle.emit(
        "model-download-error",
        serde_json::json!({ "model_id": entry.id, "error": error }),
    );
    tx.send(DownloadProgressEvent {
        model_id: entry.id.clone(),
        capability: entry.capability.clone(),
        progress: -1.0,
    })
    .map(|_| ())
    .map_err(|_| ())
}
