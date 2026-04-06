import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { UserProgress } from '@zelara/shared';
import './App.css';
import DevicePairing from './components/DevicePairing';
import ProgressDisplay from './components/ProgressDisplay';
import ModuleList from './components/ModuleList';
import TestingPanel from './components/TestingPanel';

type Tab = 'hub' | 'ai-models' | 'finance' | 'settings';

const TAB_LABELS: Record<Tab, string> = {
  'hub':       'Hub',
  'ai-models': 'AI Models',
  'finance':   'Finance',
  'settings':  'Settings',
};

const TABS: Tab[] = ['hub', 'ai-models', 'finance', 'settings'];

function App() {
  const [progress, setProgress] = useState<UserProgress | null>(null);
  const [loading, setLoading] = useState(true);
  const [toastMessage, setToastMessage] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<Tab>('hub');

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
        <div className="loading">Loading Zelara Core...</div>
      </div>
    );
  }

  return (
    <div className="app">
      <header className="app-header">
        <h1>Zelara Core</h1>
        {progress && <ProgressDisplay progress={progress} />}
      </header>

      <nav className="tab-nav">
        {TABS.map((tab) => (
          <button
            key={tab}
            className={`tab-btn${activeTab === tab ? ' tab-btn--active' : ''}`}
            onClick={() => setActiveTab(tab)}
          >
            {TAB_LABELS[tab]}
          </button>
        ))}
      </nav>

      <main className="app-main">
        {activeTab === 'hub' && (
          <>
            <section className="device-section">
              <h2>Device Linking</h2>
              <DevicePairing />
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
          </>
        )}

        {activeTab === 'ai-models' && (
          <section className="placeholder-section">
            <h2>AI Models</h2>
            <p>
              No AI models loaded. Zelara Core will support ONNX and GGUF models
              for on-device inference in a future release. Models will appear here
              once downloaded.
            </p>
          </section>
        )}

        {activeTab === 'finance' && (
          <section className="placeholder-section">
            <h2>Finance</h2>
            <p>
              Zelara Finance is a standalone app. Download it to manage your
              finances, track spending, and get AI-powered insights.
            </p>
          </section>
        )}

        {activeTab === 'settings' && (
          <section className="settings-section">
            <h2>Settings</h2>
            <details>
              <summary>Debug Tools</summary>
              <TestingPanel />
            </details>
          </section>
        )}
      </main>

      {toastMessage && (
        <div className="toast-notification">{toastMessage}</div>
      )}
    </div>
  );
}

export default App;
