import { relTime, absTime } from "../../lib/format";
import { failureText, type ApiFailure } from "../../api/client";
import { cls } from "../../lib/cls";

/**
 * How old the number above this line is.
 *
 * The rule this enforces: the page never shows a figure without saying how
 * fresh it is. The failure mode being designed out is the old dashboard's —
 * a dead backend left last hour's numbers on screen under a "Updated
 * 14:32:01" stamp that kept ticking off the *client* clock, so a broken page
 * and a quiet agent looked identical.
 */
export function Freshness({
  lastSuccessAt,
  now,
  stale,
  veryStale,
  error,
  paused,
}: {
  lastSuccessAt: number | null;
  now: number;
  stale?: boolean;
  veryStale?: boolean;
  error?: ApiFailure | null;
  paused?: boolean;
}) {
  const when = lastSuccessAt === null ? null : new Date(lastSuccessAt);
  const rel = relTime(when, now);

  let tone = "ok";
  let text = `updated ${rel}`;

  if (error) {
    tone = "failed";
    text =
      lastSuccessAt === null
        ? "never loaded"
        : `failed · last updated ${rel}`;
  } else if (paused) {
    tone = "paused";
    text = lastSuccessAt === null ? "paused" : `paused · updated ${rel}`;
  } else if (veryStale) {
    tone = "v-stale";
    text = `stale · updated ${rel}`;
  } else if (stale) {
    tone = "stale";
    text = `updated ${rel}`;
  }

  const title = [
    when ? `Last successful load: ${absTime(when)}` : "Never loaded",
    error ? failureText(error) : null,
  ]
    .filter(Boolean)
    .join("\n");

  return (
    <span className={cls("fresh", `fresh--${tone}`)} title={title}>
      <span className="fresh__dot" aria-hidden="true" />
      {text}
    </span>
  );
}
