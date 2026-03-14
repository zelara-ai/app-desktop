/// BLE Auto-Discovery — Desktop advertising module
///
/// Desktop broadcasts its WSS IP + port over BLE so nearby Mobile devices
/// can connect automatically without scanning a QR code.
///
/// Windows: uses WinRT BluetoothLEAdvertisementPublisher.
/// Other platforms: no-op (advertising returns NotSupported).
///
/// Advertisement format (manufacturer data, company ID 0xFFFE):
///   Bytes 0–3  : IPv4 address (big-endian octets)
///   Bytes 4–5  : Port number  (big-endian u16)
///
/// A fixed Zelara service UUID is also included in the advertisement so
/// Mobile can filter scans efficiently (required on iOS).
///
/// ZELARA_SERVICE_UUID = "a1b2c3d4-e5f6-7a8b-9c0d-e1f2a3b4c5d6"

use serde::{Deserialize, Serialize};
use tauri::Emitter;

/// Well-known Zelara BLE service UUID used for advertisement filtering.
/// Mobile scans for this UUID; Desktop includes it in every advertisement.
pub const ZELARA_SERVICE_UUID: &str = "a1b2c3d4-e5f6-7a8b-9c0d-e1f2a3b4c5d6";

/// Manufacturer-specific data company ID (0xFFFE = Bluetooth SIG test/reserved).
pub const ZELARA_COMPANY_ID: u16 = 0xFFFE;

// ─────────────────────────────────────────────────────────────────────────────
// Shared types (all platforms)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum BleStatus {
    /// Platform does not support BLE advertising (non-Windows, or no adapter).
    NotSupported,
    /// BLE adapter present but not currently advertising.
    Idle,
    /// Currently broadcasting advertisement.
    Advertising { ip: String, port: u16 },
    /// Advertisement failed to start.
    Error { message: String },
}

