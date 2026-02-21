import { UserProgress } from '@zelara/shared';

interface ProgressDisplayProps {
  progress: UserProgress;
}

function ProgressDisplay({ progress }: ProgressDisplayProps) {
  const nextUnlockThreshold = 50; // Finance module unlock threshold
  const progressPercentage = Math.min((progress.points / nextUnlockThreshold) * 100, 100);

  return (
    <div className="progress-display">
      <div className="points-counter">
        <span className="points-value">{progress.points}</span>
        <span className="points-label">points</span>
      </div>

      <div className="progress-bar-container">
        <div
          className="progress-bar"
          style={{ width: `${progressPercentage}%` }}
        />
        <span className="progress-label">
          {progress.points < nextUnlockThreshold
            ? `${nextUnlockThreshold - progress.points} points to unlock Finance`
            : 'Finance module available!'}
        </span>
      </div>

      {progress.availableUnlocks.length > 0 && (
        <div className="available-unlocks">
          <strong>Available to unlock:</strong>{' '}
          {progress.availableUnlocks.join(', ')}
        </div>
      )}
    </div>
  );
}

export default ProgressDisplay;
