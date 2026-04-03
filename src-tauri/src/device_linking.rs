use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use std::collections::HashMap;
use std::io::BufReader;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, State};
use tokio::net::TcpListener;
use tokio::sync::mpsc::UnboundedSender;
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
    pub discovery_method: String, // "ble" | "qr"
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

/// Shared registry of per-connection push senders (keyed by remote_addr).
/// Used by `broadcast_progress_sync` to push progress updates to all Mobile clients.
type ClientSenders = Arc<Mutex<HashMap<String, UnboundedSender<Message>>>>;

pub struct DeviceLinkingState {
    pub linked_devices: Arc<Mutex<Vec<DeviceInfo>>>,
    pub pairing_token: Arc<Mutex<Option<String>>>,
    pub server_running: Arc<Mutex<bool>>,
    pub client_senders: ClientSenders,
}

impl DeviceLinkingState {
    pub fn new() -> Self {
        Self {
            linked_devices: Arc::new(Mutex::new(Vec::new())),
            pairing_token: Arc::new(Mutex::new(None)),
            server_running: Arc::new(Mutex::new(false)),
            client_senders: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// Get primary local IP address
fn get_local_ip() -> Result<String, String> {
    get_primary_ip_pub()
}

/// Public helper — used by ble_advertising to get the primary IP for broadcasting.
pub fn get_primary_ip_pub() -> Result<String, String> {
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
    let client_senders = state.client_senders.clone();

    // Spawn server task
    tokio::spawn(async move {
        if let Err(e) = run_websocket_server(&addr, tls_acceptor, server_running, pairing_token, linked_devices, client_senders, app_handle).await {
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
    client_senders: ClientSenders,
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
                let senders = client_senders.clone();

                tokio::spawn(async move {
                    // Wrap TCP stream with TLS
                    match acceptor.accept(stream).await {
                        Ok(tls_stream) => {
                            match accept_async(tls_stream).await {
                                Ok(ws_stream) => {
                                    if let Err(e) = handle_websocket_connection(ws_stream, token, devices, senders, remote_addr, handle).await {
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

/// Serialize `progress` as a `progress_sync` JSON text frame and enqueue it
/// on every connected Mobile client's push channel.
/// Desktop is authoritative; Mobile adopts the received state.
fn broadcast_progress_sync(client_senders: &ClientSenders, progress: &crate::storage::UserProgress) {
    let payload = serde_json::json!({
        "type": "progress_sync",
        "points": progress.points,
        "unlockedModules": progress.unlocked_modules,
        "availableUnlocks": progress.available_unlocks,
        "lastUpdated": progress.last_updated,
    });
    if let Ok(text) = serde_json::to_string(&payload) {
        let msg = Message::Text(text);
        let senders = client_senders.lock().unwrap();
        for (addr, tx) in senders.iter() {
            if tx.send(msg.clone()).is_err() {
                eprintln!("[ProgressSync] dead sender for {}", addr);
            }
        }
    }
}

async fn handle_websocket_connection<S>(
    ws_stream: tokio_tungstenite::WebSocketStream<S>,
    expected_token: Option<String>,
    linked_devices: Arc<Mutex<Vec<DeviceInfo>>>,
    client_senders: ClientSenders,
    remote_addr: String,
    app_handle: tauri::AppHandle,
) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut write, mut read) = ws_stream.split();
    let mut device_registered = false;
    // Stable device ID sent by mobile in the handshake; used for cleanup on disconnect.
    let mut registered_device_id: Option<String> = None;
    // Set on the first authenticated message; once true, token checks are skipped for the
    // lifetime of this connection (BLE proximity is the trust anchor for the whole session).
    let mut is_ble_connection = false;

    // Channel for the spawned inversion task to return its result to the write loop.
    let (inversion_tx, mut inversion_rx) = tokio::sync::mpsc::channel::<InversionOutcome>(1);
    // State for a binary image upload currently being assembled from chunks.
    let mut inbound_transfer: Option<InboundImageTransfer> = None;

    // Per-connection push channel: broadcast_progress_sync enqueues messages here;
    // Arm 3 of the select loop drains them and writes to the WebSocket.
    let (push_tx, mut push_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();
    {
        client_senders.lock().unwrap().insert(remote_addr.clone(), push_tx);
    }

    'outer: loop {
        tokio::select! {
            // ── Arm 1: incoming WebSocket message ──────────────────────────
            msg = read.next() => {
                match msg {
                    None => break 'outer,
                    Some(Err(e)) => {
                        eprintln!("WebSocket read error from {}: {}", remote_addr, e);
                        break 'outer;
                    }
                    Some(Ok(Message::Close(_))) => {
                        println!("WebSocket connection closed: {}", remote_addr);
                        break 'outer;
                    }
                    Some(Ok(Message::Binary(data))) => {
                        // Binary chunk: [4 bytes uint32 BE: chunk_index][raw bytes]
                        if data.len() < 4 {
                            eprintln!("[BinaryXfer] Frame too short: {} bytes", data.len());
                            continue 'outer;
                        }
                        let chunk_index = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                        let chunk_data = data[4..].to_vec();
                        match inbound_transfer {
                            Some(ref t) if t.is_expired() => {
                                eprintln!("[BinaryXfer] Transfer '{}' expired (>{}s) — discarding", t.task_id, BINARY_TRANSFER_TIMEOUT_SECS);
                                inbound_transfer = None;
                            }
                            Some(ref mut transfer) => {
                                transfer.insert_chunk(chunk_index, chunk_data);
                                println!("[BinaryXfer] Chunk {} received ({} bytes)", chunk_index, data.len() - 4);
                            }
                            None => {
                                eprintln!("[BinaryXfer] Binary frame received with no active transfer — ignoring");
                            }
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        // Check for binary transfer control messages before parsing as TaskRequest.
                        if let Ok(ctrl) = serde_json::from_str::<serde_json::Value>(&text) {
                            match ctrl.get("type").and_then(|t| t.as_str()) {
                                Some("img_start") => {
                                    let task_id = ctrl["taskId"].as_str().unwrap_or("").to_string();
                                    let total = ctrl["totalChunks"].as_u64().unwrap_or(0) as usize;
                                    let mime = ctrl["mime"].as_str().unwrap_or("image/jpeg").to_string();
                                    println!("[BinaryXfer] img_start taskId={} totalChunks={}", task_id, total);
                                    inbound_transfer = Some(InboundImageTransfer::new(task_id, total, mime));
                                    continue 'outer;
                                }
                                Some("img_end") => {
                                    let task_id = ctrl["taskId"].as_str().unwrap_or("").to_string();
                                    println!("[BinaryXfer] img_end taskId={}", task_id);
                                    if let Some(transfer) = inbound_transfer.take() {
                                        if transfer.is_expired() {
                                            eprintln!("[BinaryXfer] img_end: transfer '{}' expired (>{}s) — discarding", task_id, BINARY_TRANSFER_TIMEOUT_SECS);
                                            continue 'outer;
                                        }
                                        if transfer.task_id == task_id && transfer.is_complete() {
                                            let assembled = transfer.assemble();
                                            let original = assembled.clone();
                                            let tid = task_id.clone();
                                            let tx = inversion_tx.clone();
                                            let addr = remote_addr.clone();
                                            tokio::task::spawn_blocking(move || {
                                                match invert_image_bytes(assembled) {
                                                    Ok(png) => { let _ = tx.blocking_send(InversionOutcome::Success { task_id: tid, png_bytes: png, original_bytes: original, addr }); }
                                                    Err(e) => { let _ = tx.blocking_send(InversionOutcome::Failure { task_id: tid, error: e }); }
                                                }
                                            });
                                        } else {
                                            eprintln!("[BinaryXfer] img_end: incomplete or taskId mismatch (got '{}', have '{}')", task_id, transfer.task_id);
                                        }
                                    } else {
                                        eprintln!("[BinaryXfer] img_end with no active transfer");
                                    }
                                    continue 'outer;
                                }
                                _ => {} // not a control message — fall through to TaskRequest
                            }
                        }

                        // Normal TaskRequest handling
                        let request: TaskRequest = serde_json::from_str(&text)
                            .map_err(|e| format!("Failed to parse request: {}", e))?;

                        println!("Received task: {} ({})", request.task_id, request.task_type);

                        // Determine discovery method from this message (only present on handshake)
                        let msg_discovery_method = request.payload
                            .get("discovery_method")
                            .and_then(|v| v.as_str())
                            .unwrap_or("qr");

                        // Mark connection as BLE once we see a BLE handshake; sticky for the session.
                        if msg_discovery_method == "ble" {
                            is_ble_connection = true;
                        }

                        // Verify token for QR connections; BLE connections are accepted by proximity.
                        let token_ok = if is_ble_connection {
                            true
                        } else if let Some(token) = request.payload.get("token").and_then(|t| t.as_str()) {
                            Some(token.to_string()) == expected_token
                        } else {
                            false
                        };

                        if !token_ok {
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

                        // Register device on first authenticated message
                        if !device_registered && token_ok {
                            let stable_id = request.payload
                                .get("device_id")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| format!("mobile_{}", chrono::Utc::now().timestamp()));

                            let method_label = if is_ble_connection { "ble" } else { "qr" };
                            let device = DeviceInfo {
                                id: stable_id.clone(),
                                name: format!("Mobile ({})", remote_addr),
                                platform: "mobile".to_string(),
                                discovery_method: method_label.to_string(),
                            };

                            let mut devices = linked_devices.lock().unwrap();
                            // Deduplicate by stable device ID — if same device reconnects, replace the old entry.
                            if let Some(existing) = devices.iter_mut().find(|d| d.id == device.id) {
                                existing.name = device.name.clone();
                                existing.discovery_method = device.discovery_method.clone();
                                println!("Re-registered known device: {} ({}) via {}", stable_id, remote_addr, method_label);
                            } else {
                                devices.push(device.clone());
                                println!("Registered mobile device: {} ({}) via {}", stable_id, remote_addr, method_label);
                                let _ = app_handle.emit("device-linked", &device);
                            }
                            registered_device_id = Some(stable_id);
                            device_registered = true;

                            // Push current Desktop progress immediately so Mobile starts in sync.
                            if let Ok(current_progress) = crate::storage::load_progress() {
                                broadcast_progress_sync(&client_senders, &current_progress);
                            }
                        }

                        // Process task based on type
                        let response = match request.task_type.as_str() {
                            "image_validation" => {
                                if let Some(image_data) = request.payload.get("imageData").and_then(|d| d.as_str()) {
                                    println!("Received image data: {} bytes", image_data.len());
                                    match crate::cv_processor::validate_image(image_data) {
                                        Ok(result) => {
                                            println!("CV validation: success={}, confidence={}", result.success, result.confidence);
                                            if result.success {
                                                if let Ok(updated) = crate::storage::award_points(10) {
                                                    let _ = app_handle.emit("progress-updated", &updated);
                                                    broadcast_progress_sync(&client_senders, &updated);
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
                                let client_version = request.payload
                                    .get("protocolVersion")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                if client_version != PROTOCOL_VERSION as u64 {
                                    eprintln!(
                                        "[Protocol] Version mismatch: Desktop={} Mobile={} — binary protocol may behave unexpectedly",
                                        PROTOCOL_VERSION, client_version
                                    );
                                }
                                TaskResponse {
                                    task_id: request.task_id,
                                    success: true,
                                    result: serde_json::json!({
                                        "message": "Handshake successful",
                                        "protocolVersion": PROTOCOL_VERSION
                                    }),
                                    timestamp: chrono::Utc::now().to_rfc3339(),
                                }
                            }
                            "request_sync" => {
                                match crate::storage::load_progress() {
                                    Ok(p) => {
                                        broadcast_progress_sync(&client_senders, &p);
                                        TaskResponse {
                                            task_id: request.task_id,
                                            success: true,
                                            result: serde_json::json!({ "message": "Sync dispatched" }),
                                            timestamp: chrono::Utc::now().to_rfc3339(),
                                        }
                                    }
                                    Err(e) => TaskResponse {
                                        task_id: request.task_id,
                                        success: false,
                                        result: serde_json::json!({ "error": e }),
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
                    Some(Ok(_)) => {} // Ping, Pong — ignore
                }
            }

            // ── Arm 2: inversion task completed on blocking thread pool ────
            Some(outcome) = inversion_rx.recv() => {
                match outcome {
                    InversionOutcome::Success { task_id, png_bytes, original_bytes, addr } => {
                        send_binary_result(&mut write, &task_id, &png_bytes, &original_bytes, &app_handle, &addr).await;
                    }
                    InversionOutcome::Failure { task_id, error } => {
                        let ctrl = serde_json::to_string(&serde_json::json!({
                            "type": "img_result_end",
                            "taskId": task_id,
                            "success": false,
                            "error": error
                        })).unwrap_or_default();
                        let _ = write.send(Message::Text(ctrl)).await;
                    }
                }
            }

            // ── Arm 3: outbound push messages (progress_sync, etc.) ─────────
            Some(msg) = push_rx.recv() => {
                if write.send(msg).await.is_err() {
                    break 'outer;
                }
            }
        }
    }

    // Deregister this connection's push sender so broadcast_progress_sync skips it.
    client_senders.lock().unwrap().remove(&remote_addr);

    // Remove device from the linked list and notify the frontend regardless of
    // whether the connection ended cleanly (Close frame) or due to an error.
    if let Some(id) = registered_device_id {
        let mut devices = linked_devices.lock().unwrap();
        let removed_method = devices.iter()
            .find(|d| d.id == id)
            .map(|d| d.discovery_method.clone())
            .unwrap_or_else(|| "qr".to_string());
        devices.retain(|d| d.id != id);
        drop(devices);
        println!("Device disconnected and removed: {}", id);
        let _ = app_handle.emit("device-disconnected", serde_json::json!({
            "id": id,
            "discovery_method": removed_method
        }));
    }

    Ok(())
}

/// Tauri command wrapper for award_points — adds broadcast to connected Mobile clients.
#[tauri::command]
pub fn award_points(
    points_to_add: i32,
    state: State<DeviceLinkingState>,
    app_handle: tauri::AppHandle,
) -> Result<crate::storage::UserProgress, String> {
    let updated = crate::storage::award_points(points_to_add)?;
    let _ = app_handle.emit("progress-updated", &updated);
    broadcast_progress_sync(&state.client_senders, &updated);
    Ok(updated)
}

/// Tauri command wrapper for reset_points — zeros points and broadcasts to connected Mobile clients.
#[tauri::command]
pub fn reset_points(
    state: State<DeviceLinkingState>,
    app_handle: tauri::AppHandle,
) -> Result<crate::storage::UserProgress, String> {
    let updated = crate::storage::reset_points()?;
    let _ = app_handle.emit("progress-updated", &updated);
    broadcast_progress_sync(&state.client_senders, &updated);
    Ok(updated)
}

/// Tauri command wrapper for unlock_module — adds broadcast to connected Mobile clients.
#[tauri::command]
pub fn unlock_module(
    module_name: String,
    state: State<DeviceLinkingState>,
    app_handle: tauri::AppHandle,
) -> Result<crate::storage::UserProgress, String> {
    let updated = crate::storage::unlock_module(module_name)?;
    let _ = app_handle.emit("progress-updated", &updated);
    broadcast_progress_sync(&state.client_senders, &updated);
    Ok(updated)
}

/// Invert image colors from raw bytes, returning raw PNG bytes.
/// No base64 — used by the binary transfer path.
fn invert_image_bytes(image_bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    use image::ImageFormat;

    let mut img = image::load_from_memory(&image_bytes)
        .map_err(|e| format!("Failed to load image: {}", e))?;
    img.invert();
    let mut output_bytes = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut output_bytes), ImageFormat::Png)
        .map_err(|e| format!("Failed to encode PNG: {}", e))?;
    Ok(output_bytes)
}

/// If no `img_end` arrives within this window after `img_start`, the transfer is discarded.
const BINARY_TRANSFER_TIMEOUT_SECS: u64 = 30;

/// State for a binary image upload in progress (chunked WebSocket frames).
struct InboundImageTransfer {
    task_id: String,
    total_chunks: usize,
    #[allow(dead_code)]
    mime: String,
    chunks: HashMap<u32, Vec<u8>>,
    started_at: std::time::Instant,
}

impl InboundImageTransfer {
    fn new(task_id: String, total_chunks: usize, mime: String) -> Self {
        Self {
            task_id,
            total_chunks,
            mime,
            chunks: HashMap::with_capacity(total_chunks),
            started_at: std::time::Instant::now(),
        }
    }

    fn is_expired(&self) -> bool {
        self.started_at.elapsed().as_secs() >= BINARY_TRANSFER_TIMEOUT_SECS
    }

    fn insert_chunk(&mut self, index: u32, data: Vec<u8>) {
        self.chunks.insert(index, data);
    }

    fn is_complete(&self) -> bool {
        self.chunks.len() == self.total_chunks
    }

    fn assemble(&self) -> Vec<u8> {
        let total_len: usize = self.chunks.values().map(|v| v.len()).sum();
        let mut out = Vec::with_capacity(total_len);
        for i in 0..self.total_chunks as u32 {
            if let Some(chunk) = self.chunks.get(&i) {
                out.extend_from_slice(chunk);
            }
        }
        out
    }
}

/// Result sent from the `spawn_blocking` inversion task back to the write loop.
enum InversionOutcome {
    Success {
        task_id: String,
        png_bytes: Vec<u8>,
        original_bytes: Vec<u8>,
        addr: String,
    },
    Failure {
        task_id: String,
        error: String,
    },
}

/// Increment when the binary transfer protocol changes in a breaking way.
/// Must stay in sync with PROTOCOL_VERSION in DeviceLinkingService.ts.
const PROTOCOL_VERSION: u32 = 1;

const BINARY_CHUNK_SIZE: usize = 65536;

/// Send an inverted PNG back to mobile as binary chunks, then emit the Tauri desktop UI event.
async fn send_binary_result<S>(
    write: &mut futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, Message>,
    task_id: &str,
    png_bytes: &[u8],
    original_bytes: &[u8],
    app_handle: &tauri::AppHandle,
    remote_addr: &str,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let total_chunks = (png_bytes.len() + BINARY_CHUNK_SIZE - 1) / BINARY_CHUNK_SIZE;

    // 1. result_start control frame
    let start = serde_json::to_string(&serde_json::json!({
        "type": "img_result_start",
        "taskId": task_id,
        "totalChunks": total_chunks
    })).unwrap_or_default();
    if write.send(Message::Text(start)).await.is_err() { return; }

    // 2. binary chunks — [4 bytes uint32 BE: chunk_index][raw PNG bytes]
    for (i, chunk) in png_bytes.chunks(BINARY_CHUNK_SIZE).enumerate() {
        let mut frame = Vec::with_capacity(4 + chunk.len());
        frame.extend_from_slice(&(i as u32).to_be_bytes());
        frame.extend_from_slice(chunk);
        if write.send(Message::Binary(frame)).await.is_err() { return; }
    }

    // 3. result_end control frame
    let end = serde_json::to_string(&serde_json::json!({
        "type": "img_result_end",
        "taskId": task_id,
        "success": true
    })).unwrap_or_default();
    if write.send(Message::Text(end)).await.is_err() { return; }

    // 4. Tauri event for Desktop UI — still base64 (internal IPC, not a WebSocket hop)
    let _ = app_handle.emit("image-inversion-result", serde_json::json!({
        "original": general_purpose::STANDARD.encode(original_bytes),
        "inverted": general_purpose::STANDARD.encode(png_bytes),
        "device": remote_addr,
        "timestamp": chrono::Utc::now().to_rfc3339()
    }));
}

#[tauri::command]
pub fn stop_pairing_server(state: State<DeviceLinkingState>) -> Result<(), String> {
    *state.server_running.lock().unwrap() = false;
    Ok(())
}

/// Returns all non-loopback IPv4 addresses on this machine.
/// Used by the BLE advertising module to determine which IPs to advertise,
/// and by the Testing Module to display the current advertisement IPs.
#[tauri::command]
pub fn get_local_ips() -> Vec<String> {
    get_all_local_ips()
}
