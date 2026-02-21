import { UserProgress, MODULES } from '@zelara/shared';

interface ModuleListProps {
  progress: UserProgress;
  onProgressUpdate: () => void;
}

function ModuleList({ progress }: ModuleListProps) {
  const allModules = [
    { id: 'green', name: 'Green', description: 'Recycling tasks and carbon footprint tracking' },
    { id: 'finance', name: 'Finance', description: 'Personal finance organization and donation suggestions' },
  ];

  return (
    <div className="module-list">
      {allModules.map((module) => {
        const isUnlocked = progress.unlockedModules.includes(module.id);
        const isAvailable = progress.availableUnlocks.includes(module.id);

        return (
          <div
            key={module.id}
            className={`module-card ${isUnlocked ? 'unlocked' : 'locked'} ${isAvailable ? 'available' : ''}`}
          >
            <h3>{module.name}</h3>
            <p>{module.description}</p>
            <div className="module-status">
              {isUnlocked && <span className="status-badge unlocked">Unlocked</span>}
              {!isUnlocked && isAvailable && (
                <button className="unlock-button">Unlock Now</button>
              )}
              {!isUnlocked && !isAvailable && (
                <span className="status-badge locked">Locked</span>
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}

export default ModuleList;
