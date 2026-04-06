import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { UserProgress } from '@zelara/shared';
import './TestingPanel.css';

interface DeviceInfo {
  id: string;
  name: string;
  platform: string;
  discovery_method?: string; // "ble" | "qr"
}

type BleStatus =
  | { status: 'notSupported' }
  | { status: 'idle' }
  | { status: 'advertising'; ip: string; port: number }
  | { status: 'error'; message: string };

interface ImageProcessingState {
  taskId: string;
  status: 'captured' | 'processing' | 'completed' | 'failed';
  message: string;
  device: string;
  effectName: string;
  progress: number;
  updatedAt: string;
  originalImage: string | null;
  originalMime: string;
  processedImage: string | null;
  processedMime: string | null;
}

const TestingPanel: React.FC = () => {
  const [serverRunning] = useState(true); // Server is started when QR is generated
  const [connectedDevices, setConnectedDevices] = useState(0);
  const [testLog, setTestLog] = useState<string[]>([]);
  const [latestImageTask, setLatestImageTask] = useState<ImageProcessingState | null>(null);
  const [currentTime, setCurrentTime] = useState(new Date().toLocaleTimeString());
  const [counter, setCounter] = useState<number | null>(null);
  const [localIps, setLocalIps] = useState<string[]>([]);
  const [bleConnectedDevices, setBleConnectedDevices] = useState(0);
  const [bleStatus, setBleStatus] = useState<BleStatus>({ status: 'idle' });
  const [currentPoints, setCurrentPoints] = useState<number | null>(null);
  const [awardingPoints, setAwardingPoints] = useState(false);

  useEffect(() => {
    // Load current device count from backend
    invoke<DeviceInfo[]>('get_linked_devices')
      .then((devices) => setConnectedDevices(devices.length))
      .catch(() => {});

    // Load local IPs for BLE discovery display
    invoke<string[]>('get_local_ips')
      .then(setLocalIps)
      .catch(() => {});

    // Load current BLE advertising status
    invoke<BleStatus>('get_ble_status')
      .then(setBleStatus)
      .catch(() => {});

    invoke<ImageProcessingState | null>('get_latest_image_processing_state')
      .then((task) => {
        if (task) {
          setLatestImageTask(task);
        }
      })
      .catch(() => {});

    // Clock — updates every second
    const clockInterval = setInterval(() => {
      setCurrentTime(new Date().toLocaleTimeString());
    }, 1000);

    const unlisteners: Array<() => void> = [];

    // Update device count when a new device links
    listen<DeviceInfo>('device-linked', (event) => {
      setConnectedDevices((prev) => prev + 1);
      if (event.payload.discovery_method === 'ble') {
        setBleConnectedDevices((prev) => prev + 1);
      }
      const method = event.payload.discovery_method === 'ble' ? ' (BLE)' : ' (QR)';
      addLogEntry(`Device linked: ${event.payload.name}${method}`);
    }).then((fn) => unlisteners.push(fn));

    // Update device count when a device disconnects
    listen<{ id: string; discovery_method?: string }>('device-disconnected', (event) => {
      setConnectedDevices((prev) => Math.max(0, prev - 1));
      if (event.payload.discovery_method === 'ble') {
        setBleConnectedDevices((prev) => Math.max(0, prev - 1));
      }
      addLogEntry(`Device disconnected: ${event.payload.id}`);
    }).then((fn) => unlisteners.push(fn));

    listen<ImageProcessingState>('image-processing-state', (event) => {
      setLatestImageTask(event.payload);
      const prefix =
        event.payload.status === 'captured'
          ? 'Photo preview received'
          : event.payload.status === 'processing'
          ? 'Desktop processing started'
          : event.payload.status === 'completed'
          ? 'Desktop processing finished'
          : 'Desktop processing failed';
      addLogEntry(`${prefix}: ${event.payload.device}`);
    }).then((fn) => unlisteners.push(fn));

    // Counter sent from mobile every second
    listen<{ value: number }>('counter-update', (event) => {
      setCounter(event.payload.value);
    }).then((fn) => unlisteners.push(fn));

    // BLE advertising status changes (e.g., started, stopped, error)
    listen<BleStatus>('ble-status-changed', (event) => {
      setBleStatus(event.payload);
      const label =
        event.payload.status === 'advertising' ? 'BLE advertising active' :
        event.payload.status === 'idle' ? 'BLE advertising stopped' :
        event.payload.status === 'error' ? `BLE error: ${(event.payload as any).message}` :
        'BLE not supported';
      addLogEntry(label);
    }).then((fn) => unlisteners.push(fn));

    // Load current points for debug display
    invoke<{ points: number }>('load_progress')
      .then((p) => setCurrentPoints(p.points))
      .catch(() => {});

    // Keep point display in sync with progress updates
    listen<{ points: number }>('progress-updated', (event) => {
      setCurrentPoints(event.payload.points);
    }).then((fn) => unlisteners.push(fn));

    return () => {
      clearInterval(clockInterval);
      unlisteners.forEach((fn) => fn());
    };
  }, []);

  const addLogEntry = (message: string) => {
    const timestamp = new Date().toLocaleTimeString();
    setTestLog(prev => [`[${timestamp}] ${message}`, ...prev].slice(0, 50));
  };

  const awardDebugPoints = async (amount: number) => {
    setAwardingPoints(true);
    try {
      const updated = await invoke<{ points: number }>('award_points', { pointsToAdd: amount });
      setCurrentPoints(updated.points);
      addLogEntry(`Awarded ${amount} pts → total: ${updated.points}`);
    } catch (err: any) {
      addLogEntry(`Award failed: ${err}`);
    } finally {
      setAwardingPoints(false);
    }
  };

  const resetDebugPoints = async () => {
    setAwardingPoints(true);
    try {
      const updated = await invoke<UserProgress>('reset_points');
      setCurrentPoints(updated.points);
      addLogEntry(`Reset points → total: ${updated.points}`);
    } catch (err: any) {
      addLogEntry(`Reset failed: ${err}`);
    } finally {
      setAwardingPoints(false);
    }
  };

  const primaryIp = localIps[0] ?? '—';
  const isConnected = connectedDevices > 0;
  const bleConnected = bleConnectedDevices > 0;
  const imageTaskStatusLabel =
    latestImageTask?.status === 'captured'
      ? 'Preview Ready'
      : latestImageTask?.status === 'processing'
      ? 'Processing'
      : latestImageTask?.status === 'completed'
      ? 'Synced'
      : latestImageTask?.status === 'failed'
      ? 'Failed'
      : null;

  return (
    <div className="testing-panel">
      <div className="testing-header">
        <h2>Testing Module</h2>
        <p className="testing-subtitle">Diagnostic tools for device communication</p>
      </div>

      <div className="testing-section testing-section--row">
        <div className="testing-live-item">
          <h3>Desktop Clock</h3>
          <div className="clock-display">{currentTime}</div>
        </div>
        <div className="testing-live-item">
          <h3>Mobile Counter</h3>
          <div className="counter-display">
            {counter !== null ? counter : '—'}
          </div>
          <p className="section-description">
            Auto-sent from mobile every second while connected
          </p>
        </div>
      </div>

      <div className="testing-section">
        <h3>WebSocket Server Status</h3>
        <div className="status-row">
          <div className={`status-indicator ${serverRunning ? 'running' : 'stopped'}`} />
          <span className="status-text">
            {serverRunning ? 'WSS running on port 8765' : 'Stopped'}
          </span>
        </div>
        <div className="status-row">
          <div className={`status-indicator ${isConnected ? 'connected' : 'disconnected'}`} />
          <span className="status-text">
            {connectedDevices} connected device(s)
          </span>
        </div>
      </div>

      {/* ── BLE Auto-Discovery ── */}
      <div className="testing-section">
        <h3>BLE Auto-Discovery</h3>
        <div className="status-row">
          <div className={`status-indicator ${
            bleStatus.status === 'advertising' ? 'running' :
            bleStatus.status === 'notSupported' ? 'stopped' :
            bleStatus.status === 'error' ? 'stopped' :
            'disconnected'
          }`} />
          <span className="status-text">
            {bleStatus.status === 'advertising'
              ? `Advertising on ${(bleStatus as any).ip}:${(bleStatus as any).port}`
              : bleStatus.status === 'notSupported'
              ? 'BLE not available on this machine'
              : bleStatus.status === 'error'
              ? `BLE error: ${(bleStatus as any).message}`
              : 'BLE advertising idle'}
          </span>
        </div>
        <p className="section-description">
          Desktop advertises its IP over Bluetooth so Mobile can connect without scanning a QR code.
          The animation shows live BLE-linked devices. QR pairing is always available as a fallback.
        </p>

        <div className="bt-track">
          <div className="bt-node">
            <span className="bt-icon">💻</span>
            <span className="bt-label">Desktop</span>
            <span className="bt-ip">{primaryIp}</span>
          </div>

          <div className="bt-lane">
            <div className="bt-wire" />
            {bleConnected && (
              <div className="bt-ball">
                <span>{primaryIp}</span>
                <span>:8765</span>
              </div>
            )}
          </div>

          <div className="bt-node">
            <span className="bt-icon">📱</span>
            <span className="bt-label">Mobile</span>
            <span className={bleConnected ? 'bt-status--connected' : 'bt-status--waiting'}>
              {bleConnected ? 'Connected via BLE' : 'Waiting…'}
            </span>
          </div>
        </div>

        {!bleConnected && (
          <p className="section-description bt-hint">
            Mobile will auto-connect when Bluetooth is active. Use Device Pairing for QR fallback.
          </p>
        )}
        {localIps.length > 1 && (
          <p className="section-description">
            All interfaces being advertised: <code>{localIps.join(', ')}</code>
          </p>
        )}
      </div>

      {/* ── Debug: Progress Controls ── */}
      <div className="testing-section">
        <h3>Debug: Progress Controls</h3>
        <p className="section-description">
          Award points directly — auto-syncs to all connected Mobile devices via WebSocket.
          Replaces the force-stop + SQLite workflow for dev testing.
        </p>
        <p className="section-description">
          Current points: <strong>{currentPoints !== null ? currentPoints : '…'}</strong>
        </p>
        <div className="debug-button-row">
          <button
            className="debug-btn"
            onClick={() => awardDebugPoints(10)}
            disabled={awardingPoints}>
            +10 pts
          </button>
          <button
            className="debug-btn"
            onClick={() => awardDebugPoints(50)}
            disabled={awardingPoints}>
            +50 pts
          </button>
          <button
            className="debug-btn debug-btn--danger"
            onClick={() => awardDebugPoints(9999)}
            disabled={awardingPoints}>
            +9999 pts (unlock all)
          </button>
          <button
            className="debug-btn debug-btn--danger"
            onClick={resetDebugPoints}
            disabled={awardingPoints}>
            Reset to 0
          </button>
        </div>
      </div>

      <div className="testing-section">
        <h3>Image Processing Tests</h3>
        <p className="section-description">
          Tests are initiated from mobile devices. Use the Testing screen on mobile to take a
          photo — the preview appears here immediately, then Zelara Hub runs the{' '}
          <strong>{latestImageTask?.effectName ?? 'Spectral Edge Remix'}</strong> and syncs the
          result back to mobile.
        </p>
      </div>

      {latestImageTask && (
        <div className="test-result-display">
          <div className="test-result-header">
            <div>
              <h3>Latest Image Processing Job</h3>
              <p className="test-result-meta">
                From: {latestImageTask.device} &nbsp;&middot;&nbsp; {new Date(latestImageTask.updatedAt).toLocaleTimeString()}
              </p>
            </div>
            {imageTaskStatusLabel && (
              <span className={`image-task-badge image-task-badge--${latestImageTask.status}`}>
                {imageTaskStatusLabel}
              </span>
            )}
          </div>
          <p className="test-result-meta">
            {latestImageTask.message} &nbsp;&middot;&nbsp; {latestImageTask.progress}% complete
          </p>
          <div className="image-comparison">
            <div className="image-comparison-item">
              <p>Captured on Mobile</p>
              {latestImageTask.originalImage ? (
                <img
                  src={`data:${latestImageTask.originalMime};base64,${latestImageTask.originalImage}`}
                  alt="Original photo from mobile"
                />
              ) : (
                <div className="image-placeholder">Waiting for preview...</div>
              )}
            </div>
            <div className="image-comparison-item">
              <p>{latestImageTask.effectName}</p>
              {latestImageTask.processedImage ? (
                <img
                  src={`data:${latestImageTask.processedMime ?? 'image/png'};base64,${latestImageTask.processedImage}`}
                  alt="Desktop-processed result"
                />
              ) : (
                <div className="image-placeholder image-placeholder--processing">
                  {latestImageTask.status === 'failed' ? 'Processing failed' : 'Desktop processing...'}
                </div>
              )}
            </div>
          </div>
        </div>
      )}

      <div className="testing-section">
        <h3>Test Log</h3>
        <div className="test-log">
          {testLog.length === 0 ? (
            <div className="log-empty">No test activity yet</div>
          ) : (
            testLog.map((entry, index) => (
              <div key={index} className="log-entry">{entry}</div>
            ))
          )}
        </div>
      </div>

      <div className="testing-info">
        <h4>About Testing Module</h4>
        <p>
          This module provides diagnostic tools for validating device-to-device communication
          and image processing pipelines. Use these features during development to ensure
          proper functionality across platforms.
        </p>
      </div>
    </div>
  );
};

export default TestingPanel;
