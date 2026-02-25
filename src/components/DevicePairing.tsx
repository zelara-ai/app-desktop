import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface PairingInfo {
  qr_data: string;
  qr_image: string; // Base64-encoded PNG
  ip_address: string;
  port: number;
  token: string;
}

interface DeviceInfo {
  id: string;
  name: string;
  platform: string;
}

function DevicePairing() {
  const [pairingInfo, setPairingInfo] = useState<PairingInfo | null>(null);
  const [linkedDevices, setLinkedDevices] = useState<DeviceInfo[]>([]);
  const [loading, setLoading] = useState(false);

  const generateQR = async () => {
    setLoading(true);
    try {
      // Start pairing server
      await invoke('start_pairing_server');

      // Generate QR code with pairing info
      const info = await invoke<PairingInfo>('generate_qr_code');
      setPairingInfo(info);

      console.log('QR Code generated');
      console.log('Server listening on:', `${info.ip_address}:${info.port}`);
    } catch (error) {
      console.error('Failed to generate QR code:', error);
    } finally {
      setLoading(false);
    }
  };

  const loadLinkedDevices = async () => {
    try {
      const devices = await invoke<DeviceInfo[]>('get_linked_devices');
      setLinkedDevices(devices);
    } catch (error) {
      console.error('Failed to load linked devices:', error);
    }
  };

  return (
    <div className="device-pairing">
      <div className="pairing-controls">
        <button onClick={generateQR} disabled={loading}>
          {loading ? 'Generating...' : 'Generate QR Code'}
        </button>
        <button onClick={loadLinkedDevices}>Refresh Devices</button>
      </div>

      {pairingInfo && (
        <div className="qr-display">
          <h3>Scan this QR code with your mobile device</h3>
          <div className="qr-code">
            <img
              src={`data:image/png;base64,${pairingInfo.qr_image}`}
              alt="QR Code for device pairing"
              style={{ width: '400px', height: '400px', border: '2px solid #ccc', borderRadius: '8px' }}
            />
          </div>
          <div className="pairing-details">
            <p><strong>Server:</strong> {pairingInfo.ip_address}:{pairingInfo.port}</p>
            <p style={{ fontSize: '12px', color: '#666' }}>Open Zelara mobile app and scan this code to connect</p>
          </div>
        </div>
      )}

      <div className="linked-devices">
        <h3>Linked Devices ({linkedDevices.length})</h3>
        {linkedDevices.length === 0 ? (
          <p>No devices linked yet</p>
        ) : (
          <ul>
            {linkedDevices.map((device) => (
              <li key={device.id}>
                {device.name} ({device.platform})
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

export default DevicePairing;
