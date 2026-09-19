import { cls } from "../../lib/cls";

export type StatusTone = "good" | "warning" | "serious" | "critical" | "neutral";

/**
 * Status colours are reserved and never reused as a series colour, and they
 * always ship with a label — the dot alone would make colour the only carrier
 * of meaning.
 */
export function Badge({
  tone = "neutral",
  children,
  title,
}: {
  tone?: StatusTone;
  children: React.ReactNode;
  title?: string;
}) {
  return (
    <span
      className={cls("badge", tone !== "neutral" && `badge--${tone}`)}
      title={title}
    >
      {tone !== "neutral" && <span className="badge__dot" aria-hidden="true" />}
      {children}
    </span>
  );
}

/** Lifecycle state → tone. Anything unrecognised stays neutral. */
export function agentStateTone(state: string): StatusTone {
  const s = state.toUpperCase();
  if (s === "DEAD") return "critical";
  if (s.includes("CRITICAL")) return "serious";
  if (s.includes("LOW") || s.includes("FUEL")) return "warning";
  if (s === "ALIVE") return "good";
  return "neutral";
}

/**
 * Trade status → tone, covering all eight values the CHECK constraint allows.
 * The old dashboard styled three and left the rest unmarked.
 */
export function tradeStatusTone(status: string): StatusTone {
  switch (status.toUpperCase()) {
    case "RESOLVED_WIN":
      return "good";
    case "RESOLVED_LOSS":
      return "critical";
    case "CANCELLED":
      return "serious";
    case "PENDING":
    case "PARTIAL":
      return "warning";
    case "OPEN":
    case "FILLED":
    case "CLOSED":
    default:
      return "neutral";
  }
}
