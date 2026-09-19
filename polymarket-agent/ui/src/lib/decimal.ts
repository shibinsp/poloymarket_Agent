/**
 * Parsing numbers that arrive in two different JSON types.
 *
 * The agent pins `rust_decimal` with the `serde-str` feature, so every
 * `Decimal` serializes as a *string* while plain integer counts serialize as
 * numbers. One payload therefore mixes both:
 *
 *   {"win_rate": "0.625", "wins": 5, "avg_cycle_duration_ms": 199491.0}
 *
 * JS coercion hides this right up until something calls `.toFixed()` on what
 * turned out to be a string. So nothing reads these fields raw — they all go
 * through `num()`.
 *
 * Precision note: these become f64. That is fine for display and for chart
 * geometry, and wrong for re-deriving money. The UI never sums trade P&Ls to
 * recompute a total; it shows the server's figure.
 */

/**
 * Parse a value that may be a JSON string, a JSON number, or absent.
 *
 * Returns `null` — never `0` — when there is nothing to parse. The old
 * dashboard used `parseFloat(x || 0)`, which is how an unparseable value
 * became a confident `$0.00`. "No data" and "zero" are different claims and
 * the formatters render them differently.
 */
export function num(v: unknown): number | null {
  if (typeof v === "number") return Number.isFinite(v) ? v : null;
  if (typeof v === "string") {
    const t = v.trim();
    // Number("") is 0, so the empty check has to come first.
    if (t === "") return null;
    const n = Number(t);
    return Number.isFinite(n) ? n : null;
  }
  return null;
}

/** `num` with a caller-chosen fallback, for places that genuinely want one. */
export function numOr(v: unknown, fallback: number): number {
  const n = num(v);
  return n === null ? fallback : n;
}

/** Sum, skipping anything unparseable. Empty input sums to `null`, not 0. */
export function sum(values: unknown[]): number | null {
  let total = 0;
  let seen = false;
  for (const v of values) {
    const n = num(v);
    if (n !== null) {
      total += n;
      seen = true;
    }
  }
  return seen ? total : null;
}

/**
 * Normalise negative zero. `rust_decimal` really does emit `"-0"`, and
 * `(-0).toFixed(2)` is `"-0.00"`, which reads as a loss that did not happen.
 */
export function denormZero(n: number): number {
  return n === 0 ? 0 : n;
}
