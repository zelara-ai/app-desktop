use std::sync::{Mutex, OnceLock};

use ort::value::Tensor;
use serde::Deserialize;
use tokenizers::Tokenizer;

use super::{model_manager, AiRuntimeState, AiTaskError, AiTaskRequest};

const TOKENIZER_JSON: &str = include_str!("../../assets/minilm_tokenizer.json");
const CATEGORY_CENTROIDS_JSON: &str = include_str!("../../assets/category_centroids.json");
const MODEL_ID: &str = "minilm-l6-v2";
const UNCATEGORIZED_THRESHOLD: f32 = 0.55;

#[derive(Debug, Deserialize)]
struct CategorySeedManifest {
    categories: Vec<CategorySeedEntry>,
}

#[derive(Debug, Deserialize)]
struct CategorySeedEntry {
    #[serde(rename = "categoryId")]
    category_id: String,
    phrases: Vec<String>,
}

#[derive(Debug, Clone)]
struct CategoryCentroid {
    category_id: String,
    vector: Vec<f32>,
}

static TOKENIZER: OnceLock<Tokenizer> = OnceLock::new();
static CENTROIDS: OnceLock<Mutex<Option<Vec<CategoryCentroid>>>> = OnceLock::new();

pub fn handle(
    request: &AiTaskRequest,
    state: &AiRuntimeState,
) -> Result<serde_json::Value, AiTaskError> {
    let description = request
        .payload
        .get("description")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AiTaskError::ProcessingFailed("Missing description payload".to_string()))?;

    let embedding = embed_text(state, description)?;
    let centroids = get_centroids(state)?;
    let (category_id, confidence) = classify_embedding(&embedding, &centroids);

    let final_category = if confidence < UNCATEGORIZED_THRESHOLD {
        "uncategorized".to_string()
    } else {
        category_id
    };

    Ok(serde_json::json!({
        "categoryId": final_category,
        "confidence": confidence,
        "source": "model",
        "description": description,
    }))
}

fn tokenizer() -> Result<&'static Tokenizer, AiTaskError> {
    if let Some(tokenizer) = TOKENIZER.get() {
        return Ok(tokenizer);
    }

    let tokenizer = Tokenizer::from_bytes(TOKENIZER_JSON.as_bytes()).map_err(|error| {
        AiTaskError::ProcessingFailed(format!("Tokenizer load failed: {error}"))
    })?;
    let _ = TOKENIZER.set(tokenizer);
    TOKENIZER
        .get()
        .ok_or_else(|| AiTaskError::ProcessingFailed("Tokenizer cache unavailable".to_string()))
}

fn get_centroids(state: &AiRuntimeState) -> Result<Vec<CategoryCentroid>, AiTaskError> {
    let cache = CENTROIDS.get_or_init(|| Mutex::new(None));

    {
        let guard = cache.lock().map_err(|_| {
            AiTaskError::ProcessingFailed("Centroid cache lock poisoned".to_string())
        })?;
        if let Some(entries) = &*guard {
            return Ok(entries.clone());
        }
    }

    let manifest: CategorySeedManifest =
        serde_json::from_str(CATEGORY_CENTROIDS_JSON).map_err(|error| {
            AiTaskError::ProcessingFailed(format!("Category centroid asset is invalid: {error}"))
        })?;

    let mut built = Vec::with_capacity(manifest.categories.len());
    for entry in manifest.categories {
        let mut embeddings = Vec::new();
        for phrase in entry.phrases {
            let phrase = phrase.trim();
            if phrase.is_empty() {
                continue;
            }
            embeddings.push(embed_text(state, phrase)?);
        }

        if embeddings.is_empty() {
            continue;
        }

        let mut centroid = vec![0.0_f32; embeddings[0].len()];
        for embedding in &embeddings {
            for (index, value) in embedding.iter().enumerate() {
                centroid[index] += value;
            }
        }

        let count = embeddings.len() as f32;
        for value in &mut centroid {
            *value /= count;
        }
        normalize_vector(&mut centroid);

        built.push(CategoryCentroid {
            category_id: entry.category_id,
            vector: centroid,
        });
    }

    let mut guard = cache
        .lock()
        .map_err(|_| AiTaskError::ProcessingFailed("Centroid cache lock poisoned".to_string()))?;
    *guard = Some(built.clone());
    Ok(built)
}

