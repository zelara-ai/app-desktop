use base64::{engine::general_purpose, Engine as _};
use calamine::{open_workbook_auto, Reader};
use csv::ReaderBuilder;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use tauri::State;
use tokio_tungstenite::tungstenite::Message;

use crate::ai::{AiRuntimeState, AiTaskError, AiTaskRequest};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ImportedTransaction {
    pub date: String,
    pub description: String,
    pub amount: f64,
    pub currency: String,
    pub merchant: Option<String>,
    pub source_format: String,
}

#[tauri::command]
pub fn import_finance_file(
    path: String,
    _state: State<'_, AiRuntimeState>,
) -> Result<Vec<ImportedTransaction>, String> {
    import_finance_file_from_path(Path::new(&path))
}

#[tauri::command]
pub fn push_finance_transactions(
    transactions: Vec<ImportedTransaction>,
    device_state: State<'_, crate::device_linking::DeviceLinkingState>,
) -> Result<usize, String> {
    if transactions.is_empty() {
        return Ok(0);
    }

    let payload = serde_json::json!({
        "type": "finance_sync_push",
        "transactions": transactions,
    });
    let text = serde_json::to_string(&payload)
        .map_err(|error| format!("Failed to serialize finance sync payload: {error}"))?;

    let senders = device_state.client_senders.lock().unwrap();
    for sender in senders.values() {
        let _ = sender.send(Message::Text(text.clone().into()));
    }

    Ok(senders.len())
}

pub fn handle_ai_task(request: &AiTaskRequest) -> Result<serde_json::Value, AiTaskError> {
    let file_base64 = request
        .payload
        .get("fileBase64")
        .and_then(|value| value.as_str())
        .ok_or_else(|| AiTaskError::ProcessingFailed("Missing fileBase64 payload".to_string()))?;
    let format = request
        .payload
        .get("format")
        .and_then(|value| value.as_str())
        .unwrap_or("csv");

    let bytes = general_purpose::STANDARD
        .decode(file_base64)
        .map_err(|error| {
            AiTaskError::ProcessingFailed(format!("Invalid base64 payload: {error}"))
        })?;

    let temp_dir = std::env::temp_dir().join("zelara-finance-import");
    fs::create_dir_all(&temp_dir)
        .map_err(|error| AiTaskError::ProcessingFailed(format!("Temp dir error: {error}")))?;

    let path = temp_dir.join(format!(
        "import_{}.{}",
        chrono::Utc::now().timestamp_millis(),
        sanitize_extension(format)
    ));

    fs::write(&path, bytes).map_err(|error| {
        AiTaskError::ProcessingFailed(format!("Write temp file failed: {error}"))
    })?;

    let result = import_finance_file_from_path(&path).map_err(AiTaskError::ProcessingFailed)?;

    let _ = fs::remove_file(&path);

    serde_json::to_value(result).map_err(|error| {
        AiTaskError::ProcessingFailed(format!("Serialize import result failed: {error}"))
    })
}

fn import_finance_file_from_path(path: &Path) -> Result<Vec<ImportedTransaction>, String> {
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "csv" => parse_csv(path),
        "ofx" | "qfx" => parse_ofx(path),
        "xlsx" | "xls" => parse_xlsx(path),
        "pdf" => Err("PDF import is not available yet on desktop".to_string()),
        _ => Err(format!("Unsupported file format: .{ext}")),
    }
}

