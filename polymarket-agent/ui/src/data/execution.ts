/**
 * Execution-quality arithmetic.
 *
 * Extracted from the Orders page because these are the numbers the paper
 * window is promoted on — a fill rate, a median and a p95 slippage — and a
 * number that decides whether real money gets deployed should be testable
 * without rendering a page.
 */
import { num } from "../lib/decimal";
import type { FillRecord, OrderRecord } from "../api/types";

/** States an order will not move out of. */
export const TERMINAL_ORDER_STATES = new Set([
  "FILLED",
  "CANCELLED",
  "EXPIRED",
  "REJECTED",
]);

export interface ExecutionStats {
  /** Orders that reached a terminal state. */
  terminal: number;
  /** Of those, how many put any quantity on. */
  filled: number;
  fillRate: number;
  /** Still working, or of unknown fate. */
  unresolved: number;
  unknown: number;
  withSlippage: number;
  medianSlippage: number | null;
  p95Slippage: number | null;
  medianFillMs: number | null;
}

/**
 * Fill rate is computed over *terminal* orders only.
 *
 * Including orders that are still working would make the rate fall every time
 * one is submitted and climb as it fills, so a busy cycle would read as a
 * venue problem. The question is "of the orders that finished, how many
 * finished by filling".
 *
 * A partial fill that later expired or was cancelled counts as filled: it put
 * a position on, which is what the number is about. Counting it as a miss
 * would make a venue that fills half of everything look like one that fills
 * none of it.
 */
export function executionStats(
  orders: OrderRecord[],
  fills: FillRecord[],
): ExecutionStats {
  let terminal = 0;
  let filled = 0;
  let unresolved = 0;
  let unknown = 0;

  for (const o of orders) {
    const state = o.state.toUpperCase();
    if (state === "UNKNOWN") unknown += 1;

    if (TERMINAL_ORDER_STATES.has(state)) {
      terminal += 1;
      const anyQty = (num(o.filled_qty) ?? 0) > 0;
      if (state === "FILLED" || anyQty) filled += 1;
    } else {
      unresolved += 1;
    }
  }

  const slippage = fills
    .map((f) => num(f.slippage_bps))
    .filter((v): v is number => v !== null);
  const times = fills
    .map((f) => f.time_to_fill_ms)
    .filter((v): v is number => v !== null && v >= 0);

  return {
    terminal,
    filled,
    fillRate: terminal === 0 ? 0 : filled / terminal,
    unresolved,
    unknown,
    withSlippage: slippage.length,
    medianSlippage: quantile(slippage, 0.5),
    p95Slippage: quantile(slippage, 0.95),
    medianFillMs: quantile(times, 0.5),
  };
}

/**
 * Nearest-rank quantile.
 *
 * Deliberately not interpolating. With a handful of fills an interpolated p95
 * is a price that never happened, and the promotion criterion is about what
 * the venue actually did — "p95 slippage ≤ 30bps" should mean an observed
 * fill, not an average of two.
 */
export function quantile(values: number[], q: number): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const rank = Math.ceil(q * sorted.length);
  return sorted[Math.min(Math.max(rank - 1, 0), sorted.length - 1)];
}
