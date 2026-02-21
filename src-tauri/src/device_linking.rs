use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::State;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub platform: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PairingInfo {
    pub qr_data: String,
    pub ip_address: String,
    pub port: u16,
    pub token: String,
}

pub struct DeviceLinkingState {
    pub linked_devices: Mutex<Vec<DeviceInfo>>,
    pub pairing_token: Mutex<Option<String>>,
}

impl DeviceLinkingState {
    pub fn new() -> Self {
        Self {
            linked_devices: Mutex::new(Vec::new()),
            pairing_token: Mutex::new(None),
        }
    }
}

#[tauri::command]
pub fn generate_qr_code(state: State<DeviceLinkingState>) -> Result<PairingInfo, String> {
    // Generate pairing token
    let token = format!("token_{}", uuid::Uuid::new_v4());

    // Get local IP (simplified - in production use proper network discovery)
    let ip_address = "127.0.0.1".to_string(); // TODO: Get actual local IP
    let port = 8765;

    // Create QR data
    let qr_data = format!("zelara://pair?ip={}&port={}&token={}", ip_address, port, token);

    // Store token
    *state.pairing_token.lock().unwrap() = Some(token.clone());

    Ok(PairingInfo {
        qr_data,
        ip_address,
        port,
        token,
    })
}

#[tauri::command]
pub fn get_linked_devices(state: State<DeviceLinkingState>) -> Result<Vec<DeviceInfo>, String> {
    Ok(state.linked_devices.lock().unwrap().clone())
}

#[tauri::command]
pub fn add_linked_device(
    state: State<DeviceLinkingState>,
    device: DeviceInfo,
) -> Result<(), String> {
    state.linked_devices.lock().unwrap().push(device);
    Ok(())
}
