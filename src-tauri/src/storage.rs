use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UserProgress {
    pub points: i32,
    pub unlocked_modules: Vec<String>,
    pub available_unlocks: Vec<String>,
    pub last_updated: String,
}

impl Default for UserProgress {
    fn default() -> Self {
        Self {
            points: 0,
            unlocked_modules: vec!["green".to_string()], // Green module always unlocked (starter)
            available_unlocks: vec![],
            last_updated: chrono::Utc::now().to_rfc3339(),
        }
    }
}

pub fn get_data_dir() -> Result<PathBuf, String> {
    let home_dir = dirs::home_dir().ok_or("Could not find home directory")?;
    let data_dir = home_dir.join(".zelara");

    if !data_dir.exists() {
        fs::create_dir_all(&data_dir).map_err(|e| format!("Failed to create data directory: {}", e))?;
    }

    Ok(data_dir)
}

#[tauri::command]
pub fn load_progress() -> Result<UserProgress, String> {
    let progress_file = get_data_dir()?.join("progress.json");

    if !progress_file.exists() {
        // Return default progress for new user
        return Ok(UserProgress::default());
    }

    let content = fs::read_to_string(&progress_file)
        .map_err(|e| format!("Failed to read progress file: {}", e))?;

    serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse progress: {}", e))
}

#[tauri::command]
pub fn save_progress(progress: UserProgress) -> Result<(), String> {
    let progress_file = get_data_dir()?.join("progress.json");

    let content = serde_json::to_string_pretty(&progress)
        .map_err(|e| format!("Failed to serialize progress: {}", e))?;

    fs::write(&progress_file, content)
        .map_err(|e| format!("Failed to write progress file: {}", e))
}

#[tauri::command]
pub fn award_points(points_to_add: i32) -> Result<UserProgress, String> {
    let mut progress = load_progress()?;
    progress.points += points_to_add;
    progress.last_updated = chrono::Utc::now().to_rfc3339();

    // Check for unlocks
    if progress.points >= 50 && !progress.unlocked_modules.contains(&"finance".to_string()) {
        progress.available_unlocks.push("finance".to_string());
    }

    save_progress(progress.clone())?;
    Ok(progress)
}

#[tauri::command]
pub fn unlock_module(module_name: String) -> Result<UserProgress, String> {
    let mut progress = load_progress()?;

    // Move from available_unlocks to unlocked_modules
    progress.available_unlocks.retain(|m| m != &module_name);

    if !progress.unlocked_modules.contains(&module_name) {
        progress.unlocked_modules.push(module_name);
    }

    progress.last_updated = chrono::Utc::now().to_rfc3339();
    save_progress(progress.clone())?;
    Ok(progress)
}
