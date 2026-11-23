import { m } from "@featherpost/i18n/messages";
import "./sync-count.css";

export function SyncCount({ count, busy, className = "" }: {
  count: number;
  busy: boolean;
  className?: string;
}) {
  return (
    <small className={`sync-count ${className}`}>
      <span className={busy ? "sync-count-hidden" : undefined}>{count || ""}</span>
      {busy && (
        <svg
          className="icon sync-count-spinner"
          viewBox="0 0 24 24"
          role="img"
          aria-label={m.studio_sync_status_syncing()}
        >
          <path d="M20 7v5h-5m5 0A8 8 0 1 0 17.2 18" />
        </svg>
      )}
    </small>
  );
}
