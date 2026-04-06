pub mod categorization;
pub mod dispatcher;
pub mod model_manager;
pub mod ocr_receipt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct AiTaskRequest {
    pub task_id: String,
    pub capability: String,
    #[allow(dead_code)] // used by capability handlers in Phase 5
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct AiTaskResponse {
    pub task_id: String,
    pub capability: String,
    pub success: bool,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
}

#[derive(Debug)]
#[allow(dead_code)] // variants used in Phase 5 when model backends are implemented
pub enum AiTaskError {
    NotYetImplemented,
    ModelNotLoaded(String),
    ProcessingFailed(String),
}

impl AiTaskError {
    pub fn to_message(&self) -> String {
        match self {
            AiTaskError::NotYetImplemented => "Capability not yet implemented".to_string(),
            AiTaskError::ModelNotLoaded(name) => format!("Model not loaded: {}", name),
            AiTaskError::ProcessingFailed(msg) => format!("Processing failed: {}", msg),
        }
    }
}