fn embed_text(state: &AiRuntimeState, text: &str) -> Result<Vec<f32>, AiTaskError> {
    let runner = model_manager::get_session(MODEL_ID, state)
        .ok_or_else(|| AiTaskError::ModelNotLoaded(MODEL_ID.to_string()))?;
    let tokenizer = tokenizer()?;
    let encoding = tokenizer
        .encode(text, true)
        .map_err(|error| AiTaskError::ProcessingFailed(format!("Tokenization failed: {error}")))?;

    let input_ids = encoding
        .get_ids()
        .iter()
        .map(|value| *value as i64)
        .collect::<Vec<_>>();
    let attention_mask = encoding
        .get_attention_mask()
        .iter()
        .map(|value| *value as i64)
        .collect::<Vec<_>>();
    let token_type_ids = encoding
        .get_type_ids()
        .iter()
        .map(|value| *value as i64)
        .collect::<Vec<_>>();

    let seq_len = input_ids.len();
    if seq_len == 0 {
        return Err(AiTaskError::ProcessingFailed(
            "Tokenization produced an empty sequence".to_string(),
        ));
    }

    let mut inputs = Vec::new();
    let session = runner
        .session
        .lock()
        .map_err(|_| AiTaskError::ProcessingFailed("Model session lock poisoned".to_string()))?;
    let input_names = session
        .inputs()
        .iter()
        .map(|input| input.name().to_string())
        .collect::<Vec<_>>();
    drop(session);

    if input_names.iter().any(|name| name == "input_ids") {
        inputs.push((
            "input_ids".to_string(),
            Tensor::<i64>::from_array(([1usize, seq_len], input_ids)).map_err(|error| {
                AiTaskError::ProcessingFailed(format!("input_ids tensor failed: {error}"))
            })?,
        ));
    }
    if input_names.iter().any(|name| name == "attention_mask") {
        inputs.push((
            "attention_mask".to_string(),
            Tensor::<i64>::from_array(([1usize, seq_len], attention_mask.clone())).map_err(
                |error| {
                    AiTaskError::ProcessingFailed(format!("attention_mask tensor failed: {error}"))
                },
            )?,
        ));
    }
    if input_names.iter().any(|name| name == "token_type_ids") {
        inputs.push((
            "token_type_ids".to_string(),
            Tensor::<i64>::from_array(([1usize, seq_len], token_type_ids)).map_err(|error| {
                AiTaskError::ProcessingFailed(format!("token_type_ids tensor failed: {error}"))
            })?,
        ));
    }

    let mut session = runner
        .session
        .lock()
        .map_err(|_| AiTaskError::ProcessingFailed("Model session lock poisoned".to_string()))?;
    let outputs = session.run(inputs).map_err(|error| {
        AiTaskError::ProcessingFailed(format!("Embedding inference failed: {error}"))
    })?;

    let output = outputs
        .get("sentence_embedding")
        .or_else(|| outputs.get("embeddings"))
        .or_else(|| outputs.get("last_hidden_state"))
        .or_else(|| outputs.get("token_embeddings"))
        .or_else(|| outputs.get("text_embeds"))
        .or_else(|| outputs.get("output_0"))
        .or_else(|| outputs.get("output"))
        .or_else(|| outputs.get("sentence_embeddings"))
        .unwrap_or(&outputs[0]);

    let (shape, values) = output.try_extract_tensor::<f32>().map_err(|error| {
        AiTaskError::ProcessingFailed(format!("Model output extraction failed: {error}"))
    })?;
    let dims = shape
        .iter()
        .map(|dimension| usize::try_from(*dimension).unwrap_or(0))
        .collect::<Vec<_>>();

    let mut embedding = match dims.as_slice() {
        [1, hidden] => values[..*hidden].to_vec(),
        [1, tokens, hidden] => {
            let mask = attention_mask
                .iter()
                .map(|value| *value as f32)
                .collect::<Vec<_>>();
            mean_pool(values, &mask, *tokens, *hidden)
        }
        _ => {
            return Err(AiTaskError::ProcessingFailed(format!(
                "Unexpected embedding output shape: {:?}",
                dims
            )));
        }
    };

    normalize_vector(&mut embedding);
    Ok(embedding)
}

fn mean_pool(values: &[f32], attention_mask: &[f32], tokens: usize, hidden: usize) -> Vec<f32> {
    let mut pooled = vec![0.0_f32; hidden];
    let mut weight_sum = 0.0_f32;

    for token_index in 0..tokens {
        let weight = *attention_mask.get(token_index).unwrap_or(&1.0);
        if weight <= 0.0 {
            continue;
        }
        weight_sum += weight;
        let offset = token_index * hidden;
        for hidden_index in 0..hidden {
            pooled[hidden_index] += values[offset + hidden_index] * weight;
        }
    }

    if weight_sum > 0.0 {
        for value in &mut pooled {
            *value /= weight_sum;
        }
    }

    pooled
}

fn classify_embedding(embedding: &[f32], centroids: &[CategoryCentroid]) -> (String, f32) {
    let mut best_category = "uncategorized".to_string();
    let mut best_score = f32::MIN;

    for centroid in centroids {
        let score = cosine_similarity(embedding, &centroid.vector);
        if score > best_score {
            best_score = score;
            best_category = centroid.category_id.clone();
        }
    }

    let confidence = if best_score.is_finite() {
        best_score.max(0.0)
    } else {
        0.0
    };

    (best_category, confidence)
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }

    left.iter()
        .zip(right.iter())
        .map(|(a, b)| a * b)
        .sum::<f32>()
}

fn normalize_vector(vector: &mut [f32]) {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();

    if norm > 0.0 {
        for value in vector {
            *value /= norm;
        }
    }
}
