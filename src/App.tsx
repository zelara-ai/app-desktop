import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { UserProgress } from '@zelara/shared';
import './App.css';
import DevicePairing from './components/DevicePairing';
import ProgressDisplay from './components/ProgressDisplay';
import ModuleList from './components/ModuleList';

function App() {
  const [progress, setProgress] = useState<UserProgress | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    loadProgress();
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
    </div>
  );
}

export default App;
