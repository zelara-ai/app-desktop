import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import './TestingPanel.css';

interface DeviceInfo {
  id: string;
  name: string;
  platform: string;
}

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

  useEffect(() => {
    // Load current device count from backend
    invoke<DeviceInfo[]>('get_linked_devices')
      .then((devices) => setConnectedDevices(devices.length))
      .catch(() => {});

    // Load local IPs for BLE discovery display
    invoke<string[]>('get_local_ips')
      .then(setLocalIps)
      .catch(() => {});

    // Clock — updates every second
    const clockInterval = setInterval(() => {
      setCurrentTime(new Date().toLocaleTimeString());
    }, 1000);

    const unlisteners: Array<() => void> = [];

    // Update device count when a new device links
    listen<DeviceInfo>('device-linked', (event) => {
      setConnectedDevices((prev) => prev + 1);
      addLogEntry(`Device linked: ${event.payload.name}`);
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

      {/* ── Bluetooth Discovery Test ── */}
      <div className="testing-section">
        <h3>Bluetooth Discovery Test</h3>
        <p className="section-description">
          Simulates what BLE will broadcast in Phase 3 — Desktop advertises its IP so
          Mobile can auto-connect without scanning the QR code. The animation shows the
          IP packet flying over the Bluetooth channel.
        </p>

        <div className="bt-track">
          <div className="bt-node">
            <span className="bt-icon">💻</span>
            <span className="bt-label">Desktop</span>
            <span className="bt-ip">{primaryIp}</span>
          </div>

          <div className="bt-lane">
            <div className="bt-wire" />
            {isConnected && (
              <div className="bt-ball">
                <span>{primaryIp}</span>
                <span>:8765</span>
              </div>
            )}
          </div>

          <div className="bt-node">
            <span className="bt-icon">📱</span>
            <span className="bt-label">Mobile</span>
            <span className={isConnected ? 'bt-status--connected' : 'bt-status--waiting'}>
              {isConnected ? 'Connected' : 'Waiting…'}
            </span>
          </div>
        </div>

        {!isConnected && (
          <p className="section-description bt-hint">
            Connect a mobile device via the Device Pairing screen to see the animation.
          </p>
        )}
        {localIps.length > 1 && (
          <p className="section-description">
            All interfaces BLE would advertise: <code>{localIps.join(', ')}</code>
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
