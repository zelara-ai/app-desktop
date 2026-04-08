pub mod categorization;
pub mod dispatcher;
pub mod hardware_tier;
pub mod model_downloader;
pub mod model_manager;
pub mod model_manifest;
pub mod ocr_receipt;
pub mod onnx_runner;
pub mod qwen_receipt;

pub use qwen_receipt::PersistentQwenProcess;

use hardware_tier::HardwareTier;
use onnx_runner::OnnxRunner;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

// ─── Task types ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AiTaskRequest {
    pub task_id: String,
    pub capability: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize, Clone)]
pub struct AiTaskResponse {
    pub task_id: String,
    pub capability: String,
    pub success: bool,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    /// Present when error == "model_downloading" (0.0–1.0).
    pub download_progress: Option<f64>,
}

#[derive(Debug)]
pub enum AiTaskError {
    NotYetImplemented,
    ModelNotLoaded(String),
    /// Model is downloading; mobile waits for `model_ready` WSS push then retries.
    ModelDownloading {
        capability: String,
        progress: f64,
    },
    ProcessingFailed(String),
    /// Device tier is below what this capability requires.
    TierInsufficient {
        required: String,
        current: String,
    },
    /// Capability handler exceeded the per-task time budget.
    Timeout {
        capability: String,
        elapsed_secs: u64,
    },
}

impl AiTaskError {
    pub fn to_message(&self) -> String {
        match self {
            AiTaskError::NotYetImplemented => "Capability not yet implemented".to_string(),
            AiTaskError::ModelNotLoaded(name) => format!("Model not loaded: {name}"),
            AiTaskError::ModelDownloading {
                capability,
                progress,
            } => {
                format!("model_downloading:{capability}:{progress:.2}")
            }
            AiTaskError::ProcessingFailed(msg) => format!("Processing failed: {msg}"),
            AiTaskError::TierInsufficient { required, current } => {
                format!("Tier insufficient: requires {required}, current {current}")
            }
            AiTaskError::Timeout {
                capability,
                elapsed_secs,
            } => {
                format!("Task timeout: {capability} exceeded {elapsed_secs}s budget")
            }
        }
    }

    pub fn error_code(&self) -> &'static str {
        match self {
            AiTaskError::ModelDownloading { .. } => "model_downloading",
            AiTaskError::TierInsufficient { .. } => "tier_insufficient",
            AiTaskError::NotYetImplemented => "not_implemented",
            AiTaskError::ModelNotLoaded(_) => "model_not_loaded",
            AiTaskError::ProcessingFailed(_) => "processing_failed",
            AiTaskError::Timeout { .. } => "task_timeout",
        }
    }
}

// ─── Download progress broadcast ─────────────────────────────────────────────

/// Sent on the broadcast channel for every download progress tick.
/// `progress == 1.0` signals successful completion; `< 0.0` signals error.
#[derive(Debug, Clone)]
pub struct DownloadProgressEvent {
    pub model_id: String,
    pub capability: String,
    /// 0.0–1.0 during download; 1.0 = complete; -1.0 = error.
    pub progress: f64,
}

// ─── Recent AI activity ring buffer ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct AiActivityEntry {
    pub task_id: String,
    pub capability: String,
    /// Short human-readable summary (e.g. "WALMART $52.40 → groceries").
    pub summary: String,
    pub timestamp: String,
    pub status: String,
    pub error: Option<String>,
    pub metadata: serde_json::Value,
}

// ─── Runtime state (Tauri-managed) ───────────────────────────────────────────

