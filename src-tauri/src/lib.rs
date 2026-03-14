// Zelara Desktop App - Tauri Backend

mod device_linking;
mod cv_processor;
mod storage;
mod ble_advertising;

use device_linking::DeviceLinkingState;
use cv_processor::CVProcessorState;
use ble_advertising::BleAdvertisingState;
use tauri::Manager;

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
        .setup(|app| {
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
            // BLE auto-discovery commands
            ble_advertising::start_ble_advertising,
            ble_advertising::stop_ble_advertising,
            ble_advertising::get_ble_status,
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
