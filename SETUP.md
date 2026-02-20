# App-Desktop Setup Instructions

This file contains instructions for setting up the Tauri desktop app scaffold.

## Status

**Repository initialized** - Tauri scaffold pending

## Next Steps

1. **Initialize Tauri app**:
   ```bash
   cd apps/desktop
   npm create tauri-app@latest . --yes --template react-ts
   ```

2. **Install dependencies**:
   ```bash
   npm install
   ```

3. **Link to core packages**:
   ```bash
   npm install ../../src/packages/shared
   npm install ../../src/packages/skill-tree
   npm install ../../src/packages/state
   npm install ../../src/packages/device-linking
   ```

4. **Add Rust dependencies** (edit `src-tauri/Cargo.toml`):
   ```toml
   [dependencies]
   tauri = { version = "1.5", features = ["shell-open"] }
   serde = { version = "1.0", features = ["derive"] }
   serde_json = "1.0"
   ort = "1.16"  # ONNX Runtime
   tokio = { version = "1", features = ["full"] }
   ```

5. **Test development build**:
   ```bash
   npm run tauri dev
   ```

## Why Not Auto-Generated?

Tauri CLI requires interactive prompts. This should be run manually by developer or in next session.

## Architecture

Once scaffolded:
- **Frontend**: React + TypeScript + Vite
- **Backend**: Rust + Tauri
- **Bridge**: Tauri commands for frontend <-> Rust communication
- **ML**: ONNX Runtime for image validation
- **Storage**: Filesystem-based (JSON + SQLite)
