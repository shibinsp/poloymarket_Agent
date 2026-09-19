import type { ReactNode } from "react";
import { failureText, type ApiFailure } from "../../api/client";

/**
 * Empty states are a pure function of the data, so a list that goes back to
 * empty renders the empty state again. The old dashboard appended rows to
 * `innerHTML` behind a `length > 0` guard, so once populated it could never
 * show "nothing here" again.
 */
export function EmptyState({
  title,
  hint,
  action,
}: {
  title: string;
  hint?: string;
  action?: ReactNode;
}) {
  return (
    <div className="state">
      <div className="state__title">{title}</div>
      {hint && <div className="state__hint">{hint}</div>}
      {action && <div style={{ marginTop: 10 }}>{action}</div>}
    </div>
  );
}

/**
 * A failure, named. The server's own message is shown verbatim — for the
 * HTTP-200-with-`{"error"}` case that string is the only diagnosis available,
 * and hiding it behind "something went wrong" throws it away.
 */
export function ErrorState({
  label,
  error,
  onRetry,
}: {
  label: string;
  error: ApiFailure;
  onRetry?: () => void;
}) {
  return (
    <div className="state state--error">
      <div className="state__title">{label} could not be loaded</div>
      <div className="state__msg">{failureText(error)}</div>
      {onRetry && (
        <div style={{ marginTop: 10 }}>
          <button className="btn btn--sm" onClick={onRetry}>
            Try again
          </button>
        </div>
      )}
    </div>
  );
}

export function Skeleton({ rows = 3 }: { rows?: number }) {
  return (
    <div
      aria-hidden="true"
      style={{ display: "flex", flexDirection: "column", gap: 8 }}
    >
      {Array.from({ length: rows }, (_, i) => (
        <div
          key={i}
          className="skeleton"
          style={{ width: `${100 - i * 12}%` }}
        />
      ))}
    </div>
  );
}
