use super::{
    model_manager::{ensure_capability_ready, CapabilityStatus},
    model_manifest::get_models_for_capability,
    AiActivityEntry, AiRuntimeState, AiTaskError, AiTaskRequest, AiTaskResponse,
};
use tauri::{AppHandle, Emitter};
use tokio::time::Duration;

/// Maximum time any capability handler may run before the dispatcher returns a structured
/// `task_timeout` error. Set below the mobile client's 90 s deadline so the client always
/// receives a response before it gives up waiting.
const TASK_TIMEOUT_SECS: u64 = 45;

/// Dispatch an `ai_task` request.
///
/// Flow:
/// 1. Add an immediate activity entry so Desktop can show the task instantly.
/// 2. Check hardware tier meets the capability's `tier_min`.
/// 3. Call `ensure_capability_ready` — downloads models if missing, loads sessions.
/// 4. If downloading: return `model_downloading` response immediately.
/// 5. If ready: run the capability handler and stream status updates to Recent AI Activity.
pub async fn dispatch(
    request: AiTaskRequest,
    state: &AiRuntimeState,
    app_handle: &AppHandle,
) -> AiTaskResponse {
    push_activity(
        state,
        app_handle,
        AiActivityEntry {
            task_id: request.task_id.clone(),
            capability: request.capability.clone(),
            summary: format!("{} queued", humanize_capability(&request.capability)),
            timestamp: chrono::Utc::now().to_rfc3339(),
            status: "queued".to_string(),
            error: None,
            metadata: serde_json::json!({}),
        },
    );

    let models = get_models_for_capability(&request.capability);
    if !models.is_empty() {
        let tier_min = &models[0].tier_min;
        if !state.tier.meets(tier_min) {
            let error = AiTaskError::TierInsufficient {
                required: tier_min.clone(),
                current: state.tier.as_str().to_string(),
            };
            return fail_with_activity(&request, state, app_handle, error, serde_json::json!({}));
        }
    }

    match ensure_capability_ready(&request.capability, state, app_handle).await {
        CapabilityStatus::Downloading(progress) => {
            push_activity(
                state,
                app_handle,
                AiActivityEntry {
                    task_id: request.task_id.clone(),
                    capability: request.capability.clone(),
                    summary: format!(
                        "{} waiting for model download ({}%)",
                        humanize_capability(&request.capability),
                        (progress * 100.0).round() as i32
                    ),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    status: "waiting_for_model".to_string(),
                    error: None,
                    metadata: serde_json::json!({ "download_progress": progress }),
                },
            );

            return AiTaskResponse {
                task_id: request.task_id,
                capability: request.capability,
                success: false,
                result: None,
                error: Some("model_downloading".to_string()),
                download_progress: Some(progress),
            };
        }
        CapabilityStatus::Failed(message) => {
            return fail_with_activity(
                &request,
                state,
                app_handle,
                AiTaskError::ProcessingFailed(message),
                serde_json::json!({}),
            );
        }
        CapabilityStatus::Ready => {}
    }

    push_activity(
        state,
        app_handle,
        AiActivityEntry {
            task_id: request.task_id.clone(),
            capability: request.capability.clone(),
            summary: format!("{} running", humanize_capability(&request.capability)),
            timestamp: chrono::Utc::now().to_rfc3339(),
            status: "running".to_string(),
            error: None,
            metadata: serde_json::json!({}),
        },
    );

    let capability = request.capability.clone();
    let result: Result<serde_json::Value, AiTaskError> =
        match tokio::time::timeout(Duration::from_secs(TASK_TIMEOUT_SECS), async {
            match capability.as_str() {
                "ocr_receipt" => super::ocr_receipt::handle(&request, state).await,
                "categorize_transaction" => super::categorization::handle(&request, state),
                "import_file" => crate::finance_import::handle_ai_task(&request),
                _ => Err(AiTaskError::NotYetImplemented),
            }
        })
        .await
        {
            Ok(inner) => inner,
            Err(_elapsed) => Err(AiTaskError::Timeout {
                capability: request.capability.clone(),
                elapsed_secs: TASK_TIMEOUT_SECS,
            }),
        };

    match result {
        Ok(value) => {
            let summary = build_summary(&request.capability, &value);
            let status = completion_status(&request.capability, &value);
            push_activity(
                state,
                app_handle,
                AiActivityEntry {
                    task_id: request.task_id.clone(),
                    capability: request.capability.clone(),
                    summary,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    status: status.to_string(),
                    error: None,
                    metadata: value.clone(),
                },
            );

            AiTaskResponse {
                task_id: request.task_id,
                capability: request.capability,
                success: true,
                result: Some(value),
                error: None,
                download_progress: None,
            }
        }
        Err(error) => fail_with_activity(&request, state, app_handle, error, serde_json::json!({})),
    }
}

