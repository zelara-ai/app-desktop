import React, { useState, useEffect } from 'react';
import './TestingPanel.css';

interface ConnectionInfo {
  isRunning: boolean;
  connectedDevices: number;
  port: number;
}

const TestingPanel: React.FC = () => {
  const [connectionInfo, setConnectionInfo] = useState<ConnectionInfo>({
    isRunning: false,
    connectedDevices: 0,
    port: 8765,
  });
  const [testLog, setTestLog] = useState<string[]>([]);

  useEffect(() => {
    // In a real implementation, this would query the Tauri backend
    // For now, we'll simulate the connection status
    const checkConnection = () => {
      // This would call a Tauri command to get actual status
      setConnectionInfo({
        isRunning: true,
        connectedDevices: 0, // Would be populated by backend
        port: 8765,
      });
    };

    checkConnection();
    const interval = setInterval(checkConnection, 5000);

    return () => clearInterval(interval);
  }, []);

  const addLogEntry = (message: string) => {
    const timestamp = new Date().toLocaleTimeString();
    setTestLog(prev => [`[${timestamp}] ${message}`, ...prev].slice(0, 50));
  };

  const simulateTest = () => {
    addLogEntry('Simulated test initiated - this would test device communication in production');
  };

  return (
    <div className="testing-panel">
      <div className="testing-header">
        <h2>Testing Module</h2>
        <p className="testing-subtitle">Diagnostic tools for device communication</p>
      </div>

      <div className="testing-section">
        <h3>WebSocket Server Status</h3>
        <div className="status-row">
          <div className={`status-indicator ${connectionInfo.isRunning ? 'running' : 'stopped'}`} />
          <span className="status-text">
            {connectionInfo.isRunning ? `Running on port ${connectionInfo.port}` : 'Stopped'}
          </span>
        </div>
        <div className="status-row">
          <div className={`status-indicator ${connectionInfo.connectedDevices > 0 ? 'connected' : 'disconnected'}`} />
          <span className="status-text">
            {connectionInfo.connectedDevices} connected device(s)
          </span>
        </div>
      </div>

      <div className="testing-section">
        <h3>Image Processing Tests</h3>
        <p className="section-description">
          Tests are initiated from mobile devices. When a mobile device sends an image inversion
          request, it will be processed by this Desktop application and the result sent back.
        </p>
        <button
          className="test-button secondary"
          onClick={simulateTest}>
          Simulate Test (Dev Only)
        </button>
      </div>

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
