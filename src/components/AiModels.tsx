import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

type ModelInfo = {
  id: string;
  name: string;
  format: string;
  capability: string;
  role: string;
  size_mb: number;
  tier_min: string;
  downloaded: boolean;
  loaded: boolean;
  download_progress?: number | null;
};

type ModelProgressPayload = {
  model_id: string;
  capability: string;
  progress?: number;
};

export default function AiModels() {
  const [hardwareTier, setHardwareTier] = useState<string>('unknown');
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [busyModelId, setBusyModelId] = useState<string | null>(null);

  useEffect(() => {
    void refresh();

    const unlisteners: Array<() => void> = [];
    listen<ModelProgressPayload>('model-download-progress', event => {
      setModels(current =>
        current.map(model =>
          model.id === event.payload.model_id
            ? {
                ...model,
                download_progress: event.payload.progress ?? model.download_progress,
              }
            : model,
        ),
      );
    }).then(unlisten => unlisteners.push(unlisten));

    listen<ModelProgressPayload>('model-download-complete', event => {
      setBusyModelId(current =>
        current === event.payload.model_id ? null : current,
      );
      void refresh();
    }).then(unlisten => unlisteners.push(unlisten));

    listen<ModelProgressPayload>('model-download-error', event => {
      setBusyModelId(current =>
        current === event.payload.model_id ? null : current,
      );
      void refresh();
    }).then(unlisten => unlisteners.push(unlisten));

    return () => {
      unlisteners.forEach(unlisten => unlisten());
    };
  }, []);

  async function refresh() {
    setLoading(true);
    try {
      const [tier, modelList] = await Promise.all([
        invoke<string>('get_hardware_tier'),
        invoke<ModelInfo[]>('list_ai_models'),
      ]);
      setHardwareTier(tier);
      setModels(modelList);
    } finally {
      setLoading(false);
    }
  }

  async function handleDownload(modelId: string) {
    setBusyModelId(modelId);
    try {
      await invoke('download_model_cmd', { modelId });
      await refresh();
    } finally {
      setBusyModelId(current => (current === modelId ? null : current));
    }
  }

  return (
    <section className="ai-models-panel">
      <div className="ai-models-header">
        <div>
          <h2>AI Models</h2>
          <p>
            Zelara Core downloads ONNX and helper models on demand for finance
            OCR, categorization, and receipt cleanup tasks.
          </p>
        </div>
        <div className="ai-models-actions">
          <span className="ai-tier-pill">Tier: {hardwareTier}</span>
          <button onClick={() => void refresh()} type="button">
            Refresh
          </button>
        </div>
      </div>

      {loading ? (
        <p className="ai-models-empty">Loading model manifest…</p>
      ) : (
        <div className="ai-models-grid">
          {models.map(model => {
            const progress = model.download_progress ?? 0;
            const usesWarmSession = model.format.toLowerCase() === 'onnx';
            const completion = model.loaded
              ? 1
              : model.downloaded
                ? 1
                : progress;
            const status = usesWarmSession
              ? model.loaded
                ? 'Loaded'
                : model.downloaded
                  ? 'Downloaded'
                  : progress > 0 && progress < 1
                    ? `Downloading ${Math.round(progress * 100)}%`
                    : 'Not downloaded'
              : model.downloaded
                ? 'Downloaded'
                : progress > 0 && progress < 1
                  ? `Downloading ${Math.round(progress * 100)}%`
                  : 'Not downloaded';
            const buttonLabel = usesWarmSession
              ? model.loaded
                ? 'Ready'
                : busyModelId === model.id
                  ? 'Preparing…'
                  : model.downloaded
                    ? 'Reload'
                    : 'Download'
              : busyModelId === model.id
                ? 'Preparing…'
                : model.downloaded
                  ? 'Downloaded'
                  : 'Download';
            const disabled = busyModelId === model.id || (usesWarmSession ? model.loaded : model.downloaded);

            return (
              <article className="ai-model-card" key={model.id}>
                <div className="ai-model-card__top">
                  <div>
                    <h3>{model.name}</h3>
                    <p>
                      {model.capability} · {model.role} · {model.format.toUpperCase()}
                    </p>
                  </div>
                  <span className="ai-model-card__status">{status}</span>
                </div>

                <div className="ai-model-card__meta">
                  <span>{model.size_mb.toFixed(1)} MB</span>
                  <span>Tier {model.tier_min}</span>
                </div>

                <div className="ai-model-progress">
                  <div
                    className="ai-model-progress__bar"
                    style={{
                      width: `${Math.round(completion * 100)}%`,
                    }}
                  />
                </div>

                <div className="ai-model-card__actions">
                  <button
                    disabled={disabled}
                    onClick={() => void handleDownload(model.id)}
                    type="button"
                  >
                    {buttonLabel}
                  </button>
                </div>
              </article>
            );
          })}
        </div>
      )}
    </section>
  );
}