fn fail_with_activity(
    request: &AiTaskRequest,
    state: &AiRuntimeState,
    app_handle: &AppHandle,
    err: AiTaskError,
    metadata: serde_json::Value,
) -> AiTaskResponse {
    let error_code = err.error_code().to_string();
    let error_msg = err.to_message();
    push_activity(
        state,
        app_handle,
        AiActivityEntry {
            task_id: request.task_id.clone(),
            capability: request.capability.clone(),
            summary: format!("{} failed", humanize_capability(&request.capability)),
            timestamp: chrono::Utc::now().to_rfc3339(),
            status: "failed".to_string(),
            error: Some(error_code.clone()),
            metadata,
        },
    );

    // Structured log — makes dispatcher-level failures visible in the Finance UI log panel.
    {
        let mut entry = crate::receipt_queue::log_entry(
            &request.task_id,
            crate::receipt_queue::OcrStage::Dispatch,
            crate::receipt_queue::LogLevel::Error,
            format!(
                "{} failed: {error_msg}",
                humanize_capability(&request.capability)
            ),
        );
        entry.error_detail = Some(error_msg);
        state.receipt_logger.push(entry);
    }

    error_response(request, err)
}

fn error_response(request: &AiTaskRequest, err: AiTaskError) -> AiTaskResponse {
    let download_progress = match &err {
        AiTaskError::ModelDownloading { progress, .. } => Some(*progress),
        AiTaskError::Timeout { .. } => None,
        _ => None,
    };
    AiTaskResponse {
        task_id: request.task_id.clone(),
        capability: request.capability.clone(),
        success: false,
        result: None,
        error: Some(err.error_code().to_string()),
        download_progress,
    }
}

fn build_summary(capability: &str, result: &serde_json::Value) -> String {
    match capability {
        "ocr_receipt" => {
            let merchant = result["merchant"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("Receipt needs review");
            let total = result["total"].as_f64().unwrap_or(0.0);
            let review_fields = result["reviewFields"]
                .as_array()
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if result["status"].as_str() == Some("needs_review") && !review_fields.is_empty() {
                return format!(
                    "{merchant} • Needs review: {}",
                    review_fields.join(", ")
                );
            }
            if total > 0.0 {
                format!("{merchant} • Total €{total:.2}")
            } else if result["status"].as_str() == Some("needs_review") {
                "Receipt needs review".to_string()
            } else {
                format!("{merchant} • Total missing")
            }
        }
        "categorize_transaction" => {
            let desc = result["description"].as_str().unwrap_or("Transaction");
            let cat = result["categoryId"].as_str().unwrap_or("uncategorized");
            format!("{desc} -> {cat}")
        }
        "import_file" => {
            let count = result.as_array().map(|a| a.len()).unwrap_or(0);
            format!("Imported {count} transactions")
        }
        _ => capability.to_string(),
    }
}

fn humanize_capability(capability: &str) -> &'static str {
    match capability {
        "ocr_receipt" => "Receipt OCR",
        "categorize_transaction" => "Transaction categorization",
        "import_file" => "File import",
        _ => "AI task",
    }
}

fn push_activity(state: &AiRuntimeState, app_handle: &AppHandle, entry: AiActivityEntry) {
    state.upsert_activity(entry.clone());
    let _ = app_handle.emit("ai-activity-updated", &entry);
}

fn completion_status(capability: &str, result: &serde_json::Value) -> &'static str {
    match capability {
        "ocr_receipt" => {
            if let Some(status) = result["status"].as_str() {
                return match status {
                    "completed" => "completed",
                    "needs_review" => "needs_review",
                    "failed" => "failed",
                    _ => "needs_review",
                };
            }
            if result["reviewReason"].as_str().is_some() {
                return "needs_review";
            }
            let total = result["total"].as_f64().unwrap_or(0.0);
            let confidence = result["confidence"].as_f64().unwrap_or(0.0);
            let merchant = result["merchant"].as_str().unwrap_or("");
            if total <= 0.0 || confidence < 0.8 || merchant.trim().is_empty() {
                "needs_review"
            } else {
                "completed"
            }
        }
        _ => "completed",
    }
}
