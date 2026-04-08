# Zelara Desktop App

Desktop application for Zelara built with Tauri (TypeScript + Rust).

## Overview

The desktop app serves as the processing hub in the Zelara ecosystem:
- **CV/ML Processing**: ONNX Runtime for recycling image validation
- **Device Linking Server**: QR code generation, pairing with mobile devices
- **Task Offloading**: Receives tasks from mobile, processes, returns results
- **Full Feature Access**: All modules available on capable hardware

## Tech Stack

- **Frontend**: React + TypeScript + Vite
- **Backend**: Rust + Tauri
- **ML/CV**: ONNX Runtime (Rust bindings)
- **Receipt cleanup fallback**: local Python helper with `Qwen/Qwen3-0.6B` for medium-tier OCR repair when rule parsing is uncertain
- **Shared Logic**: @zelara packages from core

## Receipt AI Fallback

When receipt OCR comes back noisy, Zelara Core now keeps the fast rule parser as the first pass and can invoke a local `Qwen3` cleanup helper on medium-tier machines to repair merchant/date/total fields before the mobile form opens.

Python helper dependencies:

```bash
python -m pip install -r scripts/requirements-qwen-receipt.txt
```

For detailed technical documentation, see [wiki](wikis/).
