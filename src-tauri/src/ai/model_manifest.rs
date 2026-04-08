use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ManifestEntry {
    pub id: String,
    /// Human-readable name shown in AI Models tab
    pub name: String,
    /// Capability this model serves: "ocr_receipt" | "categorize_transaction" | …
    pub capability: String,
    /// Role within a capability: "detector" | "recognizer" | "embedder" | "classifier"
    pub role: String,
    /// Always "onnx" for Phase 5
    pub format: String,
    pub download_url: String,
    pub sha256: String,
    pub size_mb: f64,
    /// Minimum tier required: "light" | "medium" | "heavy"
    pub tier_min: String,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    #[allow(dead_code)]
    version: u32,
    models: Vec<ManifestEntry>,
}

const MANIFEST_JSON: &str = include_str!("../../assets/model_manifest.json");

/// Returns all entries from the compile-time bundled manifest.
pub fn load_manifest() -> Vec<ManifestEntry> {
    match serde_json::from_str::<Manifest>(MANIFEST_JSON) {
        Ok(m) => m.models,
        Err(e) => {
            eprintln!("[ModelManifest] Failed to parse bundled manifest: {e}");
            vec![]
        }
    }
}

/// Returns all manifest entries required to serve the given capability.
pub fn get_models_for_capability(capability: &str) -> Vec<ManifestEntry> {
    load_manifest()
        .into_iter()
        .filter(|e| e.capability == capability)
        .collect()
}