pub struct AiRuntimeState {
    /// Detected once at startup.
    pub tier: HardwareTier,
    /// Loaded ONNX sessions keyed by model_id.
    /// Wrapped in Arc so the download-completion spawn can access it without raw pointers.
    pub loaded_sessions: Arc<Mutex<HashMap<String, Arc<OnnxRunner>>>>,
    /// model_ids currently being downloaded (prevents duplicate downloads).
    pub downloading: Arc<Mutex<HashSet<String>>>,
    /// Latest known download progress per model_id (0.0–1.0).
    pub download_progress: Arc<Mutex<HashMap<String, f64>>>,
    /// Broadcast channel — device_linking.rs subscribes to forward progress
    /// to connected mobile clients as WSS push messages.
    pub progress_tx: broadcast::Sender<DownloadProgressEvent>,
    /// Ring buffer of the last 50 ai_task updates shown in Desktop Finance tab.
    pub recent_activity: Mutex<Vec<AiActivityEntry>>,
    /// Structured per-receipt OCR pipeline log (ring buffer + JSONL file).
    /// Call `receipt_logger.set_log_dir()` from lib.rs `.setup()` after app data dir is known.
    pub receipt_logger: Arc<crate::receipt_queue::ReceiptLogger>,
    /// True if `python --version` succeeded at startup and returned Python ≥ 3.9.
    /// Checked by `should_refine_with_qwen()` to fail fast instead of waiting 18s.
    pub python_available: bool,
    /// Persistent Qwen3 subprocess (spawned lazily on first OCR refine call).
    /// Eliminates per-call model load latency (~3–8s saved per invocation).
    pub persistent_qwen: Arc<tokio::sync::Mutex<Option<PersistentQwenProcess>>>,
}

impl AiRuntimeState {
    pub fn new() -> Self {
        let (progress_tx, _) = broadcast::channel(64);

        // Synchronous Python availability check — fast (< 500ms), runs once at startup.
        // We try `python` then `python3` as a fallback for systems that use the latter.
        let python_available = check_python_available();

        Self {
            tier: hardware_tier::detect_tier(),
            loaded_sessions: Arc::new(Mutex::new(HashMap::new())),
            downloading: Arc::new(Mutex::new(HashSet::new())),
            download_progress: Arc::new(Mutex::new(HashMap::new())),
            progress_tx,
            recent_activity: Mutex::new(Vec::new()),
            receipt_logger: Arc::new(crate::receipt_queue::ReceiptLogger::new()),
            python_available,
            persistent_qwen: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// Upsert an ai_task update in the ring buffer (max 50 entries, oldest dropped).
    pub fn upsert_activity(&self, entry: AiActivityEntry) {
        let mut buf = self.recent_activity.lock().unwrap();
        if let Some(existing) = buf
            .iter_mut()
            .find(|current| current.task_id == entry.task_id)
        {
            *existing = entry;
            return;
        }

        if buf.len() >= 50 {
            buf.remove(0);
        }
        buf.push(entry);
    }
}

// ─── Python availability check ───────────────────────────────────────────────

/// Try `python --version` (then `python3`) at startup to detect Python ≥ 3.9.
/// Returns `false` if Python is absent or too old — Qwen3 will fail fast instead
/// of waiting 18 seconds for the cold-spawn to discover this.
fn check_python_available() -> bool {
    for cmd in &["python", "python3"] {
        if let Ok(output) = std::process::Command::new(cmd).arg("--version").output() {
            if !output.status.success() {
                continue;
            }
            // `python --version` prints to stdout on Py3, stderr on some Py2 builds.
            let text = String::from_utf8_lossy(&output.stdout).to_string()
                + &String::from_utf8_lossy(&output.stderr);
            // Accept "Python 3.9" and above; reject Python 2 entirely.
            if let Some(version) = text.trim().strip_prefix("Python ") {
                let parts: Vec<&str> = version.splitn(3, '.').collect();
                let major: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                let minor: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                if major > 3 || (major == 3 && minor >= 9) {
                    println!("[AiRuntime] Python available: {}", text.trim());
                    return true;
                }
            }
        }
    }
    println!("[AiRuntime] Python not available or < 3.9 — Qwen3 fallback disabled");
    false
}

// ─── Tauri command: recent activity ──────────────────────────────────────────

#[tauri::command]
pub fn get_ai_activity(
    state: tauri::State<'_, std::sync::Arc<AiRuntimeState>>,
) -> Vec<AiActivityEntry> {
    state.recent_activity.lock().unwrap().clone()
}

// ─── Tauri command: structured OCR log ───────────────────────────────────────

/// Return structured OCR pipeline log entries.
/// If `job_id` is provided, returns only entries for that receipt; otherwise returns all (most recent first).
#[tauri::command]
pub fn get_receipt_log(
    job_id: Option<String>,
    state: tauri::State<'_, std::sync::Arc<AiRuntimeState>>,
) -> Vec<crate::receipt_queue::ReceiptLogEntry> {
    let mut entries = state.receipt_logger.get_entries(job_id.as_deref());
    entries.reverse(); // most recent first
    entries
}
