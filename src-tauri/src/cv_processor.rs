use serde::{Deserialize, Serialize};
use tauri::State;
use base64::{engine::general_purpose, Engine as _};
use image::DynamicImage;

#[derive(Debug, Serialize, Deserialize)]
pub struct ValidationResult {
    pub success: bool,
    pub confidence: f32,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ImageValidationRequest {
    pub image_data: String, // Base64 encoded image
}

pub struct CVProcessorState {
    // Future: Load ONNX model here for production use
    // model: Option<ort::Session>,
}

impl CVProcessorState {
    pub fn new() -> Self {
        Self {
            // model: None,
        }
    }
}

/// Analyze image to detect if it contains a paper bag
/// MVP uses color analysis - paper bags typically brown/tan (RGB: 139-101-61 range)
fn analyze_bag_material(img: &DynamicImage) -> (bool, f32, String) {
    let rgb_img = img.to_rgb8();
    let (width, height) = rgb_img.dimensions();

    // Sample center region (bags typically centered in photos)
    let sample_size = 100; // pixels
    let start_x = (width / 2).saturating_sub(sample_size / 2);
    let start_y = (height / 2).saturating_sub(sample_size / 2);

    let mut brown_pixels = 0;
    let mut total_pixels = 0;
    let mut avg_r = 0u32;
    let mut avg_g = 0u32;
    let mut avg_b = 0u32;

    // Analyze center region color distribution
    for y in start_y..std::cmp::min(start_y + sample_size, height) {
        for x in start_x..std::cmp::min(start_x + sample_size, width) {
            let pixel = rgb_img.get_pixel(x, y);
            let r = pixel[0] as u32;
            let g = pixel[1] as u32;
            let b = pixel[2] as u32;

            avg_r += r;
            avg_g += g;
            avg_b += b;
            total_pixels += 1;

            // Brown/tan color range: R>100, G>60, B>30, R>B, G<R
            // Paper bags: earthy browns, tans, light browns
            if r > 100 && g > 60 && b > 30 && r > b && g < r {
                brown_pixels += 1;
            }
        }
    }

    if total_pixels == 0 {
        return (false, 0.0, "Unable to analyze image".to_string());
    }

    avg_r /= total_pixels;
    avg_g /= total_pixels;
    avg_b /= total_pixels;

    let brown_ratio = brown_pixels as f32 / total_pixels as f32;

    // Paper bag detected if >30% brown/tan pixels
    let is_paper = brown_ratio > 0.3;
    let confidence = if is_paper {
        0.6 + (brown_ratio * 0.3) // 0.6 to 0.9 confidence
    } else {
        1.0 - brown_ratio // Lower confidence if not brown
    };

    let message = if is_paper {
        format!("Paper bag detected (avg color: R{} G{} B{}, {}% brown match)",
                avg_r, avg_g, avg_b, (brown_ratio * 100.0) as u32)
    } else {
        format!("Possible plastic or non-paper material (avg color: R{} G{} B{}, {}% brown match)",
                avg_r, avg_g, avg_b, (brown_ratio * 100.0) as u32)
    };

    (is_paper, confidence, message)
}

/// Core validation logic (can be called from anywhere)
pub fn validate_image(image_data: &str) -> Result<ValidationResult, String> {
    // Decode base64 image
    let image_bytes = general_purpose::STANDARD.decode(image_data)
        .map_err(|e| format!("Failed to decode image: {}", e))?;

    // Load image from bytes
    let img = image::load_from_memory(&image_bytes)
        .map_err(|e| format!("Failed to load image: {}", e))?;

    // Analyze image for paper bag characteristics
    let (is_paper, confidence, message) = analyze_bag_material(&img);

    Ok(ValidationResult {
        success: is_paper,
        confidence,
        message,
    })
}

/// Tauri command wrapper for frontend calls
#[tauri::command]
pub async fn validate_recycling_image(
    _state: State<'_, CVProcessorState>,
    request: ImageValidationRequest,
) -> Result<ValidationResult, String> {
    validate_image(&request.image_data)
}
