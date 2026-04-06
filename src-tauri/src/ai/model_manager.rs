use serde::Serialize;

#[derive(Debug, Serialize, Clone)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub format: String,     // "onnx" | "gguf"
    pub capability: String, // "ocr_receipt" | "categorize_transaction"
    pub loaded: bool,
    pub size_mb: Option<f64>,
}

#[tauri::command]
pub fn list_ai_models() -> Vec<ModelInfo> {
    // Phase 1: no models on disk yet
    vec![]
}

#[tauri::command]
pub fn get_model_status(model_id: String) -> Result<ModelInfo, String> {
    Err(format!("Model '{}' not found", model_id))
}
