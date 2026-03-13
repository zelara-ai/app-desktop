use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use std::io::BufReader;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, State};
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use futures_util::{SinkExt, StreamExt};
use local_ip_address::local_ip;
use qrcode::QrCode;
use image::Luma;
use base64::{engine::general_purpose, Engine as _};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::TlsAcceptor;
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub platform: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PairingInfo {
    pub qr_data: String,
    pub qr_image: String, // Base64-encoded PNG image
    pub ip_address: String,        // Primary IP for display
    pub ip_addresses: Vec<String>, // All non-loopback IPs
    pub port: u16,
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRequest {
    pub task_id: String,
    pub task_type: String,
    pub payload: serde_json::Value,
    pub timestamp: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskResponse {
    pub task_id: String,
    pub success: bool,
    pub result: serde_json::Value,
    pub timestamp: String,
}

pub struct DeviceLinkingState {
    pub linked_devices: Arc<Mutex<Vec<DeviceInfo>>>,
    pub pairing_token: Arc<Mutex<Option<String>>>,
    pub server_running: Arc<Mutex<bool>>,
}

impl DeviceLinkingState {
    pub fn new() -> Self {
        Self {
            linked_devices: Arc::new(Mutex::new(Vec::new())),
            pairing_token: Arc::new(Mutex::new(None)),
            server_running: Arc::new(Mutex::new(false)),
        }
    }
}

/// Get primary local IP address
fn get_local_ip() -> Result<String, String> {
    local_ip()
        .map(|ip| ip.to_string())
        .map_err(|e| format!("Failed to get local IP: {}", e))
}

/// Get all non-loopback IPv4 addresses across all interfaces
fn get_all_local_ips() -> Vec<String> {
    match if_addrs::get_if_addrs() {
        Ok(interfaces) => interfaces
            .into_iter()
            .filter_map(|iface| {
                // Keep only IPv4, non-loopback, non-link-local addresses
                if let if_addrs::IfAddr::V4(v4) = iface.addr {
                    if !v4.ip.is_loopback() && !v4.ip.is_link_local() {
                        return Some(v4.ip.to_string());
                    }
                }
                None
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Returns a TLS acceptor backed by a self-signed certificate.
/// The cert is generated once on first launch and persisted to the app data dir.
fn create_tls_acceptor() -> Result<TlsAcceptor, String> {
    let cert_dir = dirs::data_local_dir()
        .ok_or("Could not locate app data directory")?
        .join("Zelara");

    std::fs::create_dir_all(&cert_dir)
        .map_err(|e| format!("Failed to create cert dir: {}", e))?;

    let cert_path = cert_dir.join("zelara_cert.pem");
    let key_path  = cert_dir.join("zelara_key.pem");

    // Generate cert on first run; reuse on subsequent runs
    let (cert_pem, key_pem) = if cert_path.exists() && key_path.exists() {
        let c = std::fs::read_to_string(&cert_path)
            .map_err(|e| format!("Failed to read cert: {}", e))?;
        let k = std::fs::read_to_string(&key_path)
            .map_err(|e| format!("Failed to read key: {}", e))?;
        (c, k)
    } else {
        let subject_alt_names = vec!["zelara.local".to_string(), "localhost".to_string()];
        let rcgen::CertifiedKey { cert, key_pair } =
            rcgen::generate_simple_self_signed(subject_alt_names)
                .map_err(|e| format!("Cert generation failed: {}", e))?;
        let c = cert.pem();
        let k = key_pair.serialize_pem();
        std::fs::write(&cert_path, &c)
            .map_err(|e| format!("Failed to write cert: {}", e))?;
        std::fs::write(&key_path, &k)
            .map_err(|e| format!("Failed to write key: {}", e))?;
        println!("Generated new self-signed TLS certificate at {:?}", cert_dir);
        (c, k)
    };

    // Parse PEM cert chain
    let cert_chain: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut BufReader::new(cert_pem.as_bytes()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to parse TLS cert: {}", e))?;

    // Parse PEM private key
    let private_key: PrivateKeyDer<'static> =
        rustls_pemfile::private_key(&mut BufReader::new(key_pem.as_bytes()))
            .map_err(|e| format!("Failed to parse TLS key: {}", e))?
            .ok_or_else(|| "No private key found in PEM".to_string())?;

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, private_key)
        .map_err(|e| format!("TLS config error: {}", e))?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Compute the SHA-256 fingerprint of a PEM-encoded certificate.
/// Returns a base64-encoded string suitable for certificate pinning on mobile.
fn cert_fingerprint_base64(cert_pem: &str) -> Result<String, String> {
    let der: CertificateDer = rustls_pemfile::certs(&mut BufReader::new(cert_pem.as_bytes()))
        .next()
        .ok_or("No certificate found in PEM")?
        .map_err(|e| format!("Failed to parse cert PEM: {}", e))?;
    let hash = Sha256::digest(der.as_ref());
    let fp = general_purpose::STANDARD.encode(hash);
    println!("[ZelaraTLS] cert_fingerprint_base64: {} (len={})", fp, fp.len());
    Ok(fp)
}

#[tauri::command]
pub fn generate_qr_code(state: State<DeviceLinkingState>) -> Result<PairingInfo, String> {
    // Generate pairing token
    let token = format!("token_{}", uuid::Uuid::new_v4());

    // Get primary IP for display, plus all IPs for QR code
    let ip_address = get_local_ip()?;
    let mut ip_addresses = get_all_local_ips();

    // Ensure primary IP is always present (fallback if enumeration fails)
    if ip_addresses.is_empty() {
        ip_addresses.push(ip_address.clone());
    }

    let port = 8765;

    // Read cert fingerprint for mobile certificate pinning
    let cert_path = dirs::data_local_dir()
        .ok_or("Could not locate app data directory")?
        .join("Zelara")
        .join("zelara_cert.pem");
    let cert_fp = if cert_path.exists() {
        let pem = std::fs::read_to_string(&cert_path)
            .map_err(|e| format!("Failed to read cert: {}", e))?;
        cert_fingerprint_base64(&pem)?
    } else {
        return Err("TLS certificate not found — start the pairing server first".to_string());
    };

    // Encode all IPs comma-separated so mobile can try each one
    let ips_encoded = ip_addresses.join(",");
    println!("[ZelaraTLS] QR cert param: {} (len={})", cert_fp, cert_fp.len());
    let qr_data = format!(
        "zelara://pair?ips={}&port={}&token={}&cert={}",
        ips_encoded, port, token, cert_fp
    );

    // Generate QR code image
    let code = QrCode::new(qr_data.as_bytes())
        .map_err(|e| format!("Failed to generate QR code: {}", e))?;

    // Render to image with scale factor for better visibility
    let image = code.render::<Luma<u8>>()
        .min_dimensions(400, 400)
        .build();

    // Convert to PNG bytes
    let mut png_bytes = Vec::new();
    image.write_to(&mut std::io::Cursor::new(&mut png_bytes), image::ImageFormat::Png)
        .map_err(|e| format!("Failed to encode PNG: {}", e))?;

    // Encode as base64
    let qr_image = general_purpose::STANDARD.encode(&png_bytes);

    // Store token
    *state.pairing_token.lock().unwrap() = Some(token.clone());

    Ok(PairingInfo {
        qr_data,
        qr_image,
        ip_address,
        ip_addresses,
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
pub async fn start_pairing_server(
    app_handle: tauri::AppHandle,
    state: State<'_, DeviceLinkingState>,
) -> Result<(), String> {
    // Check if server is already running
    {
        let running = state.server_running.lock().unwrap();
        if *running {
            return Ok(());
        }
    }

    // Mark server as running
    *state.server_running.lock().unwrap() = true;

    // Ensure Windows Firewall allows inbound on port 8765.
    // Check if the rule exists first; if not, request elevation once via UAC.
    #[cfg(target_os = "windows")]
    {
        let rule_exists = std::process::Command::new("netsh")
            .args(["advfirewall", "firewall", "show", "rule", "name=Zelara Device Linking"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if !rule_exists {
            // Spawn netsh elevated via PowerShell Start-Process -Verb RunAs.
            // This shows a one-time UAC prompt; once the rule exists it is never shown again.
            let _ = std::process::Command::new("powershell")
                .args([
                    "-WindowStyle", "Hidden",
                    "-Command",
                    "Start-Process netsh -ArgumentList 'advfirewall firewall add rule name=\"Zelara Device Linking\" dir=in action=allow protocol=TCP localport=8765' -Verb RunAs -Wait",
                ])
                .output();
        }
    }

    // Build TLS acceptor (generates cert on first run)
    let tls_acceptor = create_tls_acceptor()?;

    // Bind to all interfaces so connections work on any network (WiFi, hotspot, etc.)
    let addr = "0.0.0.0:8765".to_string();

    // Clone Arc for async task
    let server_running = state.server_running.clone();
    let pairing_token = state.pairing_token.clone();
    let linked_devices = state.linked_devices.clone();

    // Spawn server task
    tokio::spawn(async move {
        if let Err(e) = run_websocket_server(&addr, tls_acceptor, server_running, pairing_token, linked_devices, app_handle).await {
            eprintln!("WebSocket server error: {}", e);
        }
    });

    Ok(())
}

async fn run_websocket_server(
    addr: &str,
    tls_acceptor: TlsAcceptor,
    server_running: Arc<Mutex<bool>>,
    pairing_token: Arc<Mutex<Option<String>>>,
    linked_devices: Arc<Mutex<Vec<DeviceInfo>>>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Failed to bind to {}: {}", addr, e))?;

    println!("WSS pairing server listening on {}", addr);

    while *server_running.lock().unwrap() {
        match listener.accept().await {
            Ok((stream, addr)) => {
                println!("New connection from: {}", addr);

                // Clone for async task
                let token = pairing_token.lock().unwrap().clone();
                let devices = linked_devices.clone();
                let remote_addr = addr.to_string();
                let handle = app_handle.clone();
                let acceptor = tls_acceptor.clone();

                tokio::spawn(async move {
                    // Wrap TCP stream with TLS
                    match acceptor.accept(stream).await {
                        Ok(tls_stream) => {
                            match accept_async(tls_stream).await {
                                Ok(ws_stream) => {
                                    if let Err(e) = handle_websocket_connection(ws_stream, token, devices, remote_addr, handle).await {
                                        eprintln!("WebSocket connection error: {}", e);
                                    }
                                }
                                Err(e) => {
                                    eprintln!("WebSocket handshake error: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("TLS handshake error: {}", e);
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

async fn handle_websocket_connection<S>(
    ws_stream: tokio_tungstenite::WebSocketStream<S>,
    expected_token: Option<String>,
    linked_devices: Arc<Mutex<Vec<DeviceInfo>>>,
    remote_addr: String,
    app_handle: tauri::AppHandle,
) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut write, mut read) = ws_stream.split();
    let mut device_registered = false;

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

                    // Register device after successful token verification
                    if !device_registered {
                        let device = DeviceInfo {
                            id: format!("mobile_{}", chrono::Utc::now().timestamp()),
                            name: format!("Mobile ({})", remote_addr),
                            platform: "mobile".to_string(),
                        };

                        let mut devices = linked_devices.lock().unwrap();
                        // Don't add duplicate devices
                        if !devices.iter().any(|d| d.name == device.name) {
                            devices.push(device.clone());
                            println!("Registered mobile device: {}", remote_addr);
                            // Push update to the frontend immediately
                            let _ = app_handle.emit("device-linked", &device);
                        }
                        device_registered = true;
                    }
                }

                // Process task based on type
                let response = match request.task_type.as_str() {
                    "image_validation" => {
                        // Extract image data
                        if let Some(image_data) = request.payload.get("imageData").and_then(|d| d.as_str()) {
                            println!("Received image data: {} bytes", image_data.len());

                            // Call CV processor for actual validation
                            match crate::cv_processor::validate_image(image_data) {
                                Ok(result) => {
                                    println!("CV validation: success={}, confidence={}", result.success, result.confidence);
                                    // Award points on Desktop for successful validation and notify UI
                                    if result.success {
                                        if let Ok(updated) = crate::storage::award_points(10) {
                                            let _ = app_handle.emit("progress-updated", &updated);
                                        }
                                    }
                                    TaskResponse {
                                        task_id: request.task_id,
                                        success: result.success,
                                        result: serde_json::json!({
                                            "success": result.success,
                                            "confidence": result.confidence,
                                            "message": result.message
                                        }),
                                        timestamp: chrono::Utc::now().to_rfc3339(),
                                    }
                                }
                                Err(e) => {
                                    eprintln!("CV validation error: {}", e);
                                    TaskResponse {
                                        task_id: request.task_id,
                                        success: false,
                                        result: serde_json::json!({ "error": format!("Validation failed: {}", e) }),
                                        timestamp: chrono::Utc::now().to_rfc3339(),
                                    }
                                }
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
                    "image_inversion_test" => {
                        // Test feature: Invert image colors and send back
                        if let Some(image_data) = request.payload.get("imageData").and_then(|d| d.as_str()) {
                            println!("Received image for inversion test: {} bytes", image_data.len());

                            match invert_image(image_data) {
                                Ok(inverted_image) => {
                                    println!("Image inverted successfully");
                                    // Notify the desktop UI so it can display both images
                                    let _ = app_handle.emit("image-inversion-result", serde_json::json!({
                                        "original": image_data,
                                        "inverted": inverted_image,
                                        "device": remote_addr,
                                        "timestamp": chrono::Utc::now().to_rfc3339()
                                    }));
                                    TaskResponse {
                                        task_id: request.task_id,
                                        success: true,
                                        result: serde_json::json!({
                                            "invertedImage": inverted_image,
                                            "message": "Image inverted successfully"
                                        }),
                                        timestamp: chrono::Utc::now().to_rfc3339(),
                                    }
                                }
                                Err(e) => {
                                    eprintln!("Image inversion error: {}", e);
                                    TaskResponse {
                                        task_id: request.task_id,
                                        success: false,
                                        result: serde_json::json!({ "error": format!("Inversion failed: {}", e) }),
                                        timestamp: chrono::Utc::now().to_rfc3339(),
                                    }
                                }
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
                    "counter_update" => {
                        if let Some(value) = request.payload.get("value").and_then(|v| v.as_i64()) {
                            let _ = app_handle.emit("counter-update", serde_json::json!({ "value": value }));
                            TaskResponse {
                                task_id: request.task_id,
                                success: true,
                                result: serde_json::json!({ "received": value }),
                                timestamp: chrono::Utc::now().to_rfc3339(),
                            }
                        } else {
                            TaskResponse {
                                task_id: request.task_id,
                                success: false,
                                result: serde_json::json!({ "error": "Missing or invalid counter value" }),
                                timestamp: chrono::Utc::now().to_rfc3339(),
                            }
                        }
                    }
                    "handshake" => {
                        TaskResponse {
                            task_id: request.task_id,
                            success: true,
                            result: serde_json::json!({ "message": "Handshake successful" }),
                            timestamp: chrono::Utc::now().to_rfc3339(),
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

/// Invert image colors (test feature)
fn invert_image(base64_image: &str) -> Result<String, String> {
    use base64::{engine::general_purpose, Engine as _};
    use image::ImageFormat;

    // Decode base64 image
    let image_bytes = general_purpose::STANDARD.decode(base64_image)
        .map_err(|e| format!("Failed to decode image: {}", e))?;

    // Load image from bytes
    let mut img = image::load_from_memory(&image_bytes)
        .map_err(|e| format!("Failed to load image: {}", e))?;

    // Invert colors
    img.invert();

    // Encode back to PNG bytes
    let mut output_bytes = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut output_bytes), ImageFormat::Png)
        .map_err(|e| format!("Failed to encode PNG: {}", e))?;

    // Encode to base64
    let inverted_base64 = general_purpose::STANDARD.encode(&output_bytes);

    Ok(inverted_base64)
}

#[tauri::command]
pub fn stop_pairing_server(state: State<DeviceLinkingState>) -> Result<(), String> {
    *state.server_running.lock().unwrap() = false;
    Ok(())
}

/// Returns all non-loopback IPv4 addresses on this machine.
/// Used by the BLE Discovery Test in the Testing Module to show what
/// the Bluetooth advertisement will broadcast when Phase 3 is implemented.
#[tauri::command]
pub fn get_local_ips() -> Vec<String> {
    get_all_local_ips()
}
