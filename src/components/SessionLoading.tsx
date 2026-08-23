import type { SessionLoadProgress } from "../hooks/useSession";

function formatBytes(n: number): string {
  if (n >= 1024 * 1024 * 1024) return (n / (1024 * 1024 * 1024)).toFixed(1) + " GB";
  if (n >= 1024 * 1024) return (n / (1024 * 1024)).toFixed(1) + " MB";
  if (n >= 1024) return (n / 1024).toFixed(1) + " KB";
  return String(n) + " B";
}

export function SessionLoading({ progress }: { progress: SessionLoadProgress | null }) {
  const pct =
    progress && progress.total > 0
      ? Math.min(100, Math.round((progress.done / progress.total) * 100))
      : null;

  return (
    <div className="app__loading">
      <div className="app__loading-text">
        {progress && progress.total > 0
          ? `Loading session… ${formatBytes(progress.done)} / ${formatBytes(progress.total)}`
          : "Loading session…"}
      </div>
      {pct !== null && (
        <div className="app__loading-bar">
          <div className="app__loading-fill" style={{ width: `${pct}%` }} />
        </div>
      )}
    </div>
  );
}
