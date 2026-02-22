use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tauri::State;
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use futures_util::{SinkExt, StreamExt};
use local_ip_address::local_ip;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub platform: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PairingInfo {
    pub qr_data: String,
    pub ip_address: String,
    pub port: u16,
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TaskRequest {
    pub task_id: String,
    pub task_type: String,
    pub payload: serde_json::Value,
    pub timestamp: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TaskResponse {
    pub task_id: String,
    pub success: bool,
    pub result: serde_json::Value,
    pub timestamp: String,
}

pub struct DeviceLinkingState {
    pub linked_devices: Mutex<Vec<DeviceInfo>>,
    pub pairing_token: Mutex<Option<String>>,
    pub server_running: Arc<Mutex<bool>>,
}

impl DeviceLinkingState {
    pub fn new() -> Self {
        Self {
            linked_devices: Mutex::new(Vec::new()),
            pairing_token: Mutex::new(None),
            server_running: Arc::new(Mutex::new(false)),
        }
    }
}

/// Get local IP address
fn get_local_ip() -> Result<String, String> {
    local_ip()
        .map(|ip| ip.to_string())
        .map_err(|e| format!("Failed to get local IP: {}", e))
}

#[tauri::command]
pub fn generate_qr_code(state: State<DeviceLinkingState>) -> Result<PairingInfo, String> {
    // Generate pairing token
    let token = format!("token_{}", uuid::Uuid::new_v4());

    // Get actual local IP
    let ip_address = get_local_ip()?;
    let port = 8765;

    // Create QR data
    let qr_data = format!("zelara://pair?ip={}&port={}&token={}", ip_address, port, token);

    // Store token
    *state.pairing_token.lock().unwrap() = Some(token.clone());

    Ok(PairingInfo {
        qr_data,
        ip_address,
        port,
        token,
    })
}

#[tauri::command]
pub fn get_linked_devices(state: State<DeviceLinkingState>) -> Result<Vec<DeviceInfo>, String> {
    Ok(state.linked_devices.lock().unwrap().clone())
}

#[tauri::command]
pub fn add_linked_device(
    state: State<DeviceLinkingState>,
    device: DeviceInfo,
) -> Result<(), String> {
    state.linked_devices.lock().unwrap().push(device);
    Ok(())
}

#[tauri::command]
pub async fn start_pairing_server(state: State<'_, DeviceLinkingState>) -> Result<(), String> {
    // Check if server is already running
    {
        let running = state.server_running.lock().unwrap();
        if *running {
            return Ok(());
        }
    }

    // Mark server as running
    *state.server_running.lock().unwrap() = true;

    // Get local IP
    let ip = get_local_ip()?;
    let addr = format!("{}:8765", ip);

    // Clone Arc for async task
    let server_running = state.server_running.clone();
    let pairing_token = state.pairing_token.clone();

    // Spawn server task
    tokio::spawn(async move {
        if let Err(e) = run_websocket_server(&addr, server_running, pairing_token).await {
            eprintln!("WebSocket server error: {}", e);
        }
    });

    Ok(())
}

async fn run_websocket_server(
    addr: &str,
    server_running: Arc<Mutex<bool>>,
    pairing_token: Arc<Mutex<Option<String>>>,
) -> Result<(), String> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Failed to bind to {}: {}", addr, e))?;

    println!("WebSocket pairing server listening on {}", addr);

    while *server_running.lock().unwrap() {
        match listener.accept().await {
            Ok((stream, addr)) => {
                println!("New connection from: {}", addr);

                // Clone for async task
                let token = pairing_token.lock().unwrap().clone();

                tokio::spawn(async move {
                    match accept_async(stream).await {
                        Ok(ws_stream) => {
                            if let Err(e) = handle_websocket_connection(ws_stream, token).await {
                                eprintln!("WebSocket connection error: {}", e);
                            }
                        }
                        Err(e) => {
                            eprintln!("WebSocket handshake error: {}", e);
                        }
                    }
                });
            }
            Err(e) => {
                eprintln!("Accept error: {}", e);
            }
        }
    }

    Ok(())
}

async fn handle_websocket_connection(
    ws_stream: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    expected_token: Option<String>,
) -> Result<(), String> {
    let (mut write, mut read) = ws_stream.split();

    while let Some(message) = read.next().await {
        match message {
            Ok(Message::Text(text)) => {
                // Parse request
                let request: TaskRequest = serde_json::from_str(&text)
                    .map_err(|e| format!("Failed to parse request: {}", e))?;

                println!("Received task: {} ({})", request.task_id, request.task_type);

                // Verify token (first request should include token)
                if let Some(token) = request.payload.get("token").and_then(|t| t.as_str()) {
                    if Some(token.to_string()) != expected_token {
                        let error_response = TaskResponse {
                            task_id: request.task_id,
                            success: false,
                            result: serde_json::json!({ "error": "Invalid pairing token" }),
                            timestamp: chrono::Utc::now().to_rfc3339(),
                        };

                        let response_text = serde_json::to_string(&error_response)
                            .map_err(|e| format!("Failed to serialize response: {}", e))?;
                        write.send(Message::Text(response_text)).await
                            .map_err(|e| format!("Failed to send response: {}", e))?;
                        return Err("Invalid token".to_string());
                    }
                }

                // Process task based on type
                let response = match request.task_type.as_str() {
                    "image_validation" => {
                        // Extract image data
                        if let Some(image_data) = request.payload.get("imageData").and_then(|d| d.as_str()) {
                            // TODO: Call cv_processor to validate image
                            // For now, return mock success
                            println!("Received image data: {} bytes", image_data.len());

                            TaskResponse {
                                task_id: request.task_id,
                                success: true,
                                result: serde_json::json!({
                                    "success": true,
                                    "confidence": 0.85,
                                    "message": "Paper bag with recyclable items detected"
                                }),
                                timestamp: chrono::Utc::now().to_rfc3339(),
                            }
                        } else {
                            TaskResponse {
                                task_id: request.task_id,
                                success: false,
                                result: serde_json::json!({ "error": "Missing image data" }),
                                timestamp: chrono::Utc::now().to_rfc3339(),
                            }
                        }
                    }
                    _ => {
                        TaskResponse {
                            task_id: request.task_id,
                            success: false,
                            result: serde_json::json!({ "error": "Unknown task type" }),
                            timestamp: chrono::Utc::now().to_rfc3339(),
                        }
                    }
                };

                // Send response
                let response_text = serde_json::to_string(&response)
                    .map_err(|e| format!("Failed to serialize response: {}", e))?;
                write.send(Message::Text(response_text)).await
                    .map_err(|e| format!("Failed to send response: {}", e))?;
            }
            Ok(Message::Close(_)) => {
                println!("WebSocket connection closed");
                break;
            }
            Ok(_) => {
                // Ignore other message types (Binary, Ping, Pong)
            }
            Err(e) => {
                eprintln!("WebSocket read error: {}", e);
                break;
            }
        }
    }

    Ok(())
}

#[tauri::command]
pub fn stop_pairing_server(state: State<DeviceLinkingState>) -> Result<(), String> {
    *state.server_running.lock().unwrap() = false;
    Ok(())
}
