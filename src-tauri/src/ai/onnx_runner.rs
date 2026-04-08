use ort::{ep, session::Session, value::DynValue};
use std::path::Path;
use std::sync::Mutex;

/// Wrapper around an ONNX Runtime session.
/// One `OnnxRunner` per model; sessions are cached in `AiRuntimeState`.
///
/// The session is public so capability handlers (ocr_receipt, categorization)
/// can build tensors and run inference directly with the ort API.
pub struct OnnxRunner {
    pub session: Mutex<Session>,
    pub model_id: String,
}

impl OnnxRunner {
    /// Load an ONNX model from disk and build an ORT session.
    pub fn load(model_path: &Path, model_id: &str) -> Result<Self, String> {
        let builder = Session::builder().map_err(|e| format!("ORT session builder failed: {e}"))?;
        #[cfg(target_os = "windows")]
        let mut builder = builder
            .with_execution_providers([ep::DirectML::default().build().fail_silently()])
            .map_err(|e| format!("ORT DirectML configuration failed: {e}"))?;
        #[cfg(not(target_os = "windows"))]
        let mut builder = builder;

        let session = builder
            .commit_from_file(model_path)
            .map_err(|e| format!("Failed to load model '{}': {e}", model_path.display()))?;

        println!("[OnnxRunner] Loaded {} from {:?}", model_id, model_path);
        Ok(Self {
            session: Mutex::new(session),
            model_id: model_id.to_string(),
        })
    }

    /// Returns the model input names declared by the ONNX graph.
    pub fn input_names(&self) -> Result<Vec<String>, String> {
        let session = self
            .session
            .lock()
            .map_err(|_| format!("Session lock poisoned for {}", self.model_id))?;
        Ok(session
            .inputs()
            .iter()
            .map(|input| input.name().to_string())
            .collect())
    }

    /// Runs inference and returns owned output values keyed by output name.
    pub fn run(&self, inputs: Vec<(String, DynValue)>) -> Result<Vec<(String, DynValue)>, String> {
        let mut session = self
            .session
            .lock()
            .map_err(|_| format!("Session lock poisoned for {}", self.model_id))?;
        let outputs = session
            .run(inputs)
            .map_err(|e| format!("Inference failed for {}: {e}", self.model_id))?;
        Ok(outputs
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect())
    }
}