// ─────────────────────────────────────────────────────────────────────────────
// Windows implementation
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod platform {
    use super::{BleStatus, ZELARA_COMPANY_ID};
    use std::sync::Mutex;
    use windows::{
        core::GUID,
        Devices::Bluetooth::Advertisement::{
            BluetoothLEAdvertisementPublisher, BluetoothLEManufacturerData,
        },
        Storage::Streams::DataWriter,
    };

    // a1b2c3d4-e5f6-7a8b-9c0d-e1f2a3b4c5d6
    const ZELARA_GUID: GUID = GUID::from_values(
        0xa1b2c3d4,
        0xe5f6,
        0x7a8b,
        [0x9c, 0x0d, 0xe1, 0xf2, 0xa3, 0xb4, 0xc5, 0xd6],
    );

    /// Opaque wrapper that keeps the WinRT publisher alive until dropped.
    /// We implement Send + Sync manually because WinRT objects use COM agile
    /// marshalling and are safe to use across threads.
    pub struct PublisherHandle(BluetoothLEAdvertisementPublisher);
    // SAFETY: BluetoothLEAdvertisementPublisher is agile (thread-safe) per WinRT contract.
    unsafe impl Send for PublisherHandle {}
    unsafe impl Sync for PublisherHandle {}

    impl Drop for PublisherHandle {
        fn drop(&mut self) {
            let _ = self.0.Stop();
        }
    }

    pub struct BleState {
        handle: Mutex<Option<PublisherHandle>>,
        current_status: Mutex<BleStatus>,
    }

    impl BleState {
        pub fn new() -> Self {
            Self {
                handle: Mutex::new(None),
                current_status: Mutex::new(BleStatus::Idle),
            }
        }

        pub fn start(&self, ip: &str, port: u16) -> Result<(), String> {
            // Stop any existing advertisement first
            self.stop();

            // Create publisher and get its advertisement object to configure
            let publisher = BluetoothLEAdvertisementPublisher::new()
                .map_err(|e| format!("Failed to create BLE publisher: {e}"))?;

            let advertisement = publisher
                .Advertisement()
                .map_err(|e| format!("Failed to access advertisement from publisher: {e}"))?;

            // ── Service UUID — enables filtered scanning on iOS and Android ──
            advertisement
                .ServiceUuids()
                .map_err(|e| format!("Cannot access ServiceUuids: {e}"))?
                .Append(ZELARA_GUID)
                .map_err(|e| format!("Failed to append Zelara service UUID: {e}"))?;

            // ── Manufacturer data — encodes IP:port for the mobile scanner ──
            // Parse IPv4 address into 4 bytes
            let octets: Vec<u8> = ip
                .split('.')
                .filter_map(|s| s.parse::<u8>().ok())
                .collect();
            if octets.len() != 4 {
                return Err(format!("Invalid IPv4 address for BLE advertisement: {ip}"));
            }

            // Write [ip0, ip1, ip2, ip3, port_hi, port_lo] into a DataWriter buffer
            let writer =
                DataWriter::new().map_err(|e| format!("Failed to create DataWriter: {e}"))?;
            for byte in &octets {
                writer
                    .WriteByte(*byte)
                    .map_err(|e| format!("Failed to write IP byte: {e}"))?;
            }
            writer
                .WriteByte((port >> 8) as u8)
                .map_err(|e| format!("Failed to write port high byte: {e}"))?;
            writer
                .WriteByte((port & 0xFF) as u8)
                .map_err(|e| format!("Failed to write port low byte: {e}"))?;
            let buffer = writer
                .DetachBuffer()
                .map_err(|e| format!("Failed to detach DataWriter buffer: {e}"))?;

            let mfr = BluetoothLEManufacturerData::new()
                .map_err(|e| format!("Failed to create manufacturer data: {e}"))?;
            mfr.SetCompanyId(ZELARA_COMPANY_ID)
                .map_err(|e| format!("Failed to set company ID: {e}"))?;
            mfr.SetData(buffer)
                .map_err(|e| format!("Failed to set manufacturer data payload: {e}"))?;

            advertisement
                .ManufacturerData()
                .map_err(|e| format!("Cannot access ManufacturerData: {e}"))?
                .Append(mfr)
                .map_err(|e| format!("Failed to append manufacturer data: {e}"))?;

            // Start broadcasting
            publisher
                .Start()
                .map_err(|e| format!("Failed to start BLE advertising: {e}"))?;

            println!("[BLE] Advertising {}:{}", ip, port);

            *self.handle.lock().unwrap() = Some(PublisherHandle(publisher));
            *self.current_status.lock().unwrap() = BleStatus::Advertising {
                ip: ip.to_string(),
                port,
            };
            Ok(())
        }

        pub fn stop(&self) {
            // Dropping PublisherHandle calls Stop() via Drop impl
            *self.handle.lock().unwrap() = None;
            *self.current_status.lock().unwrap() = BleStatus::Idle;
        }

        pub fn status(&self) -> BleStatus {
            self.current_status.lock().unwrap().clone()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Non-Windows stub (macOS, Linux — no BLE advertising yet)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::BleStatus;

    pub struct BleState;

    impl BleState {
        pub fn new() -> Self {
            Self
        }

        pub fn start(&self, _ip: &str, _port: u16) -> Result<(), String> {
            Err("BLE advertising is not yet supported on this platform".to_string())
        }

        pub fn stop(&self) {}

        pub fn status(&self) -> BleStatus {
            BleStatus::NotSupported
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Public-facing state type (wraps platform impl)
// ─────────────────────────────────────────────────────────────────────────────

pub struct BleAdvertisingState(platform::BleState);

impl BleAdvertisingState {
    pub fn new() -> Self {
        Self(platform::BleState::new())
    }

    pub fn start(&self, ip: &str, port: u16) -> Result<(), String> {
        self.0.start(ip, port)
    }

    pub fn stop(&self) {
        self.0.stop();
    }

    pub fn status(&self) -> BleStatus {
        self.0.status()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tauri commands
// ─────────────────────────────────────────────────────────────────────────────

/// Start BLE advertising using the machine's primary non-loopback IPv4 address.
/// No-op if already advertising. Returns an error string on failure (non-fatal —
/// the app continues to work; QR pairing is always available as a fallback).
#[tauri::command]
pub fn start_ble_advertising(
    app: tauri::AppHandle,
    state: tauri::State<BleAdvertisingState>,
) -> Result<BleStatus, String> {
    // Already advertising — no-op
    if let BleStatus::Advertising { .. } = state.status() {
        return Ok(state.status());
    }

    let ip = crate::device_linking::get_primary_ip_pub()?;
    state.start(&ip, 8765)?;
    let status = state.status();
    let _ = app.emit("ble-status-changed", &status);
    Ok(status)
}

/// Stop BLE advertising. Safe to call even if not currently advertising.
#[tauri::command]
pub fn stop_ble_advertising(app: tauri::AppHandle, state: tauri::State<BleAdvertisingState>) {
    state.stop();
    let _ = app.emit("ble-status-changed", state.status());
}

/// Return the current BLE advertising status.
#[tauri::command]
pub fn get_ble_status(state: tauri::State<BleAdvertisingState>) -> BleStatus {
    state.status()
}
