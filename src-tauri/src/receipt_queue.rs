use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
// NOTE: image_base64 was removed from ReceiptJob in v0.6.0 Phase 5B.
// Old manifests that contain the field will deserialize without error (serde ignores unknown fields).

// ─── Structured OCR logging ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OcrStage {
    Preprocess,
    WindowsOcr,
    PaddleDetect,
    PaddleRecognize,
    Parse,
    Qwen3,
    Categorize,
    Dispatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LogLevel {
    Info,
    Warning,
    Error,
}

/// Scores for each preprocessing variant tried during OCR extraction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VariantScore {
    pub label: String,
    pub score: i32,
    pub chars: usize,
    pub selected: bool,
}

/// One structured log entry for a receipt processing pipeline step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptLogEntry {
    pub timestamp: String,
    pub job_id: String,
    pub stage: OcrStage,
    pub level: LogLevel,
    pub message: String,
    pub duration_ms: Option<u64>,
    pub error_detail: Option<String>,
    /// Present for the WindowsOcr/PaddleDetect stages: which variant was selected and why.
    pub variant_scores: Option<Vec<VariantScore>>,
}

/// In-memory ring buffer (max 500 entries) + JSONL file writer for OCR pipeline logs.
///
/// Thread-safe: all methods take `&self`.
pub struct ReceiptLogger {
    entries: Mutex<VecDeque<ReceiptLogEntry>>,
    log_dir: Mutex<Option<PathBuf>>,
}

impl ReceiptLogger {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            log_dir: Mutex::new(None),
        }
    }

    /// Set the directory for the JSONL log file. Call this once from `.setup()` after
    /// the app data directory is known. Creates `{dir}/logs/` if needed.
    pub fn set_log_dir(&self, app_data_dir: PathBuf) {
        let log_dir = app_data_dir.join("logs");
        let _ = fs::create_dir_all(&log_dir);
        *self.log_dir.lock().unwrap() = Some(log_dir);
    }

    /// Push a log entry to the ring buffer and schedule a non-blocking file write.
    pub fn push(&self, entry: ReceiptLogEntry) {
        // Add to in-memory ring
        {
            let mut ring = self.entries.lock().unwrap();
            if ring.len() >= 500 {
                ring.pop_front();
            }
            ring.push_back(entry.clone());
        }

        // Append to JSONL file — best-effort, non-blocking
        let log_dir = self.log_dir.lock().unwrap().clone();
        if let Some(dir) = log_dir {
            if let Ok(line) = serde_json::to_string(&entry) {
                // spawn_blocking so file I/O never blocks the async runtime thread
                let _ = tokio::task::spawn_blocking(move || {
                    append_log_line(&dir, &line);
                });
            }
        }
    }

    /// Return all entries, optionally filtered to a single job_id.
    pub fn get_entries(&self, job_id: Option<&str>) -> Vec<ReceiptLogEntry> {
        let ring = self.entries.lock().unwrap();
        match job_id {
            Some(id) => ring.iter().filter(|e| e.job_id == id).cloned().collect(),
            None => ring.iter().cloned().collect(),
        }
    }
}

