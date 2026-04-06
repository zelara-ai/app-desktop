use super::{AiTaskError, AiTaskRequest};

pub fn handle(_request: &AiTaskRequest) -> Result<serde_json::Value, AiTaskError> {
    Err(AiTaskError::NotYetImplemented)
}
