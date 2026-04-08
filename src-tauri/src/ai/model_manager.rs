use crate::ai::{
    model_downloader::{is_model_downloaded, model_path},
    model_manifest::{get_models_for_capability, load_manifest, ManifestEntry},
    onnx_runner::OnnxRunner,
    qwen_receipt::{download_helper_model, helper_model_downloaded, QWEN_HELPER_MODEL_ID},
    AiRuntimeState,
};
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

#[derive(Debug, Serialize, Clone)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub format: String,
    pub capability: String,
    pub role: String,
    pub size_mb: f64,
    pub tier_min: String,
    /// File exists on disk with verified SHA-256.
    pub downloaded: bool,
    /// ORT session is active in memory.
    pub loaded: bool,
    /// 0.0–1.0 while downloading; None otherwise.
    pub download_progress: Option<f64>,
}

// ─── Tauri commands ───────────────────────────────────────────────────────────

/// Returns manifest entries enriched with real disk/memory status.
#[tauri::command]
pub fn list_ai_models(state: State<'_, Arc<AiRuntimeState>>) -> Vec<ModelInfo> {
    let manifest = load_manifest();
    let sessions = state.loaded_sessions.lock().unwrap();
    let progress_map = state.download_progress.lock().unwrap();

    let mut models = manifest
        .iter()
        .map(|entry| {
            let downloaded = is_model_downloaded(entry);
            let loaded = sessions.contains_key(&entry.id);
            let download_progress = progress_map.get(&entry.id).copied();
            ModelInfo {
                id: entry.id.clone(),
                name: entry.name.clone(),
                format: entry.format.clone(),
                capability: entry.capability.clone(),
                role: entry.role.clone(),
                size_mb: entry.size_mb,
                tier_min: entry.tier_min.clone(),
                downloaded,
                loaded,
                download_progress,
            }
        })
        .collect::<Vec<_>>();

    models.push(helper_model_info(&progress_map));
    models
}

/// Returns status for a single model by ID.
#[tauri::command]
pub fn get_model_status(
    model_id: String,
    state: State<'_, Arc<AiRuntimeState>>,
) -> Result<ModelInfo, String> {
    if model_id == QWEN_HELPER_MODEL_ID {
        let progress_map = state.download_progress.lock().unwrap();
        return Ok(helper_model_info(&progress_map));
    }

    let manifest = load_manifest();
    let entry = manifest
        .into_iter()
        .find(|e| e.id == model_id)
        .ok_or_else(|| format!("Model '{model_id}' not in manifest"))?;

    let sessions = state.loaded_sessions.lock().unwrap();
    let progress_map = state.download_progress.lock().unwrap();

    Ok(ModelInfo {
        downloaded: is_model_downloaded(&entry),
        loaded: sessions.contains_key(&entry.id),
        download_progress: progress_map.get(&entry.id).copied(),
        id: entry.id,
        name: entry.name,
        format: entry.format,
        capability: entry.capability,
        role: entry.role,
        size_mb: entry.size_mb,
        tier_min: entry.tier_min,
    })
}

/// Tauri command: manually trigger a model download (also available from AI Models tab).
/// Internally, the dispatcher calls `ensure_capability_ready` which handles auto-download.
#[tauri::command]
pub async fn download_model_cmd(
    model_id: String,
    state: State<'_, Arc<AiRuntimeState>>,
    app_handle: AppHandle,
) -> Result<(), String> {
    if model_id == QWEN_HELPER_MODEL_ID {
        {
            let mut downloading = state.downloading.lock().unwrap();
            if downloading.contains(QWEN_HELPER_MODEL_ID) {
                return Ok(());
            }
            downloading.insert(QWEN_HELPER_MODEL_ID.to_string());
        }
        state
            .download_progress
            .lock()
            .unwrap()
            .insert(QWEN_HELPER_MODEL_ID.to_string(), 0.05);

        let result = download_helper_model(&app_handle).await;

        {
            state
                .downloading
                .lock()
                .unwrap()
                .remove(QWEN_HELPER_MODEL_ID);
        }

        match result {
            Ok(()) => {
                state
                    .download_progress
                    .lock()
                    .unwrap()
                    .insert(QWEN_HELPER_MODEL_ID.to_string(), 1.0);
                return Ok(());
            }
            Err(error) => {
                state
                    .download_progress
                    .lock()
                    .unwrap()
                    .remove(QWEN_HELPER_MODEL_ID);
                return Err(error);
            }
        }
    }

    let manifest = load_manifest();
    let entry = manifest
        .into_iter()
        .find(|e| e.id == model_id)
        .ok_or_else(|| format!("Model '{model_id}' not in manifest"))?;

    trigger_download(entry.clone(), &**state, app_handle).await?;
    if is_model_downloaded(&entry) {
        load_session(&entry, &**state)?;
    }
    Ok(())
}

