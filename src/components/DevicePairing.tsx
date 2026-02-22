import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface PairingInfo {
  qr_data: string;
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

      // TODO: Generate actual QR code image from qr_data
      console.log('QR Data:', info.qr_data);
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
          <div className="qr-placeholder">
            {/* TODO: Render actual QR code */}
            <p>QR Code: {pairingInfo.qr_data}</p>
            <p>IP: {pairingInfo.ip_address}:{pairingInfo.port}</p>
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