fn parse_csv(path: &Path) -> Result<Vec<ImportedTransaction>, String> {
    let content = fs::read_to_string(path)
        .or_else(|_| fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
        .map_err(|error| format!("Could not read CSV: {error}"))?;

    let delimiter = detect_delimiter(&content);
    let mut reader = ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(content.as_bytes());

    let headers = reader
        .headers()
        .map_err(|error| format!("Could not read CSV header: {error}"))?
        .iter()
        .map(normalize_header)
        .collect::<Vec<_>>();

    let date_index = find_header_index(&headers, &["date", "booked date", "transaction date"]);
    let description_index = find_header_index(
        &headers,
        &["description", "memo", "name", "details", "purpose", "payee"],
    );
    let amount_index = find_header_index(&headers, &["amount", "value", "transaction amount"]);
    let debit_index = find_header_index(&headers, &["debit", "withdrawal", "outflow"]);
    let credit_index = find_header_index(&headers, &["credit", "deposit", "inflow"]);
    let currency_index = find_header_index(&headers, &["currency", "curr"]);
    let merchant_index = find_header_index(&headers, &["merchant", "name", "payee"]);

    let mut transactions = Vec::new();
    for record in reader.records() {
        let row = record.map_err(|error| format!("Invalid CSV record: {error}"))?;
        let description = get_cell(&row, description_index)
            .or_else(|| get_cell(&row, merchant_index))
            .unwrap_or_default();

        if description.trim().is_empty() {
            continue;
        }

        let amount = if let Some(index) = amount_index {
            parse_amount(&row.get(index).unwrap_or_default())
        } else {
            let debit = debit_index
                .and_then(|index| row.get(index))
                .map(parse_amount)
                .unwrap_or(0.0);
            let credit = credit_index
                .and_then(|index| row.get(index))
                .map(parse_amount)
                .unwrap_or(0.0);
            credit - debit.abs()
        };

        transactions.push(ImportedTransaction {
            date: normalize_date(
                date_index
                    .and_then(|index| row.get(index))
                    .unwrap_or_default(),
            ),
            description: description.trim().to_string(),
            amount,
            currency: currency_index
                .and_then(|index| row.get(index))
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "EUR".to_string()),
            merchant: merchant_index
                .and_then(|index| row.get(index))
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            source_format: "csv".to_string(),
        });
    }

    Ok(transactions)
}

fn parse_ofx(path: &Path) -> Result<Vec<ImportedTransaction>, String> {
    let content = fs::read_to_string(path)
        .or_else(|_| fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
        .map_err(|error| format!("Could not read OFX: {error}"))?;

    let mut transactions = Vec::new();
    let mut cursor = content.as_str();

    while let Some(start) = cursor.find("<STMTTRN>") {
        let after_start = &cursor[start + "<STMTTRN>".len()..];
        let Some(end) = after_start.find("</STMTTRN>") else {
            break;
        };
        let block = &after_start[..end];
        cursor = &after_start[end + "</STMTTRN>".len()..];

        let description = extract_ofx_field(block, "NAME")
            .or_else(|| extract_ofx_field(block, "MEMO"))
            .unwrap_or_else(|| "Imported transaction".to_string());
        let merchant = extract_ofx_field(block, "NAME");
        let date = normalize_date(&extract_ofx_field(block, "DTPOSTED").unwrap_or_default());
        let amount = extract_ofx_field(block, "TRNAMT")
            .map(|value| parse_amount(&value))
            .unwrap_or(0.0);

        transactions.push(ImportedTransaction {
            date,
            description,
            amount,
            currency: extract_ofx_field(block, "CURDEF").unwrap_or_else(|| "EUR".to_string()),
            merchant,
            source_format: "ofx".to_string(),
        });
    }

    Ok(transactions)
}

fn parse_xlsx(path: &Path) -> Result<Vec<ImportedTransaction>, String> {
    let mut workbook =
        open_workbook_auto(path).map_err(|error| format!("Could not open spreadsheet: {error}"))?;
    let sheet_name = workbook
        .sheet_names()
        .first()
        .cloned()
        .ok_or_else(|| "Spreadsheet has no worksheets".to_string())?;

    let range = workbook
        .worksheet_range(&sheet_name)
        .map_err(|error| format!("Could not read worksheet '{sheet_name}': {error}"))?;

    let rows = range.rows().collect::<Vec<_>>();
    let header_row_index = rows
        .iter()
        .enumerate()
        .find_map(|(index, row)| {
            let normalized = row
                .iter()
                .map(|cell| normalize_header(&cell.to_string()))
                .collect::<Vec<_>>();
            let has_date = find_header_index(&normalized, &["date", "booked date"]).is_some();
            let has_description =
                find_header_index(&normalized, &["description", "memo", "name", "details"])
                    .is_some();
            if has_date && has_description {
                Some(index)
            } else {
                None
            }
        })
        .unwrap_or(0);

    let headers = rows
        .get(header_row_index)
        .ok_or_else(|| "Spreadsheet header row missing".to_string())?
        .iter()
        .map(|cell| normalize_header(&cell.to_string()))
        .collect::<Vec<_>>();

    let date_index = find_header_index(&headers, &["date", "booked date", "transaction date"]);
    let description_index = find_header_index(
        &headers,
        &["description", "memo", "name", "details", "purpose", "payee"],
    );
    let amount_index = find_header_index(&headers, &["amount", "value", "transaction amount"]);
    let debit_index = find_header_index(&headers, &["debit", "withdrawal", "outflow"]);
    let credit_index = find_header_index(&headers, &["credit", "deposit", "inflow"]);
    let currency_index = find_header_index(&headers, &["currency", "curr"]);
    let merchant_index = find_header_index(&headers, &["merchant", "name", "payee"]);

    let mut transactions = Vec::new();
    for row in rows.into_iter().skip(header_row_index + 1) {
        let description = description_index
            .and_then(|index| row.get(index))
            .map(|cell| cell.to_string())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_default();

        if description.trim().is_empty() {
            continue;
        }

        let amount = if let Some(index) = amount_index {
            row.get(index)
                .map(|cell| parse_amount(&cell.to_string()))
                .unwrap_or(0.0)
        } else {
            let debit = debit_index
                .and_then(|index| row.get(index))
                .map(|cell| parse_amount(&cell.to_string()))
                .unwrap_or(0.0);
            let credit = credit_index
                .and_then(|index| row.get(index))
                .map(|cell| parse_amount(&cell.to_string()))
                .unwrap_or(0.0);
            credit - debit.abs()
        };

        transactions.push(ImportedTransaction {
            date: normalize_date(
                &date_index
                    .and_then(|index| row.get(index))
                    .map(|cell| cell.to_string())
                    .unwrap_or_default(),
            ),
            description: description.trim().to_string(),
            amount,
            currency: currency_index
                .and_then(|index| row.get(index))
                .map(|cell| cell.to_string())
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "EUR".to_string()),
            merchant: merchant_index
                .and_then(|index| row.get(index))
                .map(|cell| cell.to_string())
                .filter(|value| !value.trim().is_empty()),
            source_format: "xlsx".to_string(),
        });
    }

    Ok(transactions)
}

