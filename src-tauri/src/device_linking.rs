use base64::{engine::general_purpose, Engine as _};
use futures_util::{SinkExt, StreamExt};
use image::Luma;
use qrcode::QrCode;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::BufReader;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, State};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::{sleep, Duration};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

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
    pub qr_image: String,          // Base64-encoded PNG image
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
    #[serde(default)]
    pub capability: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskResponse {
    pub task_id: String,
    pub success: bool,
    pub result: serde_json::Value,
    pub timestamp: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ImageProcessingState {
    pub task_id: String,
    pub status: String,
    pub message: String,
    pub device: String,
    pub effect_name: String,
    pub progress: u8,
    pub updated_at: String,
    pub original_image: Option<String>,
    pub original_mime: String,
    pub processed_image: Option<String>,
    pub processed_mime: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FinanceContextCategory {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub color: String,
    pub budget_limit: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FinanceContextHistoryEntry {
    pub description: String,
    pub merchant_name: Option<String>,
    pub category: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FinanceContextSnapshot {
    pub categories: Vec<FinanceContextCategory>,
    pub history: Vec<FinanceContextHistoryEntry>,
    pub synced_at: String,
}

/// Shared registry of per-connection push senders (keyed by remote_addr).
/// Used by `broadcast_progress_sync` to push progress updates to all Mobile clients.
type ClientSenders = Arc<Mutex<HashMap<String, UnboundedSender<Message>>>>;
type LatestImageProcessing = Arc<Mutex<Option<ImageProcessingState>>>;
type LatestFinanceContext = Arc<Mutex<Option<FinanceContextSnapshot>>>;

pub struct DeviceLinkingState {
    pub linked_devices: Arc<Mutex<Vec<DeviceInfo>>>,
    pub pairing_token: Arc<Mutex<Option<String>>>,
    pub server_running: Arc<Mutex<bool>>,
    pub client_senders: ClientSenders,
    pub latest_image_processing: LatestImageProcessing,
    pub latest_finance_context: LatestFinanceContext,
}

impl DeviceLinkingState {
    pub fn new() -> Self {
        Self {
            linked_devices: Arc::new(Mutex::new(Vec::new())),
            pairing_token: Arc::new(Mutex::new(None)),
            server_running: Arc::new(Mutex::new(false)),
            client_senders: Arc::new(Mutex::new(HashMap::new())),
            latest_image_processing: Arc::new(Mutex::new(None)),
            latest_finance_context: Arc::new(Mutex::new(None)),
        }
    }
}

/// Get primary local IP address
fn get_local_ip() -> Result<String, String> {
    get_primary_ip_pub()
}

#[derive(Debug, Clone)]
struct LocalIpv4Candidate {
    interface_name: String,
    ip: Ipv4Addr,
    netmask: Ipv4Addr,
}

fn get_local_ipv4_candidates() -> Vec<LocalIpv4Candidate> {
    match if_addrs::get_if_addrs() {
        Ok(interfaces) => interfaces
            .into_iter()
            .filter_map(|iface| {
                if let if_addrs::IfAddr::V4(v4) = iface.addr {
                    if !v4.ip.is_loopback() && !v4.ip.is_link_local() {
                        return Some(LocalIpv4Candidate {
                            interface_name: iface.name,
                            ip: v4.ip,
                            netmask: v4.netmask,
                        });
                    }
                }
                None
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn score_local_ip_candidate(candidate: &LocalIpv4Candidate) -> i32 {
    let mut score = 0;
    let name = candidate.interface_name.to_ascii_lowercase();
    let octets = candidate.ip.octets();

    if candidate.ip.is_private() {
        score += 50;
        match octets {
            [192, 168, _, _] => score += 20,
            [172, second, _, _] if (16..=31).contains(&second) => score += 15,
            [10, _, _, _] => score += 10,
            _ => {}
        }
    } else {
        score -= 25;
    }

    if candidate.netmask == Ipv4Addr::new(255, 255, 255, 255) {
        score -= 30;
    } else {
        score += 20;
    }

    if name.contains("wi-fi")
        || name.contains("wifi")
        || name.contains("wlan")
        || name.contains("wireless")
    {
        score += 40;
    }
    if name.contains("ethernet") || name.starts_with("eth") || name.starts_with("en") {
        score += 25;
    }
    if name.contains("wi-fi direct")
        || name.contains("mobile hotspot")
        || name.contains("local area connection")
    {
        score += 30;
    }

    let suspicious_interface_markers = [
        "protonvpn",
        "tailscale",
        "wireguard",
        "vpn",
        "tun",
        "tap",
        "vethernet",
        "hyper-v",
        "wsl",
        "virtualbox",
        "vmware",
        "docker",
        "container",
        "loopback",
    ];
    if suspicious_interface_markers
        .iter()
        .any(|marker| name.contains(marker))
    {
        score -= 100;
    }

    score
}

fn sort_local_ip_candidates(candidates: &mut [LocalIpv4Candidate]) {
    candidates.sort_by(|a, b| {
        score_local_ip_candidate(b)
            .cmp(&score_local_ip_candidate(a))
            .then_with(|| a.interface_name.cmp(&b.interface_name))
            .then_with(|| a.ip.octets().cmp(&b.ip.octets()))
    });
}

/// Public helper — used by BLE advertising and QR display to pick the most
/// LAN-reachable IPv4 address instead of a VPN / virtual adapter.
pub fn get_primary_ip_pub() -> Result<String, String> {
    let mut candidates = get_local_ipv4_candidates();
    if candidates.is_empty() {
        return Err("Failed to get local IP: no non-loopback IPv4 addresses found".to_string());
    }

    sort_local_ip_candidates(&mut candidates);
    let ranked = candidates
        .iter()
        .map(|candidate| {
            format!(
                "{}@{}(score={})",
                candidate.ip,
                candidate.interface_name,
                score_local_ip_candidate(candidate)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let chosen = &candidates[0];
    println!(
        "[DeviceLinking] Selected preferred local IP {} on '{}' from [{}]",
        chosen.ip, chosen.interface_name, ranked
    );

    Ok(chosen.ip.to_string())
}

/// Get all non-loopback IPv4 addresses across all interfaces, ordered with the
/// most likely LAN-reachable address first.
fn get_all_local_ips() -> Vec<String> {
    let mut candidates = get_local_ipv4_candidates();
    sort_local_ip_candidates(&mut candidates);

    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter_map(|candidate| {
            if seen.insert(candidate.ip) {
                Some(candidate.ip.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Returns a TLS acceptor backed by a self-signed certificate.
/// The cert is generated once on first launch and persisted to the app data dir.
fn create_tls_acceptor() -> Result<TlsAcceptor, String> {
    let cert_dir = dirs::data_local_dir()
        .ok_or("Could not locate app data directory")?
        .join("Zelara");

    std::fs::create_dir_all(&cert_dir).map_err(|e| format!("Failed to create cert dir: {}", e))?;

    let cert_path = cert_dir.join("zelara_cert.pem");
    let key_path = cert_dir.join("zelara_key.pem");

    // Generate cert on first run; reuse on subsequent runs
    let (cert_pem, key_pem) = if cert_path.exists() && key_path.exists() {
        let c = std::fs::read_to_string(&cert_path)
            .map_err(|e| format!("Failed to read cert: {}", e))?;
        let k =
            std::fs::read_to_string(&key_path).map_err(|e| format!("Failed to read key: {}", e))?;
        (c, k)
    } else {
        let subject_alt_names = vec!["zelara.local".to_string(), "localhost".to_string()];
        let rcgen::CertifiedKey { cert, key_pair } =
            rcgen::generate_simple_self_signed(subject_alt_names)
                .map_err(|e| format!("Cert generation failed: {}", e))?;
        let c = cert.pem();
        let k = key_pair.serialize_pem();
        std::fs::write(&cert_path, &c).map_err(|e| format!("Failed to write cert: {}", e))?;
        std::fs::write(&key_path, &k).map_err(|e| format!("Failed to write key: {}", e))?;
        println!(
            "Generated new self-signed TLS certificate at {:?}",
            cert_dir
        );
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
    println!(
        "[ZelaraTLS] cert_fingerprint_base64: {} (len={})",
        fp,
        fp.len()
    );
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
    println!(
        "[ZelaraTLS] QR cert param: {} (len={})",
        cert_fp,
        cert_fp.len()
    );
    let qr_data = format!(
        "zelara://pair?ips={}&port={}&token={}&cert={}",
        ips_encoded, port, token, cert_fp
    );

    // Generate QR code image
    let code = QrCode::new(qr_data.as_bytes())
        .map_err(|e| format!("Failed to generate QR code: {}", e))?;

    // Render to image with scale factor for better visibility
    let image = code.render::<Luma<u8>>().min_dimensions(400, 400).build();

    // Convert to PNG bytes
    let mut png_bytes = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
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
    ai_state: State<'_, std::sync::Arc<crate::ai::AiRuntimeState>>,
    receipt_state: State<'_, std::sync::Arc<crate::receipt_queue::ReceiptQueueState>>,
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
            .args([
                "advfirewall",
                "firewall",
                "show",
                "rule",
                "name=Zelara Device Linking",
            ])
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
    let latest_image_processing = state.latest_image_processing.clone();
    let latest_finance_context = state.latest_finance_context.clone();
    let progress_tx = ai_state.progress_tx.clone();
    let ai_arc = std::sync::Arc::clone(&*ai_state);
    let receipt_arc = std::sync::Arc::clone(&*receipt_state);

    // Spawn server task
    tokio::spawn(async move {
        if let Err(e) = run_websocket_server(
            &addr,
            tls_acceptor,
            server_running,
            pairing_token,
            linked_devices,
            client_senders,
            latest_image_processing,
            latest_finance_context,
            app_handle,
            progress_tx,
            ai_arc,
            receipt_arc,
        )
        .await
        {
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
    latest_image_processing: LatestImageProcessing,
    latest_finance_context: LatestFinanceContext,
    app_handle: tauri::AppHandle,
    progress_tx: tokio::sync::broadcast::Sender<crate::ai::DownloadProgressEvent>,
    ai_state: std::sync::Arc<crate::ai::AiRuntimeState>,
    receipt_state: std::sync::Arc<crate::receipt_queue::ReceiptQueueState>,
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
                let latest_processing = latest_image_processing.clone();
                let latest_finance_context = latest_finance_context.clone();
                let conn_progress_tx = progress_tx.clone();
                let conn_ai = std::sync::Arc::clone(&ai_state);
                let conn_receipt = std::sync::Arc::clone(&receipt_state);

                tokio::spawn(async move {
                    // Wrap TCP stream with TLS
                    match acceptor.accept(stream).await {
                        Ok(tls_stream) => match accept_async(tls_stream).await {
                            Ok(ws_stream) => {
                                if let Err(e) = handle_websocket_connection(
                                    ws_stream,
                                    token,
                                    devices,
                                    senders,
                                    latest_processing,
                                    latest_finance_context,
                                    remote_addr,
                                    handle,
                                    conn_progress_tx,
                                    conn_ai,
                                    conn_receipt,
                                )
                                .await
                                {
                                    eprintln!("WebSocket connection error: {}", e);
                                }
                            }
                            Err(e) => {
                                eprintln!("WebSocket handshake error: {}", e);
                            }
                        },
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
fn broadcast_progress_sync(
    client_senders: &ClientSenders,
    progress: &crate::storage::UserProgress,
) {
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

const IMAGE_PROCESSING_EFFECT_NAME: &str = "Spectral Edge Remix";

fn image_processing_message(snapshot: &ImageProcessingState) -> Option<Message> {
    let mut payload = serde_json::to_value(snapshot).ok()?;
    payload.as_object_mut()?.insert(
        "type".to_string(),
        serde_json::Value::String("image_processing_sync".to_string()),
    );
    serde_json::to_string(&payload).ok().map(Message::Text)
}

fn publish_image_processing_state(
    latest_image_processing: &LatestImageProcessing,
    client_senders: &ClientSenders,
    app_handle: &tauri::AppHandle,
    snapshot: ImageProcessingState,
) {
    *latest_image_processing.lock().unwrap() = Some(snapshot.clone());
    let _ = app_handle.emit("image-processing-state", &snapshot);

    if let Some(msg) = image_processing_message(&snapshot) {
        let senders = client_senders.lock().unwrap();
        for tx in senders.values() {
            let _ = tx.send(msg.clone());
        }
    }
}

fn rebroadcast_latest_image_processing_state(
    latest_image_processing: &LatestImageProcessing,
    client_senders: &ClientSenders,
) {
    let snapshot = latest_image_processing.lock().unwrap().clone();
    if let Some(snapshot) = snapshot {
        if let Some(msg) = image_processing_message(&snapshot) {
            let senders = client_senders.lock().unwrap();
            for tx in senders.values() {
                let _ = tx.send(msg.clone());
            }
        }
    }
}

fn finance_context_request_message() -> Option<Message> {
    serde_json::to_string(&serde_json::json!({
        "type": "finance_context_request",
    }))
    .ok()
    .map(Message::Text)
}

fn publish_finance_context_snapshot(
    latest_finance_context: &LatestFinanceContext,
    app_handle: &tauri::AppHandle,
    snapshot: FinanceContextSnapshot,
) {
    *latest_finance_context.lock().unwrap() = Some(snapshot.clone());
    let _ = app_handle.emit("finance-context-updated", &snapshot);
}

fn receipt_queue_update_message(job: &crate::receipt_queue::ReceiptJob) -> Option<Message> {
    serde_json::to_string(&serde_json::json!({
        "type": "receipt_queue_update",
        "job": job,
    }))
    .ok()
    .map(Message::Text)
}

fn receipt_queue_snapshot_message(jobs: &[crate::receipt_queue::ReceiptJob]) -> Option<Message> {
    serde_json::to_string(&serde_json::json!({
        "type": "receipt_queue_snapshot",
        "jobs": jobs,
    }))
    .ok()
    .map(Message::Text)
}

fn publish_receipt_job_update(
    receipt_state: &crate::receipt_queue::ReceiptQueueState,
    client_senders: &ClientSenders,
    app_handle: &tauri::AppHandle,
    job: &crate::receipt_queue::ReceiptJob,
) {
    let _ = app_handle.emit("receipt-job-updated", job);
    let _ = app_handle.emit("receipt-jobs-updated", receipt_state.list_jobs());

    if let Some(message) = receipt_queue_update_message(job) {
        let senders = client_senders.lock().unwrap();
        for sender in senders.values() {
            let _ = sender.send(message.clone());
        }
    }
}

async fn handle_websocket_connection<S>(
    ws_stream: tokio_tungstenite::WebSocketStream<S>,
    expected_token: Option<String>,
    linked_devices: Arc<Mutex<Vec<DeviceInfo>>>,
    client_senders: ClientSenders,
    latest_image_processing: LatestImageProcessing,
    latest_finance_context: LatestFinanceContext,
    remote_addr: String,
    app_handle: tauri::AppHandle,
    progress_tx: tokio::sync::broadcast::Sender<crate::ai::DownloadProgressEvent>,
    ai_state: std::sync::Arc<crate::ai::AiRuntimeState>,
    receipt_state: std::sync::Arc<crate::receipt_queue::ReceiptQueueState>,
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
    // Keep a clone for use in spawned tasks (ai_task async dispatch result).
    let push_tx_self = push_tx.clone();
    {
        client_senders
            .lock()
            .unwrap()
            .insert(remote_addr.clone(), push_tx);
    }

    // Subscribe to the AI model download progress broadcast.
    // Each connected mobile client gets its own receiver so all get the same events.
    let mut progress_rx = progress_tx.subscribe();

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
                                Some("img_preview") => {
                                    if !device_registered {
                                        eprintln!("[BinaryXfer] img_preview received before handshake completion");
                                        continue 'outer;
                                    }

                                    let task_id = ctrl["taskId"].as_str().unwrap_or("").to_string();
                                    let original_base64 = ctrl["originalBase64"].as_str().unwrap_or("").to_string();
                                    let mime = ctrl["mime"].as_str().unwrap_or("image/jpeg").to_string();
                                    if task_id.is_empty() || original_base64.is_empty() {
                                        eprintln!("[BinaryXfer] img_preview missing taskId or originalBase64");
                                        continue 'outer;
                                    }

                                    publish_image_processing_state(
                                        &latest_image_processing,
                                        &client_senders,
                                        &app_handle,
                                        ImageProcessingState {
                                            task_id,
                                            status: "captured".to_string(),
                                            message: "Photo captured on mobile. Waiting for desktop processing.".to_string(),
                                            device: remote_addr.clone(),
                                            effect_name: IMAGE_PROCESSING_EFFECT_NAME.to_string(),
                                            progress: 10,
                                            updated_at: chrono::Utc::now().to_rfc3339(),
                                            original_image: Some(original_base64),
                                            original_mime: mime,
                                            processed_image: None,
                                            processed_mime: None,
                                        },
                                    );
                                    continue 'outer;
                                }
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
                                            let original_base64 = general_purpose::STANDARD.encode(&original);
                                            let original_mime = transfer.mime.clone();
                                            publish_image_processing_state(
                                                &latest_image_processing,
                                                &client_senders,
                                                &app_handle,
                                                ImageProcessingState {
                                                    task_id: task_id.clone(),
                                                    status: "processing".to_string(),
                                                    message: "Zelara Hub is building the spectral edge remix.".to_string(),
                                                    device: remote_addr.clone(),
                                                    effect_name: IMAGE_PROCESSING_EFFECT_NAME.to_string(),
                                                    progress: 55,
                                                    updated_at: chrono::Utc::now().to_rfc3339(),
                                                    original_image: Some(original_base64.clone()),
                                                    original_mime: original_mime.clone(),
                                                    processed_image: None,
                                                    processed_mime: None,
                                                },
                                            );
                                            let tid = task_id.clone();
                                            let tx = inversion_tx.clone();
                                            let addr = remote_addr.clone();
                                            let original_base64_for_result = original_base64.clone();
                                            let original_base64_for_error = original_base64.clone();
                                            let original_mime_for_result = original_mime.clone();
                                            let original_mime_for_error = original_mime.clone();
                                            tokio::task::spawn_blocking(move || {
                                                match render_spectral_edge_remix(assembled) {
                                                    Ok(png) => { let _ = tx.blocking_send(InversionOutcome::Success { task_id: tid, png_bytes: png, original_bytes: original, original_base64: original_base64_for_result, original_mime: original_mime_for_result, addr }); }
                                                    Err(e) => { let _ = tx.blocking_send(InversionOutcome::Failure { task_id: tid, error: e, original_base64: Some(original_base64_for_error), original_mime: Some(original_mime_for_error), addr }); }
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

                            {
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
                            drop(devices);
                            }
                            registered_device_id = Some(stable_id);
                            device_registered = true;

                            // Push current Desktop progress immediately so Mobile starts in sync.
                            if let Ok(current_progress) = crate::storage::load_progress() {
                                broadcast_progress_sync(&client_senders, &current_progress);
                            }
                            rebroadcast_latest_image_processing_state(
                                &latest_image_processing,
                                &client_senders,
                            );
                            if let Some(snapshot_msg) =
                                receipt_queue_snapshot_message(&receipt_state.list_jobs())
                            {
                                let _ = write.send(snapshot_msg).await;
                            }
                            if let Some(finance_context_request) = finance_context_request_message()
                            {
                                let _ = write.send(finance_context_request).await;
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
                            "finance_context_sync" => {
                                let categories = request
                                    .payload
                                    .get("categories")
                                    .cloned()
                                    .unwrap_or_else(|| serde_json::json!([]));
                                let history = request
                                    .payload
                                    .get("history")
                                    .cloned()
                                    .unwrap_or_else(|| serde_json::json!([]));
                                let synced_at = request
                                    .payload
                                    .get("syncedAt")
                                    .and_then(|value| value.as_str())
                                    .unwrap_or_else(|| request.timestamp.as_str())
                                    .to_string();

                                match (
                                    serde_json::from_value::<Vec<FinanceContextCategory>>(categories),
                                    serde_json::from_value::<Vec<FinanceContextHistoryEntry>>(history),
                                ) {
                                    (Ok(categories), Ok(history)) => {
                                        let snapshot = FinanceContextSnapshot {
                                            categories,
                                            history,
                                            synced_at,
                                        };
                                        publish_finance_context_snapshot(
                                            &latest_finance_context,
                                            &app_handle,
                                            snapshot,
                                        );
                                        TaskResponse {
                                            task_id: request.task_id,
                                            success: true,
                                            result: serde_json::json!({ "message": "Finance context synced" }),
                                            timestamp: chrono::Utc::now().to_rfc3339(),
                                        }
                                    }
                                    (Err(error), _) | (_, Err(error)) => TaskResponse {
                                        task_id: request.task_id,
                                        success: false,
                                        result: serde_json::json!({ "error": format!("Invalid finance context payload: {error}") }),
                                        timestamp: chrono::Utc::now().to_rfc3339(),
                                    },
                                }
                            }
                            "receipt_capture" => {
                                let receipt_id = request
                                    .payload
                                    .get("receiptId")
                                    .and_then(|value| value.as_str())
                                    .unwrap_or(&request.task_id)
                                    .to_string();
                                let device_id = request
                                    .payload
                                    .get("deviceId")
                                    .and_then(|value| value.as_str())
                                    .unwrap_or("unknown-device")
                                    .to_string();
                                let captured_at = request
                                    .payload
                                    .get("capturedAt")
                                    .and_then(|value| value.as_str())
                                    .unwrap_or_else(|| request.timestamp.as_str())
                                    .to_string();
                                let mime_type = request
                                    .payload
                                    .get("mimeType")
                                    .and_then(|value| value.as_str())
                                    .unwrap_or("image/jpeg")
                                    .to_string();
                                let image_base64 = request
                                    .payload
                                    .get("imageBase64")
                                    .and_then(|value| value.as_str())
                                    .unwrap_or("")
                                    .to_string();

                                let capture_quality: crate::receipt_queue::ReceiptCaptureQuality =
                                    serde_json::from_value(
                                        request
                                            .payload
                                            .get("captureQuality")
                                            .cloned()
                                            .unwrap_or_else(|| serde_json::json!({})),
                                    )
                                    .unwrap_or_default();

                                match receipt_state.save_uploaded_job(
                                    receipt_id.clone(),
                                    device_id,
                                    captured_at,
                                    mime_type,
                                    image_base64,
                                    capture_quality,
                                ) {
                                    Ok(job) => {
                                        publish_receipt_job_update(
                                            &*receipt_state,
                                            &client_senders,
                                            &app_handle,
                                            &job,
                                        );
                                        start_receipt_processing_loop(
                                            std::sync::Arc::clone(&receipt_state),
                                            std::sync::Arc::clone(&ai_state),
                                            client_senders.clone(),
                                            app_handle.clone(),
                                        );
                                        TaskResponse {
                                            task_id: request.task_id,
                                            success: true,
                                            result: serde_json::json!({
                                                "message": "Receipt queued",
                                                "receiptId": receipt_id,
                                                "status": job.status,
                                            }),
                                            timestamp: chrono::Utc::now().to_rfc3339(),
                                        }
                                    }
                                    Err(error) => TaskResponse {
                                        task_id: request.task_id,
                                        success: false,
                                        result: serde_json::json!({ "error": error }),
                                        timestamp: chrono::Utc::now().to_rfc3339(),
                                    },
                                }
                            }
                            "ai_task" => {
                                let ai_request = crate::ai::AiTaskRequest {
                                    task_id: request.task_id.clone(),
                                    capability: request.capability.clone().unwrap_or_default(),
                                    payload: request.payload.clone(),
                                };
                                // Dispatch is async (may download models); run in a spawned task
                                // so the message loop is not blocked. Result is sent back over
                                // this connection's push channel (Arm 3 writes it to the socket).
                                let self_push = push_tx_self.clone();
                                let handle = app_handle.clone();
                                let dispatch_ai = std::sync::Arc::clone(&ai_state);
                                tokio::spawn(async move {
                                    let ai_response = crate::ai::dispatcher::dispatch(
                                        ai_request,
                                        &*dispatch_ai,
                                        &handle,
                                    )
                                    .await;
                                    let result_msg = serde_json::json!({
                                        "task_id": ai_response.task_id,
                                        "type": "ai_task_result",
                                        "success": ai_response.success,
                                        "capability": ai_response.capability,
                                        "result": ai_response.result,
                                        "error": ai_response.error,
                                        "download_progress": ai_response.download_progress,
                                    });
                                    if let Ok(text) = serde_json::to_string(&result_msg) {
                                        let _ = self_push.send(Message::Text(text));
                                    }
                                });
                                // Immediate ack — real result arrives via push channel
                                TaskResponse {
                                    task_id: request.task_id,
                                    success: true,
                                    result: serde_json::json!({ "type": "ai_task_queued" }),
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
                    Some(Ok(_)) => {} // Ping, Pong — ignore
                }
            }

            // ── Arm 2: inversion task completed on blocking thread pool ────
            Some(outcome) = inversion_rx.recv() => {
                match outcome {
                    InversionOutcome::Success {
                        task_id,
                        png_bytes,
                        original_bytes,
                        original_base64,
                        original_mime,
                        addr,
                    } => {
                        send_binary_result(&mut write, &task_id, &png_bytes, &original_bytes, &app_handle, &addr).await;
                        publish_image_processing_state(
                            &latest_image_processing,
                            &client_senders,
                            &app_handle,
                            ImageProcessingState {
                                task_id,
                                status: "completed".to_string(),
                                message: "Desktop processing finished. Result synced back to mobile.".to_string(),
                                device: addr,
                                effect_name: IMAGE_PROCESSING_EFFECT_NAME.to_string(),
                                progress: 100,
                                updated_at: chrono::Utc::now().to_rfc3339(),
                                original_image: Some(original_base64),
                                original_mime,
                                processed_image: Some(general_purpose::STANDARD.encode(&png_bytes)),
                                processed_mime: Some("image/png".to_string()),
                            },
                        );
                    }
                    InversionOutcome::Failure {
                        task_id,
                        error,
                        original_base64,
                        original_mime,
                        addr,
                    } => {
                        let ctrl = serde_json::to_string(&serde_json::json!({
                            "type": "img_result_end",
                            "taskId": task_id.clone(),
                            "success": false,
                            "error": error.clone()
                        })).unwrap_or_default();
                        let _ = write.send(Message::Text(ctrl)).await;
                        publish_image_processing_state(
                            &latest_image_processing,
                            &client_senders,
                            &app_handle,
                            ImageProcessingState {
                                task_id,
                                status: "failed".to_string(),
                                message: error,
                                device: addr,
                                effect_name: IMAGE_PROCESSING_EFFECT_NAME.to_string(),
                                progress: 100,
                                updated_at: chrono::Utc::now().to_rfc3339(),
                                original_image: original_base64,
                                original_mime: original_mime.unwrap_or_else(|| "image/jpeg".to_string()),
                                processed_image: None,
                                processed_mime: None,
                            },
                        );
                    }
                }
            }

            // ── Arm 3: outbound push messages (progress_sync, ai_task_result, etc.) ─
            Some(msg) = push_rx.recv() => {
                if write.send(msg).await.is_err() {
                    break 'outer;
                }
            }

            // ── Arm 4: AI model download progress → push to this client ────
            Ok(evt) = progress_rx.recv() => {
                let msg_type = if evt.progress >= 1.0 {
                    "model_ready"
                } else if evt.progress < 0.0 {
                    "model_download_error"
                } else {
                    "model_download_progress"
                };
                let payload = serde_json::json!({
                    "type": msg_type,
                    "capability": evt.capability,
                    "model_id": evt.model_id,
                    "progress": if evt.progress < 0.0 { serde_json::Value::Null } else { serde_json::json!(evt.progress) },
                });
                if let Ok(text) = serde_json::to_string(&payload) {
                    if write.send(Message::Text(text)).await.is_err() {
                        break 'outer;
                    }
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
        let removed_method = devices
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.discovery_method.clone())
            .unwrap_or_else(|| "qr".to_string());
        devices.retain(|d| d.id != id);
        drop(devices);
        println!("Device disconnected and removed: {}", id);
        let _ = app_handle.emit(
            "device-disconnected",
            serde_json::json!({
                "id": id,
                "discovery_method": removed_method
            }),
        );
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
fn render_spectral_edge_remix(image_bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    use image::{DynamicImage, GrayImage, ImageFormat, Luma, Rgba, RgbaImage};

    let rgba = image::load_from_memory(&image_bytes)
        .map_err(|e| format!("Failed to load image: {}", e))?
        .to_rgba8();
    let (width, height) = rgba.dimensions();

    let grayscale = DynamicImage::ImageRgba8(rgba.clone())
        .grayscale()
        .to_luma8();
    let blurred = image::imageops::blur(&grayscale, 2.2);
    let mut edges = GrayImage::new(width, height);

    for y in 0..height {
        for x in 0..width {
            let x = x as i32;
            let y = y as i32;
            let gx = -sample_luma(&blurred, x - 1, y - 1) + sample_luma(&blurred, x + 1, y - 1)
                - 2.0 * sample_luma(&blurred, x - 1, y)
                + 2.0 * sample_luma(&blurred, x + 1, y)
                - sample_luma(&blurred, x - 1, y + 1)
                + sample_luma(&blurred, x + 1, y + 1);
            let gy = sample_luma(&blurred, x - 1, y - 1)
                + 2.0 * sample_luma(&blurred, x, y - 1)
                + sample_luma(&blurred, x + 1, y - 1)
                - sample_luma(&blurred, x - 1, y + 1)
                - 2.0 * sample_luma(&blurred, x, y + 1)
                - sample_luma(&blurred, x + 1, y + 1);
            let magnitude = (gx.mul_add(gx, gy * gy)).sqrt().min(255.0) as u8;
            edges.put_pixel(x as u32, y as u32, Luma([magnitude]));
        }
    }

    let center_x = (width.saturating_sub(1)) as f32 / 2.0;
    let center_y = (height.saturating_sub(1)) as f32 / 2.0;
    let max_distance = (center_x.mul_add(center_x, center_y * center_y))
        .sqrt()
        .max(1.0);
    let mut output = RgbaImage::new(width, height);

    for y in 0..height {
        for x in 0..width {
            let original = rgba.get_pixel(x, y).0;
            let tone = blurred.get_pixel(x, y)[0] as f32 / 255.0;
            let edge = edges.get_pixel(x, y)[0] as f32 / 255.0;
            let dx = x as f32 - center_x;
            let dy = y as f32 - center_y;
            let vignette =
                (1.0 - 0.28 * ((dx.mul_add(dx, dy * dy)).sqrt() / max_distance)).clamp(0.72, 1.0);
            let highlight = (1.0 - tone).powf(1.25);
            let shadow = tone.powf(1.1);

            let mut red = ((255 - original[2]) as f32 * 0.18
                + edge * 235.0
                + highlight * 75.0
                + shadow * 15.0)
                * vignette;
            let mut green =
                ((255 - original[1]) as f32 * 0.48 + edge * 170.0 + highlight * 105.0) * vignette;
            let mut blue =
                ((255 - original[0]) as f32 * 0.82 + edge * 255.0 + highlight * 145.0) * vignette;

            let warm_accent = original[0] as f32 / 255.0 * 45.0 * (1.0 - edge * 0.45);
            red += warm_accent;
            green += warm_accent * 0.18;
            blue += (1.0 - shadow) * 24.0;

            output.put_pixel(
                x,
                y,
                Rgba([
                    posterize(red, 6),
                    posterize(green, 6),
                    posterize(blue, 7),
                    original[3],
                ]),
            );
        }
    }

    let mut output_bytes = Vec::new();
    DynamicImage::ImageRgba8(output)
        .write_to(
            &mut std::io::Cursor::new(&mut output_bytes),
            ImageFormat::Png,
        )
        .map_err(|e| format!("Failed to encode PNG: {}", e))?;
    Ok(output_bytes)
}

fn sample_luma(image: &image::GrayImage, x: i32, y: i32) -> f32 {
    let clamped_x = x.clamp(0, image.width().saturating_sub(1) as i32) as u32;
    let clamped_y = y.clamp(0, image.height().saturating_sub(1) as i32) as u32;
    image.get_pixel(clamped_x, clamped_y)[0] as f32
}

fn posterize(value: f32, levels: u8) -> u8 {
    let levels = levels.max(2) as f32;
    let step = 255.0 / (levels - 1.0);
    ((value.clamp(0.0, 255.0) / step).round() * step).clamp(0.0, 255.0) as u8
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
        original_base64: String,
        original_mime: String,
        addr: String,
    },
    Failure {
        task_id: String,
        error: String,
        original_base64: Option<String>,
        original_mime: Option<String>,
        addr: String,
    },
}

/// Increment when the binary transfer protocol changes in a breaking way.
/// Must stay in sync with PROTOCOL_VERSION in DeviceLinkingService.ts.
const PROTOCOL_VERSION: u32 = 1;

const BINARY_CHUNK_SIZE: usize = 65536;

/// Send the processed PNG back to mobile as binary chunks, then emit the legacy
/// Tauri desktop UI event for the current TestingPanel consumer.
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
    }))
    .unwrap_or_default();
    if write.send(Message::Text(start)).await.is_err() {
        return;
    }

    // 2. binary chunks — [4 bytes uint32 BE: chunk_index][raw PNG bytes]
    for (i, chunk) in png_bytes.chunks(BINARY_CHUNK_SIZE).enumerate() {
        let mut frame = Vec::with_capacity(4 + chunk.len());
        frame.extend_from_slice(&(i as u32).to_be_bytes());
        frame.extend_from_slice(chunk);
        if write.send(Message::Binary(frame)).await.is_err() {
            return;
        }
    }

    // 3. result_end control frame
    let end = serde_json::to_string(&serde_json::json!({
        "type": "img_result_end",
        "taskId": task_id,
        "success": true
    }))
    .unwrap_or_default();
    if write.send(Message::Text(end)).await.is_err() {
        return;
    }

    // 4. Tauri event for Desktop UI — still base64 (internal IPC, not a WebSocket hop)
    let _ = app_handle.emit(
        "image-inversion-result",
        serde_json::json!({
            "original": general_purpose::STANDARD.encode(original_bytes),
            "inverted": general_purpose::STANDARD.encode(png_bytes),
            "device": remote_addr,
            "timestamp": chrono::Utc::now().to_rfc3339()
        }),
    );
}

fn start_receipt_processing_loop(
    receipt_state: std::sync::Arc<crate::receipt_queue::ReceiptQueueState>,
    ai_state: std::sync::Arc<crate::ai::AiRuntimeState>,
    client_senders: ClientSenders,
    app_handle: tauri::AppHandle,
) {
    {
        let mut processing = receipt_state.processing.lock().unwrap();
        if *processing {
            return;
        }
        *processing = true;
    }

    tauri::async_runtime::spawn(async move {
        'jobs: loop {
            // Stop the loop cleanly on app shutdown.
            if receipt_state.is_shutdown() {
                *receipt_state.processing.lock().unwrap() = false;
                break;
            }

            let Some(job) = receipt_state.next_pending_job() else {
                *receipt_state.processing.lock().unwrap() = false;
                break;
            };

            // Permanently fail jobs that have exceeded the retry cap.
            if job.retry_count >= crate::receipt_queue::MAX_JOB_RETRIES {
                eprintln!(
                    "[ReceiptQueue] Job {} exceeded max retries ({}); marking as permanently failed",
                    job.receipt_id,
                    crate::receipt_queue::MAX_JOB_RETRIES
                );
                let _ = receipt_state.update_job(&job.receipt_id, |current| {
                    current.status = "failed".to_string();
                    current.error = Some("max_retries_exceeded".to_string());
                });
                if let Ok(Some(snapshot)) = receipt_state.update_job(&job.receipt_id, |_| {}) {
                    publish_receipt_job_update(
                        &*receipt_state,
                        &client_senders,
                        &app_handle,
                        &snapshot,
                    );
                }
                continue 'jobs;
            }

            if let Ok(Some(snapshot)) = receipt_state.update_job(&job.receipt_id, |current| {
                current.status = "running".to_string();
                current.error = None;
            }) {
                publish_receipt_job_update(
                    &*receipt_state,
                    &client_senders,
                    &app_handle,
                    &snapshot,
                );
            }

            let queue_started = std::time::Instant::now();

            loop {
                // Honour shutdown between model-poll ticks.
                if receipt_state.is_shutdown() {
                    *receipt_state.processing.lock().unwrap() = false;
                    break 'jobs;
                }

                match crate::ai::model_manager::ensure_capability_ready(
                    "ocr_receipt",
                    &*ai_state,
                    &app_handle,
                )
                .await
                {
                    crate::ai::model_manager::CapabilityStatus::Ready => break,
                    crate::ai::model_manager::CapabilityStatus::Downloading(progress) => {
                        if let Ok(Some(snapshot)) =
                            receipt_state.update_job(&job.receipt_id, |current| {
                                current.status = "waiting_for_model".to_string();
                                current.stage_timings = serde_json::json!({
                                    "waitForModelMs": queue_started.elapsed().as_millis() as u64,
                                    "downloadProgress": progress,
                                });
                            })
                        {
                            publish_receipt_job_update(
                                &receipt_state,
                                &client_senders,
                                &app_handle,
                                &snapshot,
                            );
                        }
                        sleep(Duration::from_millis(700)).await;
                    }
                    crate::ai::model_manager::CapabilityStatus::Failed(message) => {
                        if let Ok(Some(snapshot)) =
                            receipt_state.update_job(&job.receipt_id, |current| {
                                current.status = "failed".to_string();
                                current.error = Some(message.clone());
                                current.retry_count += 1;
                            })
                        {
                            publish_receipt_job_update(
                                &receipt_state,
                                &client_senders,
                                &app_handle,
                                &snapshot,
                            );
                        }
                        continue 'jobs;
                    }
                }
            }

            let ai_request = crate::ai::AiTaskRequest {
                task_id: job.receipt_id.clone(),
                capability: "ocr_receipt".to_string(),
                payload: serde_json::json!({
                    "imagePath": job.image_path,
                    "mimeType": job.mime_type,
                    "receiptId": job.receipt_id,
                    "captureQuality": job.capture_quality,
                }),
            };

            let process_started = std::time::Instant::now();
            let response =
                crate::ai::dispatcher::dispatch(ai_request, &*ai_state, &app_handle).await;

            match response {
                crate::ai::AiTaskResponse {
                    success: true,
                    result: Some(result),
                    ..
                } => {
                    let status = result["status"]
                        .as_str()
                        .unwrap_or("needs_review")
                        .to_string();
                    let updated = receipt_state.update_job(&job.receipt_id, |current| {
                        current.status = status.clone();
                        current.error = None;
                        current.ocr_result = Some(result.clone());
                        current.review_reason = result["reviewReason"]
                            .as_str()
                            .map(|value| value.to_string());
                        current.review_fields = result["reviewFields"]
                            .as_array()
                            .map(|values| {
                                values
                                    .iter()
                                    .filter_map(|value| {
                                        value.as_str().map(|value| value.to_string())
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        current.readiness_score = result["readinessScore"].as_f64();
                        current.field_confidence = result.get("fieldConfidence").cloned();
                        current.field_evidence = result.get("fieldEvidence").cloned();
                        current.field_suggestions = result.get("fieldSuggestions").cloned();
                        current.processing_trace = Some(serde_json::json!({
                            "ocrTrace": result.get("ocrTrace").cloned(),
                            "qwenFallback": result.get("qwenFallback").cloned(),
                        }));
                        current.stage_timings = merge_stage_timings(
                            result.get("stageTimings").cloned(),
                            queue_started.elapsed().as_millis() as u64,
                            process_started.elapsed().as_millis() as u64,
                        );
                        current.draft = Some(crate::receipt_queue::ReceiptDraft {
                            amount: result["total"].as_f64().unwrap_or(0.0),
                            currency: "EUR".to_string(),
                            description: result["description"]
                                .as_str()
                                .or_else(|| result["merchant"].as_str())
                                .unwrap_or("Receipt")
                                .to_string(),
                            merchant: result["merchant"].as_str().map(|value| value.to_string()),
                            date: result["date"].as_str().map(|value| value.to_string()),
                            category_id: result["categoryId"]
                                .as_str()
                                .map(|value| value.to_string()),
                        });
                    });
                    if let Ok(Some(snapshot)) = updated {
                        publish_receipt_job_update(
                            &receipt_state,
                            &client_senders,
                            &app_handle,
                            &snapshot,
                        );
                    }

                    // Spawn background Qwen3 refinement if the heuristic result needs it.
                    // Runs after the job is already marked completed so the UX is instant.
                    if result
                        .get("qwenNeeded")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        let raw_text = result["rawText"].as_str().unwrap_or("").to_string();
                        let merchant_hint = result["merchant"].as_str().map(|s| s.to_string());
                        let date_hint = result["date"].as_str().map(|s| s.to_string());
                        let total_hint = result["total"].as_f64().filter(|&t| t != 0.0);
                        let confidence_hint = result["confidence"].as_f64().unwrap_or(0.0);
                        let used_total_fallback = result
                            .get("usedTotalFallback")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true);
                        let receipt_id_bg = job.receipt_id.clone();
                        let ai_state_bg = std::sync::Arc::clone(&ai_state);
                        let receipt_state_bg = std::sync::Arc::clone(&receipt_state);
                        let client_senders_bg = client_senders.clone();
                        let app_handle_bg = app_handle.clone();

                        tokio::spawn(async move {
                            println!(
                                "[QwenReceipt][{receipt_id_bg}] Background refinement started"
                            );
                            let input = crate::ai::qwen_receipt::QwenReceiptInput {
                                raw_text: &raw_text,
                                merchant: merchant_hint.as_deref(),
                                date: date_hint.as_deref(),
                                total: total_hint,
                                confidence: confidence_hint,
                                used_total_fallback,
                            };
                            match crate::ai::qwen_receipt::refine_receipt_text(
                                &*ai_state_bg,
                                &input,
                            )
                            .await
                            {
                                Ok(suggestion) => {
                                    // Prefer description as merchant when it's cleaner than the raw merchant field.
                                    // Qwen3 often puts the clean store name in `description` (e.g. "Dollar Tree")
                                    // even when `merchant` still carries the full OCR address line.
                                    let clean_merchant = suggestion
                                        .merchant
                                        .as_deref()
                                        .filter(|m| !m.is_empty())
                                        .or(suggestion
                                            .description
                                            .as_deref()
                                            .filter(|d| !d.is_empty()))
                                        .map(|s| s.to_string());
                                    println!(
                                        "[QwenReceipt][{receipt_id_bg}] Background refined => merchant={:?} total={:?}",
                                        clean_merchant, suggestion.total
                                    );
                                    let refined =
                                        receipt_state_bg.update_job(&receipt_id_bg, |current| {
                                            if let Some(ref mut draft) = current.draft {
                                                if let Some(ref m) = clean_merchant {
                                                    draft.merchant = Some(m.clone());
                                                    draft.description = format!("{m} receipt");
                                                }
                                                if let Some(t) = suggestion.total {
                                                    if t > 0.0 {
                                                        draft.amount = t;
                                                    }
                                                }
                                                if let Some(ref d) = suggestion.date {
                                                    if !d.is_empty() {
                                                        draft.date = Some(d.clone());
                                                    }
                                                }
                                            }
                                            if let Some(obj) = current.stage_timings.as_object_mut()
                                            {
                                                obj.insert(
                                                    "qwenRefinedAt".to_string(),
                                                    serde_json::Value::String(
                                                        chrono::Utc::now().to_rfc3339(),
                                                    ),
                                                );
                                            }
                                        });
                                    if let Ok(Some(refined_snapshot)) = refined {
                                        publish_receipt_job_update(
                                            &receipt_state_bg,
                                            &client_senders_bg,
                                            &app_handle_bg,
                                            &refined_snapshot,
                                        );
                                        let _ = app_handle_bg.emit(
                                            "receipt-refined",
                                            serde_json::json!({
                                                "receiptId": receipt_id_bg,
                                                "merchant": suggestion.merchant,
                                                "total": suggestion.total,
                                                "date": suggestion.date,
                                                "confidence": suggestion.confidence,
                                            }),
                                        );
                                    }
                                }
                                Err(e) => {
                                    eprintln!("[QwenReceipt][{receipt_id_bg}] Background refinement failed: {e}");
                                }
                            }
                        });
                    }
                }
                failed => {
                    let error = failed
                        .error
                        .unwrap_or_else(|| "receipt_processing_failed".to_string());
                    if let Ok(Some(snapshot)) =
                        receipt_state.update_job(&job.receipt_id, |current| {
                            current.status = "failed".to_string();
                            current.error = Some(error.clone());
                            current.retry_count += 1;
                            current.stage_timings = serde_json::json!({
                                "queuedMs": queue_started.elapsed().as_millis() as u64,
                                "processingMs": process_started.elapsed().as_millis() as u64,
                            });
                        })
                    {
                        publish_receipt_job_update(
                            &receipt_state,
                            &client_senders,
                            &app_handle,
                            &snapshot,
                        );
                    }
                }
            }
        }
    });
}

pub fn resume_receipt_processing_loop(
    receipt_state: std::sync::Arc<crate::receipt_queue::ReceiptQueueState>,
    ai_state: std::sync::Arc<crate::ai::AiRuntimeState>,
    client_senders: ClientSenders,
    app_handle: tauri::AppHandle,
) {
    start_receipt_processing_loop(receipt_state, ai_state, client_senders, app_handle);
}

fn merge_stage_timings(
    stage_timings: Option<serde_json::Value>,
    queued_ms: u64,
    processing_ms: u64,
) -> serde_json::Value {
    let mut map = stage_timings
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    map.insert("queuedMs".to_string(), serde_json::json!(queued_ms));
    map.insert("processingMs".to_string(), serde_json::json!(processing_ms));
    serde_json::Value::Object(map)
}

#[tauri::command]
pub fn get_latest_image_processing_state(
    state: State<DeviceLinkingState>,
) -> Option<ImageProcessingState> {
    state.latest_image_processing.lock().unwrap().clone()
}

#[tauri::command]
pub fn get_finance_context(state: State<DeviceLinkingState>) -> Option<FinanceContextSnapshot> {
    state.latest_finance_context.lock().unwrap().clone()
}

#[tauri::command]
pub fn request_finance_context_sync(state: State<DeviceLinkingState>) -> Result<usize, String> {
    let senders = state.client_senders.lock().unwrap();
    let count = senders.len();
    if count == 0 {
        return Ok(0);
    }

    if let Some(message) = finance_context_request_message() {
        for sender in senders.values() {
            let _ = sender.send(message.clone());
        }
    }

    Ok(count)
}

#[tauri::command]
pub fn send_receipt_job_to_mobile(
    receipt_id: String,
    amount: f64,
    description: String,
    merchant: Option<String>,
    date: Option<String>,
    category_id: Option<String>,
    receipt_state: State<'_, std::sync::Arc<crate::receipt_queue::ReceiptQueueState>>,
    device_state: State<'_, DeviceLinkingState>,
    app_handle: tauri::AppHandle,
) -> Result<usize, String> {
    let senders = device_state.client_senders.lock().unwrap();
    if senders.is_empty() {
        return Err("No mobile clients are currently connected.".to_string());
    }

    let transaction = crate::finance_import::ImportedTransaction {
        date: date.unwrap_or_else(|| chrono::Utc::now().date_naive().to_string()),
        description,
        amount,
        currency: "EUR".to_string(),
        merchant,
        source_format: "ocr".to_string(),
    };
    let transaction_date = transaction.date.clone();
    let transaction_description = transaction.description.clone();
    let transaction_currency = transaction.currency.clone();
    let transaction_merchant = transaction.merchant.clone();
    let category_for_payload = category_id.clone();
    let receipt_id_for_payload = receipt_id.clone();
    let payload = serde_json::json!({
        "type": "finance_sync_push",
        "transactions": [{
            "date": transaction_date,
            "description": transaction_description,
            "amount": transaction.amount,
            "currency": transaction_currency,
            "merchant": transaction_merchant,
            "source": "ocr",
            "categoryId": category_for_payload,
            "receiptId": receipt_id_for_payload,
        }],
    });
    let text = serde_json::to_string(&payload)
        .map_err(|error| format!("Failed to serialize receipt draft: {error}"))?;

    for sender in senders.values() {
        let _ = sender.send(Message::Text(text.clone().into()));
    }
    drop(senders);

    if let Ok(Some(snapshot)) = receipt_state.update_job(&receipt_id, |current| {
        if let Some(draft) = current.draft.as_mut() {
            draft.amount = amount;
            draft.description = transaction.description.clone();
            draft.merchant = transaction.merchant.clone();
            draft.date = Some(transaction.date.clone());
            draft.category_id = category_id.clone();
        }
        current.status = "saved".to_string();
        current.error = None;
    }) {
        publish_receipt_job_update(
            &receipt_state,
            &device_state.client_senders,
            &app_handle,
            &snapshot,
        );
    }

    Ok(device_state.client_senders.lock().unwrap().len())
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
