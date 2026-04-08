use serde::Serialize;
use sysinfo::System;
use tauri::State;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HardwareTier {
    /// Battery-powered AND <16 GB RAM — rule-based only; small ONNX models still run
    Light,
    /// AC power OR ≥16 GB RAM — full ONNX suite, DirectML/NPU acceleration available
    Medium,
    /// Dedicated GPU detected — reserved for v0.7.0+ large models
    Heavy,
}

impl HardwareTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            HardwareTier::Light => "light",
            HardwareTier::Medium => "medium",
            HardwareTier::Heavy => "heavy",
        }
    }

    /// Returns true if this tier meets or exceeds the required tier.
    pub fn meets(&self, required: &str) -> bool {
        let self_rank = self.rank();
        let req_rank = match required {
            "light" => 0,
            "medium" => 1,
            "heavy" => 2,
            _ => 0,
        };
        self_rank >= req_rank
    }

    fn rank(&self) -> u8 {
        match self {
            HardwareTier::Light => 0,
            HardwareTier::Medium => 1,
            HardwareTier::Heavy => 2,
        }
    }
}

/// Returns the detected hardware tier as a string ("light" | "medium" | "heavy").
#[tauri::command]
pub fn get_hardware_tier(state: State<'_, std::sync::Arc<super::AiRuntimeState>>) -> String {
    state.tier.as_str().to_string()
}

/// Detect hardware tier at startup. Result is cached in `AiRuntimeState`.
///
/// Logic:
///   - Heavy: dedicated GPU (DirectML adapter with dedicated VRAM) — deferred to v0.7.0
///   - Medium: AC power OR total RAM >= 16 GB
///   - Light:  battery-powered AND total RAM < 16 GB
pub fn detect_tier() -> HardwareTier {
    let mut sys = System::new();
    sys.refresh_memory();

    let total_ram_bytes = sys.total_memory();
    let total_ram_gb = total_ram_bytes as f64 / 1024.0 / 1024.0 / 1024.0;

    let on_ac = is_on_ac_power();

    if total_ram_gb >= 16.0 || on_ac {
        HardwareTier::Medium
    } else {
        HardwareTier::Light
    }
}

/// Returns true if the system is connected to AC power (not running on battery).
/// Falls back to `true` on non-Windows or when the API is unavailable.
fn is_on_ac_power() -> bool {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        let mut status = SYSTEM_POWER_STATUS::default();
        unsafe {
            if GetSystemPowerStatus(&mut status).is_ok() {
                // ACLineStatus: 0 = offline (battery), 1 = online (AC), 255 = unknown
                return status.ACLineStatus != 0;
            }
        }
        true // default: assume AC on API failure
    }
    #[cfg(not(target_os = "windows"))]
    {
        true // non-Windows: assume AC (desktop likely)
    }
}
