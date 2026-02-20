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
- **Shared Logic**: @zelara packages from core

For detailed technical documentation, see [wiki](wikis/).