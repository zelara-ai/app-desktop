import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { UserProgress } from '@zelara/shared';
import './App.css';
import DevicePairing from './components/DevicePairing';
import ProgressDisplay from './components/ProgressDisplay';
import ModuleList from './components/ModuleList';
import TestingPanel from './components/TestingPanel';

function App() {
  const [progress, setProgress] = useState<UserProgress | null>(null);
  const [loading, setLoading] = useState(true);
  const [toastMessage, setToastMessage] = useState<string | null>(null);

  useEffect(() => {
    loadProgress();
    // Start the WSS/TLS pairing server at launch so BLE auto-discovery
    // connections succeed immediately (BLE advertises this address from startup).
    // The command is idempotent — safe to call again when QR code is generated.
    invoke('start_pairing_server').catch((e: any) =>
      console.error('[App] Failed to auto-start pairing server:', e),
    );
  }, []);

  useEffect(() => {
    const unlisteners: Array<() => void> = [];

    listen<{ id: string; name: string; platform: string }>('device-linked', (event) => {
      setToastMessage(`Device linked: ${event.payload.name}`);
      setTimeout(() => setToastMessage(null), 3000);
    }).then((fn) => unlisteners.push(fn));

    listen<UserProgress>('progress-updated', (event) => {
      setProgress(event.payload);
    }).then((fn) => unlisteners.push(fn));

    return () => {
      unlisteners.forEach((fn) => fn());
    };
  }, []);

  const loadProgress = async () => {
    try {
      const userProgress = await invoke<UserProgress>('load_progress');
      setProgress(userProgress);
    } catch (error) {
      console.error('Failed to load progress:', error);
    } finally {
      setLoading(false);
    }
  };

  const handleProgressUpdate = () => {
    loadProgress();
  };

  if (loading) {
    return (
      <div className="app">
        <div className="loading">Loading Zelara...</div>
      </div>
    );
  }

  return (
    <div className="app">
      <header className="app-header">
        <h1>Zelara Desktop</h1>
        {progress && <ProgressDisplay progress={progress} />}
      </header>

      <main className="app-main">
        <section className="device-section">
          <h2>Device Linking</h2>
          <DevicePairing />
        </section>

        <section className="testing-section">
          <TestingPanel />
        </section>

        <section className="modules-section">
          <h2>Modules</h2>
          {progress && (
            <ModuleList
              progress={progress}
              onProgressUpdate={handleProgressUpdate}
            />
          )}
        </section>
      </main>
      {toastMessage && (
        <div className="toast-notification">{toastMessage}</div>
      )}
    </div>
  );
}

export default App;