fn detect_delimiter(content: &str) -> u8 {
    let header = content.lines().next().unwrap_or_default();
    let comma = header.matches(',').count();
    let semicolon = header.matches(';').count();
    let tab = header.matches('\t').count();

    if semicolon > comma && semicolon >= tab {
        b';'
    } else if tab > comma {
        b'\t'
    } else {
        b','
    }
}

fn normalize_header(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .replace('_', " ")
        .replace('-', " ")
}

fn find_header_index(headers: &[String], candidates: &[&str]) -> Option<usize> {
    headers.iter().position(|header| {
        candidates
            .iter()
            .any(|candidate| header == candidate || header.contains(candidate))
    })
}

fn get_cell(record: &csv::StringRecord, index: Option<usize>) -> Option<String> {
    index
        .and_then(|column| record.get(column))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_amount(value: &str) -> f64 {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return 0.0;
    }

    let normalized = trimmed
        .replace('€', "")
        .replace('$', "")
        .replace('£', "")
        .replace(' ', "");

    let normalized = if normalized.contains(',') && normalized.contains('.') {
        normalized.replace('.', "").replace(',', ".")
    } else if normalized.contains(',') {
        normalized.replace(',', ".")
    } else {
        normalized
    };

    normalized.parse::<f64>().unwrap_or(0.0)
}

fn normalize_date(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return chrono::Utc::now().date_naive().to_string();
    }

    let head = trimmed
        .split(|ch| ch == 'T' || ch == ' ')
        .next()
        .unwrap_or(trimmed)
        .replace('.', "-")
        .replace('/', "-");

    if head.len() >= 8 {
        return head;
    }

    chrono::Utc::now().date_naive().to_string()
}

fn extract_ofx_field(block: &str, tag: &str) -> Option<String> {
    let open_tag = format!("<{tag}>");
    let start = block.find(&open_tag)?;
    let value_start = start + open_tag.len();
    let rest = &block[value_start..];
    let value = rest
        .lines()
        .next()
        .unwrap_or(rest)
        .trim()
        .trim_end_matches('\r');

    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn sanitize_extension(format: &str) -> String {
    let cleaned = format
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>();
    if cleaned.is_empty() {
        "csv".to_string()
    } else {
        cleaned.to_lowercase()
    }
}
