// Zelara Core - Tauri Backend

mod ai;
mod ble_advertising;
mod cv_processor;
mod device_linking;
mod finance_import;
mod receipt_queue;
mod storage;

use ai::{AiRuntimeState, AiTaskRequest};
use ble_advertising::BleAdvertisingState;
use cv_processor::CVProcessorState;
use device_linking::DeviceLinkingState;
use receipt_queue::ReceiptQueueState;
use std::sync::Arc;
use tauri::Manager;

/// Run the full OCR pipeline on an arbitrary local image file for smoke-testing.
/// Returns the same JSON the mobile client would receive (fieldConfidence, draft, etc.).
#[tauri::command]
async fn test_ocr_pipeline(
    image_path: String,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, Arc<AiRuntimeState>>,
) -> Result<serde_json::Value, String> {
    use ai::model_manager::{ensure_capability_ready, CapabilityStatus};
    match ensure_capability_ready("ocr_receipt", &*state, &app_handle).await {
        CapabilityStatus::Downloading(p) => {
            return Ok(serde_json::json!({
                "status": "model_downloading",
                "progress": p
            }));
        }
        CapabilityStatus::Failed(msg) => {
            return Err(format!("Model not ready: {msg}"));
        }
        CapabilityStatus::Ready => {}
    }
    let req = AiTaskRequest {
        task_id: "test-ocr".to_string(),
        capability: "ocr_receipt".to_string(),
        payload: serde_json::json!({ "imagePath": image_path }),
    };
    ai::ocr_receipt::handle(&req, &*state)
        .await
        .map_err(|e| format!("{e:?}"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls ring crypto provider");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(DeviceLinkingState::new())
        .manage(CVProcessorState::new())
        .manage(BleAdvertisingState::new())
        .manage(Arc::new(AiRuntimeState::new()))
        .manage(Arc::new(ReceiptQueueState::new()))
        .setup(|app| {
            // Log detected hardware tier at startup
            let ai_state = app.state::<Arc<AiRuntimeState>>();
            let ai_runtime = Arc::clone(&*ai_state);
            println!("[AiRuntime] Hardware tier: {}", ai_state.tier.as_str());

            // Keep OCR logs beside the persisted receipt queue so support/debugging has one source of truth.
            if let Ok(data_dir) = crate::storage::get_data_dir() {
                ai_state.receipt_logger.set_log_dir(data_dir);
            } else if let Ok(data_dir) = app.path().app_data_dir() {
                ai_state.receipt_logger.set_log_dir(data_dir);
            }
            drop(ai_state);

            // Start BLE advertising at launch so Mobile can auto-discover without
            // the user having to open the QR pairing screen first.
            // Failure is non-fatal — QR pairing always works as a fallback.
            let ble = app.state::<BleAdvertisingState>();
            match device_linking::get_primary_ip_pub() {
                Ok(ip) => match ble.start(&ip, 8765) {
                    Ok(_) => println!("[BLE] Auto-discovery advertising started on {}:8765", ip),
                    Err(e) => eprintln!("[BLE] Auto-discovery not available: {}", e),
                },
                Err(e) => eprintln!("[BLE] Cannot resolve local IP for BLE advertising: {}", e),
            }

            let receipt_state = app.state::<Arc<ReceiptQueueState>>();
            if receipt_state.has_pending_jobs() {
                let device_state = app.state::<DeviceLinkingState>();
                device_linking::resume_receipt_processing_loop(
                    Arc::clone(&*receipt_state),
                    ai_runtime,
                    device_state.client_senders.clone(),
                    app.handle().clone(),
                );
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Device linking commands
            device_linking::generate_qr_code,
            device_linking::get_linked_devices,
            device_linking::add_linked_device,
            device_linking::start_pairing_server,
            device_linking::stop_pairing_server,
            device_linking::get_local_ips,
            device_linking::get_latest_image_processing_state,
            device_linking::get_finance_context,
            device_linking::request_finance_context_sync,
            // BLE auto-discovery commands
            ble_advertising::start_ble_advertising,
            ble_advertising::stop_ble_advertising,
            ble_advertising::get_ble_status,
            // CV processing commands
            cv_processor::validate_recycling_image,
            // Storage commands (load/save are direct; award/unlock go via device_linking wrappers for sync)
            storage::load_progress,
            storage::save_progress,
            device_linking::award_points,
            device_linking::reset_points,
            device_linking::unlock_module,
            // AI model management commands
            ai::model_manager::list_ai_models,
            ai::model_manager::get_model_status,
            ai::model_manager::download_model_cmd,
            ai::hardware_tier::get_hardware_tier,
            ai::get_ai_activity,
            ai::get_receipt_log,
            receipt_queue::get_receipt_jobs,
            // Finance file import
            finance_import::import_finance_file,
            finance_import::push_finance_transactions,
            device_linking::send_receipt_job_to_mobile,
            test_ocr_pipeline,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                // Signal the receipt processing loop to stop cleanly.
                app_handle
                    .state::<Arc<receipt_queue::ReceiptQueueState>>()
                    .shutdown();
            }
        });
}
