import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
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

interface InversionResult {
  original: string;
  inverted: string;
  device: string;
  timestamp: string;
}

const TestingPanel: React.FC = () => {
  const [serverRunning] = useState(true); // Server is started when QR is generated
  const [connectedDevices, setConnectedDevices] = useState(0);
  const [testLog, setTestLog] = useState<string[]>([]);
  const [lastResult, setLastResult] = useState<InversionResult | null>(null);
  const [currentTime, setCurrentTime] = useState(new Date().toLocaleTimeString());
  const [counter, setCounter] = useState<number | null>(null);
  const [localIps, setLocalIps] = useState<string[]>([]);
  const [bleConnectedDevices, setBleConnectedDevices] = useState(0);
  const [bleStatus, setBleStatus] = useState<BleStatus>({ status: 'idle' });

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

    // Display image inversion results sent from mobile
    listen<InversionResult>('image-inversion-result', (event) => {
      setLastResult(event.payload);
      addLogEntry(`Image inversion test received from ${event.payload.device}`);
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

    return () => {
      clearInterval(clockInterval);
      unlisteners.forEach((fn) => fn());
    };
  }, []);

  const addLogEntry = (message: string) => {
    const timestamp = new Date().toLocaleTimeString();
    setTestLog(prev => [`[${timestamp}] ${message}`, ...prev].slice(0, 50));
  };

  const primaryIp = localIps[0] ?? '—';
  const isConnected = connectedDevices > 0;
  const bleConnected = bleConnectedDevices > 0;

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

      <div className="testing-section">
        <h3>Image Processing Tests</h3>
        <p className="section-description">
          Tests are initiated from mobile devices. Use the Testing screen on mobile to take a
          photo — it will be sent here, inverted, and the result will appear on both devices.
        </p>
      </div>

      {lastResult && (
        <div className="test-result-display">
          <h3>Last Inversion Test</h3>
          <p className="test-result-meta">
            From: {lastResult.device} &nbsp;&middot;&nbsp; {new Date(lastResult.timestamp).toLocaleTimeString()}
          </p>
          <div className="image-comparison">
            <div className="image-comparison-item">
              <p>Original</p>
              <img
                src={`data:image/jpeg;base64,${lastResult.original}`}
                alt="Original photo from mobile"
              />
            </div>
            <div className="image-comparison-item">
              <p>Inverted</p>
              <img
                src={`data:image/png;base64,${lastResult.inverted}`}
                alt="Inverted result"
              />
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
