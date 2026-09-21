import { describe, expect, it } from "vitest";
import { executionStats, quantile } from "./execution";
import type { FillRecord, OrderRecord } from "../api/types";

function order(state: string, filled_qty = "0"): OrderRecord {
  return {
    id: 1,
    client_order_id: `c-${state}-${filled_qty}-${Math.random()}`,
    venue_order_id: null,
    venue_id: "alpaca",
    symbol: "BTC/USD",
    side: "BUY",
    intent: "ENTRY",
    trade_id: null,
    limit_price: "100",
    qty: "1",
    filled_qty,
    avg_fill_price: null,
    state,
    reject_reason: null,
    cycle: 1,
    submitted_at: null,
    updated_at: null,
    expires_at: null,
  };
}

function fill(slippage_bps: string | null, time_to_fill_ms: number | null): FillRecord {
  return {
    id: 1,
    order_id: 1,
    client_order_id: "c",
    venue_id: "alpaca",
    symbol: "BTC/USD",
    side: "BUY",
    intent: "ENTRY",
    qty: "1",
    price: "100",
    fee: null,
    mid_at_submit: "100",
    slippage_bps,
    time_to_fill_ms,
  } as FillRecord;
}

describe("quantile", () => {
  it("returns null for an empty sample rather than zero", () => {
    // Zero slippage and "no fills yet" are very different claims, and the
    // second one must not render as a passing grade.
    expect(quantile([], 0.5)).toBeNull();
  });

  it("returns an observed value, never an interpolation", () => {
    // p95 of four samples is the largest. Interpolating would produce 29.5,
    // a number no fill ever traded at.
    expect(quantile([10, 20, 29, 30], 0.95)).toBe(30);
  });

  it("takes the lower of the two middle values for an even sample", () => {
    // Nearest-rank, consistently: ceil(0.5 * 4) = 2 -> the second value.
    expect(quantile([10, 20, 30, 40], 0.5)).toBe(20);
  });

  it("handles a single sample", () => {
    expect(quantile([7], 0.5)).toBe(7);
    expect(quantile([7], 0.95)).toBe(7);
  });

  it("does not mutate its input", () => {
    const values = [3, 1, 2];
    quantile(values, 0.5);
    expect(values).toEqual([3, 1, 2]);
  });
});

describe("executionStats", () => {
  it("measures the fill rate over terminal orders only", () => {
    // Two finished (one filled), two still working. The rate is 1/2, not
    // 1/4 — otherwise submitting an order makes the venue look worse.
    const stats = executionStats(
      [
        order("FILLED", "1"),
        order("CANCELLED"),
        order("ACCEPTED"),
        order("PENDING"),
      ],
      [],
    );
    expect(stats.terminal).toBe(2);
    expect(stats.filled).toBe(1);
    expect(stats.fillRate).toBe(0.5);
    expect(stats.unresolved).toBe(2);
  });

  it("counts a partial that later expired as a fill", () => {
    // It put a position on. Counting it as a miss would make a venue that
    // fills half of everything look like one that fills none of it.
    const stats = executionStats([order("EXPIRED", "0.4")], []);
    expect(stats.filled).toBe(1);
    expect(stats.fillRate).toBe(1);
  });

  it("does not count an expired order that never filled", () => {
    const stats = executionStats([order("EXPIRED", "0")], []);
    expect(stats.filled).toBe(0);
    expect(stats.fillRate).toBe(0);
  });

  it("reports a zero rate rather than dividing by zero", () => {
    const stats = executionStats([order("PENDING")], []);
    expect(stats.terminal).toBe(0);
    expect(stats.fillRate).toBe(0);
  });

  it("counts UNKNOWN separately and as unresolved", () => {
    // UNKNOWN is not terminal: a submit that timed out may still have been
    // accepted, so it is never treated as "did not happen".
    const stats = executionStats([order("UNKNOWN")], []);
    expect(stats.unknown).toBe(1);
    expect(stats.unresolved).toBe(1);
    expect(stats.terminal).toBe(0);
  });

  it("ignores fills with no recorded slippage instead of treating them as zero", () => {
    const stats = executionStats(
      [],
      [fill("12", 100), fill(null, 200), fill("8", null)],
    );
    expect(stats.withSlippage).toBe(2);
    expect(stats.medianSlippage).toBe(8);
    // Only two fills carry a time, and the median takes the lower of them.
    expect(stats.medianFillMs).toBe(100);
  });

  it("reports nulls, not zeroes, when there is nothing to measure", () => {
    const stats = executionStats([], []);
    expect(stats.medianSlippage).toBeNull();
    expect(stats.p95Slippage).toBeNull();
    expect(stats.medianFillMs).toBeNull();
  });

  it("treats a negative time to fill as unusable", () => {
    // Clock skew between the agent host and the venue can produce one.
    const stats = executionStats([], [fill("5", -20)]);
    expect(stats.medianFillMs).toBeNull();
  });

  it("keeps negative slippage, which is price improvement", () => {
    const stats = executionStats([], [fill("-4", 10)]);
    expect(stats.medianSlippage).toBe(-4);
  });
});
