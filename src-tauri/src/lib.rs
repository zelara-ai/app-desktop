// Zelara Desktop App - Tauri Backend

mod device_linking;
mod cv_processor;
mod storage;

use device_linking::DeviceLinkingState;
use cv_processor::CVProcessorState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls ring crypto provider");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(DeviceLinkingState::new())
        .manage(CVProcessorState::new())
        .invoke_handler(tauri::generate_handler![
            // Device linking commands
            device_linking::generate_qr_code,
            device_linking::get_linked_devices,
            device_linking::add_linked_device,
            device_linking::start_pairing_server,
            device_linking::stop_pairing_server,
            device_linking::get_local_ips,
            // CV processing commands
            cv_processor::validate_recycling_image,
            // Storage commands
            storage::load_progress,
            storage::save_progress,
            storage::award_points,
            storage::unlock_module,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
