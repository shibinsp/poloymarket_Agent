/**
 * Display formatting.
 *
 * Every formatter takes `number | null` and renders an em dash for null, so
 * "we have no figure" never renders as a confident zero. `Intl` formatters are
 * built once at module scope — constructing one per cell is measurable on a
 * few hundred table rows.
 */
import { denormZero } from "./decimal";

export const DASH = "—";
/** U+2212. Wider than a hyphen, so signed columns line up. */
const MINUS = "−";

const money2 = new Intl.NumberFormat("en-US", {
  style: "currency",
  currency: "USD",
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
});
const money4 = new Intl.NumberFormat("en-US", {
  style: "currency",
  currency: "USD",
  minimumFractionDigits: 2,
  maximumFractionDigits: 4,
});
const intFmt = new Intl.NumberFormat("en-US", { maximumFractionDigits: 0 });
const compactFmt = new Intl.NumberFormat("en-US", {
  notation: "compact",
  maximumFractionDigits: 1,
});

export function money(v: number | null | undefined): string {
  return v === null || v === undefined ? DASH : money2.format(denormZero(v));
}

/**
 * Prices on a prediction market live in the third and fourth decimal — a
 * contract at 0.0325 is a real position. Rounding those to cents erases the
 * difference between two very different trades.
 */
export function price(v: number | null | undefined): string {
  return v === null || v === undefined ? DASH : money4.format(denormZero(v));
}

/** Signed money with a true minus sign, for P&L columns. */
export function moneySigned(v: number | null | undefined): string {
  if (v === null || v === undefined) return DASH;
  const n = denormZero(v);
  const body = money2.format(Math.abs(n));
  if (n > 0) return "+" + body;
  if (n < 0) return MINUS + body;
  return body;
}

export function int(v: number | null | undefined): string {
  return v === null || v === undefined ? DASH : intFmt.format(v);
}

export function compact(v: number | null | undefined): string {
  return v === null || v === undefined ? DASH : compactFmt.format(v);
}

/**
 * The only percent helper, and it takes a fraction.
 *
 * `win_rate`, `roi_pct`, `edge_at_entry`, `avg_edge_at_entry`, `confidence`,
 * `kelly_raw` and `kelly_adjusted` are all fractions on the wire — `roi_pct`
 * despite its name. There is deliberately no `pct()` that takes an
 * already-multiplied number, so reaching for the wrong one reads wrong at the
 * call site instead of silently rendering 0.62%.
 */
export function pctFromFraction(
  v: number | null | undefined,
  dp = 1,
): string {
  if (v === null || v === undefined) return DASH;
  return (denormZero(v) * 100).toFixed(dp) + "%";
}

/** Signed percent from a fraction, for deltas. */
export function pctSigned(v: number | null | undefined, dp = 1): string {
  if (v === null || v === undefined) return DASH;
  const n = denormZero(v) * 100;
  const body = Math.abs(n).toFixed(dp) + "%";
  if (n > 0) return "+" + body;
  if (n < 0) return MINUS + body;
  return body;
}

export function durationMs(v: number | null | undefined): string {
  if (v === null || v === undefined) return DASH;
  // Zero reads as "0s" so a duration axis does not mix units at its origin.
  if (v === 0) return "0s";
  if (v < 1000) return Math.round(v) + "ms";
  const secs = v / 1000;
  if (secs < 60) return secs.toFixed(1) + "s";
  const m = Math.floor(secs / 60);
  const s = Math.round(secs - m * 60);
  if (m < 60) return `${m}m ${s}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${m - h * 60}m`;
}

export function durationSec(v: number | null | undefined): string {
  return v === null || v === undefined ? DASH : durationMs(v * 1000);
}

/**
 * Relative time. `now` is a parameter rather than a call to `Date.now()` so
 * this stays pure and every relative stamp on the page ticks from the same
 * clock instead of drifting apart.
 */
export function relTime(then: Date | null, now: number): string {
  if (!then) return DASH;
  const deltaMs = now - then.getTime();
  const future = deltaMs < 0;
  const secs = Math.abs(deltaMs) / 1000;

  let body: string;
  if (secs < 10) body = future ? "moments" : "just now";
  else if (secs < 60) body = `${Math.round(secs)}s`;
  else if (secs < 3600) body = `${Math.round(secs / 60)}m`;
  else if (secs < 86400) body = `${Math.round(secs / 3600)}h`;
  else body = `${Math.round(secs / 86400)}d`;

  if (body === "just now") return body;
  return future ? `in ${body}` : `${body} ago`;
}

/** Absolute time, always offered alongside a relative one via `title`. */
export function absTime(then: Date | null): string {
  if (!then) return DASH;
  return then.toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    timeZoneName: "short",
  });
}