/// Append a single JSONL line to the log file; rotate at 10 MB.
fn append_log_line(log_dir: &PathBuf, line: &str) {
    let log_path = log_dir.join("receipt_ocr.jsonl");
    // Rotation: if file exceeds 10 MB, archive to .1 and start fresh
    if let Ok(meta) = fs::metadata(&log_path) {
        if meta.len() > 10 * 1024 * 1024 {
            let rotated = log_dir.join("receipt_ocr.1.jsonl");
            let _ = fs::rename(&log_path, &rotated);
        }
    }
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// Helper: build a ReceiptLogEntry with the current UTC timestamp.
pub fn log_entry(
    job_id: impl Into<String>,
    stage: OcrStage,
    level: LogLevel,
    message: impl Into<String>,
) -> ReceiptLogEntry {
    ReceiptLogEntry {
        timestamp: chrono::Utc::now().to_rfc3339(),
        job_id: job_id.into(),
        stage,
        level,
        message: message.into(),
        duration_ms: None,
        error_detail: None,
        variant_scores: None,
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptCaptureQuality {
    pub accepted: bool,
    pub score: f64,
    pub issues: Vec<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub file_size: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptDraft {
    pub amount: f64,
    pub currency: String,
    pub description: String,
    pub merchant: Option<String>,
    pub date: Option<String>,
    pub category_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptJob {
    pub receipt_id: String,
    pub device_id: String,
    pub status: String,
    pub captured_at: String,
    pub updated_at: String,
    pub retry_count: u32,
    pub image_path: String,
    pub mime_type: String,
    pub capture_quality: ReceiptCaptureQuality,
    pub error: Option<String>,
    pub review_reason: Option<String>,
    pub review_fields: Vec<String>,
    pub readiness_score: Option<f64>,
    pub field_confidence: Option<serde_json::Value>,
    pub field_evidence: Option<serde_json::Value>,
    pub field_suggestions: Option<serde_json::Value>,
    pub processing_trace: Option<serde_json::Value>,
    pub stage_timings: serde_json::Value,
    pub ocr_result: Option<serde_json::Value>,
    pub draft: Option<ReceiptDraft>,
}

impl ReceiptJob {
    pub fn display_status(&self) -> &str {
        self.status.as_str()
    }
}

/// Maximum number of times a receipt job will be retried before it is permanently failed.
pub const MAX_JOB_RETRIES: u32 = 5;
const STALE_PENDING_JOB_HOURS: i64 = 6;

pub struct ReceiptQueueState {
    jobs: Arc<Mutex<Vec<ReceiptJob>>>,
    pub processing: Arc<Mutex<bool>>,
    /// Set to `true` on app shutdown to stop the processing loop cleanly.
    shutdown: Arc<AtomicBool>,
}

impl ReceiptQueueState {
    pub fn new() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(load_jobs().unwrap_or_default())),
            processing: Arc::new(Mutex::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Signal the processing loop to stop after the current job.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }

    /// Returns `true` if the app is shutting down.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }

    pub fn list_jobs(&self) -> Vec<ReceiptJob> {
        self.reconcile_jobs();
        let mut jobs = self.jobs.lock().unwrap().clone();
        jobs.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        jobs
    }

    pub fn has_pending_jobs(&self) -> bool {
        self.reconcile_jobs();
        self.jobs
            .lock()
            .unwrap()
            .iter()
            .any(|job| job.status == "queued")
    }

    pub fn next_pending_job(&self) -> Option<ReceiptJob> {
        self.reconcile_jobs();
        self.jobs
            .lock()
            .unwrap()
            .iter()
            .find(|job| job.status == "queued")
            .cloned()
    }

    fn reconcile_jobs(&self) {
        let mut jobs = self.jobs.lock().unwrap();
        let (next_jobs, changed) = reconcile_loaded_jobs(jobs.clone());
        if !changed {
            return;
        }

        *jobs = next_jobs;
        if let Err(error) = persist_jobs(&jobs) {
            eprintln!("[ReceiptQueue] Failed to persist reconciled jobs: {error}");
        }
    }

    pub fn save_uploaded_job(
        &self,
        receipt_id: String,
        device_id: String,
        captured_at: String,
        mime_type: String,
        image_base64: String,
        capture_quality: ReceiptCaptureQuality,
    ) -> Result<ReceiptJob, String> {
        let image_bytes = general_purpose::STANDARD
            .decode(&image_base64)
            .map_err(|error| format!("Invalid receipt image payload: {error}"))?;
        let images_dir = receipt_jobs_dir()?.join("images");
        fs::create_dir_all(&images_dir)
            .map_err(|error| format!("Failed to create receipt image dir: {error}"))?;

        let extension = match mime_type.as_str() {
            "image/png" => "png",
            _ => "jpg",
        };
        let image_path = images_dir.join(format!("{receipt_id}.{extension}"));
        fs::write(&image_path, image_bytes)
            .map_err(|error| format!("Failed to persist receipt image: {error}"))?;

        let now = chrono::Utc::now().to_rfc3339();
        let job = ReceiptJob {
            receipt_id: receipt_id.clone(),
            device_id,
            status: "queued".to_string(),
            captured_at,
            updated_at: now,
            retry_count: 0,
            image_path: image_path.to_string_lossy().to_string(),
            mime_type,
            capture_quality,
            error: None,
            review_reason: None,
            review_fields: Vec::new(),
            readiness_score: None,
            field_confidence: None,
            field_evidence: None,
            field_suggestions: None,
            processing_trace: None,
            stage_timings: serde_json::json!({}),
            ocr_result: None,
            draft: None,
        };

        let mut jobs = self.jobs.lock().unwrap();
        if let Some(existing) = jobs
            .iter_mut()
            .find(|existing| existing.receipt_id == receipt_id)
        {
            *existing = job.clone();
        } else {
            jobs.push(job.clone());
        }
        persist_jobs(&jobs)?;
        Ok(job)
    }

    pub fn update_job<F>(&self, receipt_id: &str, mutate: F) -> Result<Option<ReceiptJob>, String>
    where
        F: FnOnce(&mut ReceiptJob),
    {
        let mut jobs = self.jobs.lock().unwrap();
        let Some(index) = jobs.iter().position(|job| job.receipt_id == receipt_id) else {
            return Ok(None);
        };
        mutate(&mut jobs[index]);
        jobs[index].updated_at = chrono::Utc::now().to_rfc3339();
        let snapshot = jobs[index].clone();
        persist_jobs(&jobs)?;
        Ok(Some(snapshot))
    }
}

#[tauri::command]
pub fn get_receipt_jobs(
    state: tauri::State<'_, std::sync::Arc<ReceiptQueueState>>,
) -> Vec<ReceiptJob> {
    state.list_jobs()
}

fn load_jobs() -> Result<Vec<ReceiptJob>, String> {
    let path = receipt_manifest_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(&path)
        .map_err(|error| format!("Failed to read receipt queue: {error}"))?;
    let jobs: Vec<ReceiptJob> = serde_json::from_str(&content)
        .map_err(|error| format!("Failed to parse receipt queue: {error}"))?;
    let (jobs, changed) = reconcile_loaded_jobs(jobs);
    if changed {
        persist_jobs(&jobs)?;
    }
    Ok(jobs)
}

fn persist_jobs(jobs: &[ReceiptJob]) -> Result<(), String> {
    let path = receipt_manifest_path()?;
    let content = serde_json::to_string_pretty(jobs)
        .map_err(|error| format!("Failed to serialize receipt queue: {error}"))?;
    fs::write(path, content).map_err(|error| format!("Failed to write receipt queue: {error}"))
}

fn receipt_jobs_dir() -> Result<PathBuf, String> {
    let dir = crate::storage::get_data_dir()?.join("receipt-jobs");
    fs::create_dir_all(&dir).map_err(|error| format!("Failed to create receipt dir: {error}"))?;
    Ok(dir)
}

fn receipt_manifest_path() -> Result<PathBuf, String> {
    Ok(receipt_jobs_dir()?.join("receipt-jobs.json"))
}

fn reconcile_loaded_jobs(jobs: Vec<ReceiptJob>) -> (Vec<ReceiptJob>, bool) {
    let mut changed = false;
    let now = chrono::Utc::now();
    let now_rfc3339 = now.to_rfc3339();
    let mut next_jobs = Vec::with_capacity(jobs.len());

    for mut job in jobs {
        if job.status == "saved" {
            changed = true;
            let _ = fs::remove_file(&job.image_path);
            continue;
        }

        if receipt_image_is_corrupt(&job.image_path) {
            changed = true;
            let _ = fs::remove_file(&job.image_path);
            eprintln!(
                "[ReceiptQueue] Removing corrupt receipt {} because its image payload is missing or unreadable",
                job.receipt_id
            );
            continue;
        }

        if is_stale_pending_job(&job, now) {
            job.status = "corrupt".to_string();
            job.error = Some("stale_pending_receipt".to_string());
            if job.review_reason.is_none() {
                job.review_reason = Some(format!(
                    "Receipt request expired after waiting more than {STALE_PENDING_JOB_HOURS} hours without a completed OCR result."
                ));
            }
            job.updated_at = now_rfc3339.clone();
            changed = true;
        }

        if matches!(job.status.as_str(), "running" | "waiting_for_model") {
            job.status = "queued".to_string();
            job.error = None;
            job.updated_at = now_rfc3339.clone();
            changed = true;
        }

        next_jobs.push(job);
    }

    (next_jobs, changed)
}

fn receipt_image_is_corrupt(image_path: &str) -> bool {
    let path = PathBuf::from(image_path);
    if !path.exists() {
        return true;
    }

    let Ok(metadata) = fs::metadata(&path) else {
        return true;
    };
    if !metadata.is_file() || metadata.len() == 0 {
        return true;
    }

    let Ok(bytes) = fs::read(&path) else {
        return true;
    };
    image::load_from_memory(&bytes).is_err()
}

fn is_stale_pending_job(job: &ReceiptJob, now: chrono::DateTime<chrono::Utc>) -> bool {
    if !matches!(
        job.status.as_str(),
        "queued" | "running" | "waiting_for_model"
    ) {
        return false;
    }

    if job.draft.is_some() || job.ocr_result.is_some() {
        return false;
    }

    let timestamp = parse_job_timestamp(&job.updated_at)
        .or_else(|| parse_job_timestamp(&job.captured_at));
    let Some(timestamp) = timestamp else {
        return false;
    };

    now.signed_duration_since(timestamp) >= chrono::Duration::hours(STALE_PENDING_JOB_HOURS)
}

fn parse_job_timestamp(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&chrono::Utc))
}
