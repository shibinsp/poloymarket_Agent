/**
 * Timestamps arrive in two formats and one of them is a trap.
 *
 * `/api/health` is serialized by chrono and carries a zone:
 *     "2026-09-19T10:28:10.506085Z"
 *
 * Trades, cycles and costs come from SQLite columns defaulted to
 * `datetime('now')` (migrations 001/002), which stores UTC but writes it with
 * a space and *no zone*:
 *     "2026-09-19 09:53:16"
 *
 * `new Date("2026-09-19 09:53:16")` is parsed as **local time** by V8 and
 * JSC. Measured on a UTC+5:30 host that is 330 minutes of drift — and it is
 * silent: the timestamp still renders, it is just wrong. Every timestamp in
 * this app goes through `parseTs`.
 */

/** True when a string already carries a zone designator. */
function hasZone(s: string): boolean {
  return /[zZ]$|[+-]\d{2}:?\d{2}$/.test(s);
}

/**
 * Parse either timestamp format into a `Date`, or `null` if it is unusable.
 * A bare SQLite stamp is treated as UTC, which is what SQLite wrote.
 */
export function parseTs(v: string | null | undefined): Date | null {
  if (!v) return null;
  const s = v.trim();
  if (s === "") return null;

  const looksBare =
    /^\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}/.test(s) && !hasZone(s);
  const iso = looksBare ? s.replace(" ", "T") + "Z" : s;

  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? null : d;
}

/**
 * Start of the current UTC day. The agent's `get_today_api_cost` filters on
 * SQLite `date('now')`, which is UTC — so "spend today" has to mean the same
 * day the server means, not the viewer's midnight.
 */
export function todayUtcStart(now: Date = new Date()): Date {
  return new Date(
    Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate()),
  );
}

/** The UTC calendar day of a timestamp, as `YYYY-MM-DD`, for grouping. */
export function utcDayKey(d: Date): string {
  return d.toISOString().slice(0, 10);
}
