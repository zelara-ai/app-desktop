use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::{timeout, Duration, Instant};

use super::{hardware_tier::HardwareTier, AiRuntimeState};

const QWEN_MODEL_ID: &str = "Qwen/Qwen3-0.6B";
pub const QWEN_HELPER_MODEL_ID: &str = "qwen3-0.6b-receipt";
/// Per-inference timeout (model already loaded).
/// CPU inference for 80 tokens on Qwen3-0.6B: ~20-120s depending on hardware.
const PYTHON_TIMEOUT_SECS: u64 = 300;
/// Cold-start timeout: BF16 Qwen3-0.6B loads in ~5–15 s on a modern CPU.
/// 120 s gives plenty of headroom even on slow drives.
const WARMUP_TIMEOUT_SECS: u64 = 120;
const DOWNLOAD_TIMEOUT_SECS: u64 = 15 * 60;
const DOWNLOAD_MARKER: &str = ".download-complete";

#[derive(Debug, Serialize)]
pub struct QwenReceiptInput<'a> {
    pub raw_text: &'a str,
    pub merchant: Option<&'a str>,
    pub date: Option<&'a str>,
    pub total: Option<f64>,
    pub confidence: f64,
    pub used_total_fallback: bool,
}

/// Holds a live Qwen3 subprocess (stdin/stdout handles + child for kill-on-failure).
/// Stored in `AiRuntimeState.persistent_qwen` behind a `tokio::sync::Mutex<Option<...>>`.
/// Spawned lazily on the first `refine_receipt_text()` call; re-spawned after any error.
pub struct PersistentQwenProcess {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: Lines<BufReader<ChildStdout>>,
}

// Safety: the Child/stdin/stdout are `!Send` due to platform handles, but we wrap the
// entire struct in `tokio::sync::Mutex` so only one task accesses it at a time.
// tokio::process types are actually Send on all supported platforms; the compiler accepts this.

#[derive(Debug, Deserialize)]
pub struct QwenReceiptSuggestion {
    pub merchant: Option<String>,
    pub description: Option<String>,
    pub date: Option<String>,
    pub total: Option<f64>,
    pub confidence: Option<f64>,
    /// Present when the Python server returns an application-level error.
    #[serde(default)]
    pub error: Option<String>,
}

/// Spawn the Qwen3 helper script in `--server` mode (persistent subprocess).
/// The model warm-up happens inside the script before it reads the first request.
async fn spawn_persistent_qwen() -> Result<PersistentQwenProcess, String> {
    let script_path = python_script_path();
    if !script_path.exists() {
        return Err(format!(
            "Qwen3 helper script not found at {}",
            script_path.display()
        ));
    }

    let mut child = Command::new("python")
        .arg(&script_path)
        .arg("--model")
        .arg(QWEN_MODEL_ID)
        .arg("--local-dir")
        .arg(helper_model_dir())
        .arg("--server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()) // captured → background task → eprintln for visibility
        .env("PYTHONUTF8", "1")
        .spawn()
        .map_err(|e| format!("Could not start Qwen3 helper: {e}"))?;

    let stdin = child
        .stdin
        .take()
        .ok_or("Could not take Qwen3 helper stdin")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Could not take Qwen3 helper stdout")?;

    // Forward Python stderr lines to our console so errors are visible.
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                eprintln!("[QwenPy] {line}");
            }
        });
    }

    println!(
        "[QwenReceipt] Persistent subprocess spawned (PID {:?})",
        child.id()
    );

    Ok(PersistentQwenProcess {
        child,
        stdin,
        stdout: BufReader::new(stdout).lines(),
    })
}

pub async fn refine_receipt_text(
    state: &AiRuntimeState,
    input: &QwenReceiptInput<'_>,
) -> Result<QwenReceiptSuggestion, String> {
    // Fast-fail checks — no 18s wait if preconditions are unmet.
    if !matches!(state.tier, HardwareTier::Medium | HardwareTier::Heavy) {
        return Err("Qwen3 fallback requires medium tier or better".to_string());
    }
    if !state.python_available {
        return Err("Python ≥ 3.9 is not available on this system — Qwen3 disabled".to_string());
    }
    if !helper_model_downloaded() {
        return Err(
            "Qwen3 helper model is not downloaded yet. Download it from AI Models first."
                .to_string(),
        );
    }

    let payload_line = {
        let mut s = serde_json::to_string(input)
            .map_err(|e| format!("Could not serialize Qwen3 payload: {e}"))?;
        s.push('\n');
        s
    };

    let started_at = Instant::now();

    // Lock the persistent subprocess slot.
    let mut guard = state.persistent_qwen.lock().await;

    // (Re-)spawn if not running, then wait for the ready signal before sending any request.
    if guard.is_none() {
        match spawn_persistent_qwen().await {
            Ok(proc) => *guard = Some(proc),
            Err(e) => return Err(format!("Could not start Qwen3 server process: {e}")),
        }
        let proc = guard.as_mut().expect("just set above");
        let warmup_start = Instant::now();
        if let Err(e) = wait_for_ready(proc).await {
            let _ = proc.child.kill().await;
            *guard = None;
            return Err(e);
        }
        println!(
            "[QwenReceipt] Warmup: {}ms",
            warmup_start.elapsed().as_millis()
        );
    }

    let proc = guard.as_mut().expect("just set above");
    let inference_start = Instant::now();

    // Write request line.
    if let Err(e) = proc.stdin.write_all(payload_line.as_bytes()).await {
        // stdin closed — process died; clear slot and report error
        *guard = None;
        return Err(format!("Qwen3 helper stdin write failed: {e}"));
    }

    // Read one response line with timeout (model is already loaded at this point).
    match timeout(
        Duration::from_secs(PYTHON_TIMEOUT_SECS),
        proc.stdout.next_line(),
    )
    .await
    {
        Err(_elapsed) => {
            // Timeout — kill and clear slot so next call re-spawns fresh.
            let _ = proc.child.kill().await;
            *guard = None;
            return Err(format!(
                "Qwen3 receipt cleanup timed out after {}s; falling back to heuristic parse",
                PYTHON_TIMEOUT_SECS
            ));
        }
        Ok(Err(io_err)) => {
            *guard = None;
            return Err(format!("Qwen3 helper stdout read failed: {io_err}"));
        }
        Ok(Ok(None)) => {
            // EOF — process exited unexpectedly.
            *guard = None;
            return Err("Qwen3 helper process exited unexpectedly".to_string());
        }
        Ok(Ok(Some(response_line))) => {
            let inference_ms = inference_start.elapsed().as_millis();
            let total_ms = started_at.elapsed().as_millis();
            let trimmed = response_line.trim().to_string();
            println!(
                "[QwenReceipt] Inference: {}ms  Total: {}ms | preview: {}",
                inference_ms,
                total_ms,
                preview_text(&trimmed, 200)
            );

            let suggestion = serde_json::from_str::<QwenReceiptSuggestion>(&trimmed)
                .map_err(|e| format!("Qwen3 helper returned invalid JSON: {e}"))?;

            if let Some(ref err) = suggestion.error {
                return Err(format!("Qwen3 helper error: {err}"));
            }

            Ok(suggestion)
        }
    }
}