fn helper_model_info(progress_map: &std::collections::HashMap<String, f64>) -> ModelInfo {
    ModelInfo {
        id: QWEN_HELPER_MODEL_ID.to_string(),
        name: "Qwen3 Receipt Cleanup Helper".to_string(),
        format: "transformers".to_string(),
        capability: "ocr_receipt".to_string(),
        role: "receipt_cleanup".to_string(),
        size_mb: 1300.0,
        tier_min: "medium".to_string(),
        downloaded: helper_model_downloaded(),
        loaded: false,
        download_progress: progress_map.get(QWEN_HELPER_MODEL_ID).copied(),
    }
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

pub enum CapabilityStatus {
    Ready,
    Downloading(f64),
    Failed(String),
}

/// Ensures all models for a capability are downloaded and their ORT sessions loaded.
///
/// Called by `dispatcher.rs` before every `ai_task`.
/// - If already loaded: returns `Ready` immediately.
/// - If downloaded but not loaded: loads the session and returns `Ready`.
/// - If not downloaded: triggers download (once) and returns `Downloading(progress)`.
pub async fn ensure_capability_ready(
    capability: &str,
    state: &AiRuntimeState,
    app_handle: &AppHandle,
) -> CapabilityStatus {
    let models = get_models_for_capability(capability);
    if models.is_empty() {
        return CapabilityStatus::Failed(format!("No models registered for '{capability}'"));
    }

    let mut any_downloading = false;
    let mut overall_progress = 0.0_f64;

    for entry in &models {
        // Already in session cache?
        {
            let sessions = state.loaded_sessions.lock().unwrap();
            if sessions.contains_key(&entry.id) {
                continue;
            }
        }

        // Downloaded but not loaded?
        if is_model_downloaded(entry) {
            if let Err(e) = load_session(entry, state) {
                return CapabilityStatus::Failed(e);
            }
            continue;
        }

        // Not downloaded — check if already in progress
        {
            let downloading = state.downloading.lock().unwrap();
            let progress_map = state.download_progress.lock().unwrap();
            if downloading.contains(&entry.id) {
                any_downloading = true;
                overall_progress += progress_map.get(&entry.id).copied().unwrap_or(0.0);
                continue;
            }
        }

        // Trigger new download
        {
            state.downloading.lock().unwrap().insert(entry.id.clone());
            state
                .download_progress
                .lock()
                .unwrap()
                .insert(entry.id.clone(), 0.0);
        }

        let entry_clone = entry.clone();
        // Clone the inner Arc handles (not the whole state) so the spawn is safe without raw ptrs.
        let downloading = Arc::clone(&state.downloading);
        let download_progress = Arc::clone(&state.download_progress);
        let loaded_sessions = Arc::clone(&state.loaded_sessions);
        let receipt_logger = Arc::clone(&state.receipt_logger);
        let app = app_handle.clone();
        let tx = state.progress_tx.clone();

        tokio::spawn(async move {
            let result =
                crate::ai::model_downloader::download_model(entry_clone.clone(), app.clone(), tx)
                    .await;

            downloading.lock().unwrap().remove(&entry_clone.id);

            match result {
                Ok(()) => {
                    download_progress
                        .lock()
                        .unwrap()
                        .insert(entry_clone.id.clone(), 1.0);
                    // Emit model_ready over WSS (picked up by device_linking.rs listener)
                    let _ = app.emit(
                        "model-ready",
                        serde_json::json!({
                            "model_id": entry_clone.id,
                            "capability": entry_clone.capability,
                        }),
                    );
                    // Auto-load after download: build a minimal state-like struct inline
                    let runner = crate::ai::onnx_runner::OnnxRunner::load(
                        &crate::ai::model_downloader::model_path(&entry_clone),
                        &entry_clone.id,
                    );
                    match runner {
                        Ok(r) => {
                            loaded_sessions
                                .lock()
                                .unwrap()
                                .insert(entry_clone.id.clone(), Arc::new(r));
                        }
                        Err(e) => {
                            let mut entry = crate::receipt_queue::log_entry(
                                "model_manager",
                                crate::receipt_queue::OcrStage::Dispatch,
                                crate::receipt_queue::LogLevel::Error,
                                format!("Auto-load failed for {}: {e}", entry_clone.id),
                            );
                            entry.error_detail = Some(e);
                            receipt_logger.push(entry);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[ModelManager] Download failed for {}: {e}", entry_clone.id);
                    download_progress.lock().unwrap().remove(&entry_clone.id);
                }
            }
        });

        any_downloading = true;
    }

    if any_downloading {
        let count = models.len() as f64;
        CapabilityStatus::Downloading(if count > 0.0 {
            overall_progress / count
        } else {
            0.0
        })
    } else {
        CapabilityStatus::Ready
    }
}

/// Retrieve a loaded session for a model by ID. Returns None if not loaded.
pub fn get_session(model_id: &str, state: &AiRuntimeState) -> Option<Arc<OnnxRunner>> {
    state.loaded_sessions.lock().unwrap().get(model_id).cloned()
}

/// Load an ORT session from disk into the session cache.
fn load_session(entry: &ManifestEntry, state: &AiRuntimeState) -> Result<(), String> {
    let path = model_path(entry);
    let runner = OnnxRunner::load(&path, &entry.id)?;
    state
        .loaded_sessions
        .lock()
        .unwrap()
        .insert(entry.id.clone(), Arc::new(runner));
    println!("[ModelManager] Session loaded for {}", entry.id);
    Ok(())
}

/// Trigger a download for a single manifest entry (deduplicates concurrent calls).
async fn trigger_download(
    entry: ManifestEntry,
    state: &AiRuntimeState,
    app_handle: AppHandle,
) -> Result<(), String> {
    {
        let downloading = state.downloading.lock().unwrap();
        if downloading.contains(&entry.id) {
            return Ok(()); // already in progress
        }
    }
    if is_model_downloaded(&entry) {
        return Ok(()); // already on disk
    }

    {
        state.downloading.lock().unwrap().insert(entry.id.clone());
        state
            .download_progress
            .lock()
            .unwrap()
            .insert(entry.id.clone(), 0.0);
    }

    let tx = state.progress_tx.clone();
    let result = crate::ai::model_downloader::download_model(entry.clone(), app_handle, tx).await;

    {
        state.downloading.lock().unwrap().remove(&entry.id);
    }

    result
}
