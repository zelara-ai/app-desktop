use super::{AiTaskError, AiTaskRequest, AiTaskResponse};

pub fn dispatch(request: AiTaskRequest) -> AiTaskResponse {
    let result: Result<serde_json::Value, AiTaskError> = match request.capability.as_str() {
        "ocr_receipt" => super::ocr_receipt::handle(&request),
        "categorize_transaction" => super::categorization::handle(&request),
        _ => Err(AiTaskError::NotYetImplemented),
    };

    match result {
        Ok(value) => AiTaskResponse {
            task_id: request.task_id,
            capability: request.capability,
            success: true,
            result: Some(value),
            error: None,
        },
        Err(e) => AiTaskResponse {
            task_id: request.task_id,
            capability: request.capability,
            success: false,
            result: None,
            error: Some(e.to_message()),
        },
    }
}