/// Wait for the Python server to emit `{"status":"ready"}` after model loading.
/// Uses a long timeout (WARMUP_TIMEOUT_SECS) separate from the per-inference timeout.
async fn wait_for_ready(proc: &mut PersistentQwenProcess) -> Result<(), String> {
    match timeout(
        Duration::from_secs(WARMUP_TIMEOUT_SECS),
        proc.stdout.next_line(),
    )
    .await
    {
        Err(_) => Err(format!(
            "Qwen3 model loading timed out after {WARMUP_TIMEOUT_SECS}s — check [QwenPy] logs"
        )),
        Ok(Err(e)) => Err(format!("Qwen3 startup read error: {e}")),
        Ok(Ok(None)) => Err("Qwen3 process exited before sending ready signal".to_string()),
        Ok(Ok(Some(line))) => {
            println!("[QwenReceipt] Server ready signal: {}", line.trim());
            Ok(())
        }
    }
}

pub fn helper_model_dir() -> PathBuf {
    super::model_downloader::models_dir().join(QWEN_HELPER_MODEL_ID)
}

pub fn helper_model_downloaded() -> bool {
    let dir = helper_model_dir();
    dir.join(DOWNLOAD_MARKER).exists() && dir.join("config.json").exists()
}

pub async fn download_helper_model(app_handle: &AppHandle) -> Result<(), String> {
    let script_path = python_script_path();
    if !script_path.exists() {
        return Err(format!(
            "Qwen3 helper script not found at {}",
            script_path.display()
        ));
    }

    let model_dir = helper_model_dir();
    std::fs::create_dir_all(&model_dir)
        .map_err(|error| format!("Could not create Qwen3 model directory: {error}"))?;

    let _ = app_handle.emit(
        "model-download-progress",
        serde_json::json!({
            "model_id": QWEN_HELPER_MODEL_ID,
            "capability": "ocr_receipt",
            "progress": 0.05,
        }),
    );

    let output = timeout(
        Duration::from_secs(DOWNLOAD_TIMEOUT_SECS),
        Command::new("python")
            .arg(script_path)
            .arg("--model")
            .arg(QWEN_MODEL_ID)
            .arg("--local-dir")
            .arg(&model_dir)
            .arg("--download-only")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PYTHONUTF8", "1")
            .output(),
    )
    .await
    .map_err(|_| "Qwen3 model download timed out".to_string())?
    .map_err(|error| format!("Could not start Qwen3 model download: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let error = if stderr.is_empty() {
            format!("Qwen3 model download failed with status {}", output.status)
        } else {
            stderr
        };
        let _ = app_handle.emit(
            "model-download-error",
            serde_json::json!({
                "model_id": QWEN_HELPER_MODEL_ID,
                "error": error,
            }),
        );
        return Err(error);
    }

    if !helper_model_downloaded() {
        let error = format!(
            "Qwen3 model download finished but {} is missing expected files",
            display_path(&model_dir)
        );
        let _ = app_handle.emit(
            "model-download-error",
            serde_json::json!({
                "model_id": QWEN_HELPER_MODEL_ID,
                "error": error,
            }),
        );
        return Err(error);
    }

    let _ = app_handle.emit(
        "model-download-progress",
        serde_json::json!({
            "model_id": QWEN_HELPER_MODEL_ID,
            "capability": "ocr_receipt",
            "progress": 1.0,
        }),
    );
    let _ = app_handle.emit(
        "model-download-complete",
        serde_json::json!({
            "model_id": QWEN_HELPER_MODEL_ID,
            "capability": "ocr_receipt",
        }),
    );

    Ok(())
}

fn python_script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("scripts")
        .join("qwen_receipt_cleanup.py")
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn preview_text(value: &str, max_chars: usize) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let preview = collapsed.chars().take(max_chars).collect::<String>();
    if collapsed.chars().count() > max_chars {
        format!("{preview}...")
    } else {
        preview
    }
}
