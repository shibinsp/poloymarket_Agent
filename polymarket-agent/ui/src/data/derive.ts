/** Shaping wire records into the series the charts want. */
import { num, sum } from "../lib/decimal";
import { parseTs, utcDayKey } from "../lib/time";
import type { ApiCostRecord, CycleRecord, TradeRecord } from "../api/types";

/** Cycles oldest-first, which is the order every time axis wants. */
export function cyclesAscending(rows: CycleRecord[]): CycleRecord[] {
  return [...rows].sort((a, b) => a.cycle_number - b.cycle_number);
}

/**
 * Trades newest-first.
 *
 * Worth doing explicitly: `/api/trades` arrives `id DESC` and
 * `/api/trades/all` arrives `id` ASC, so anything relying on arrival order is
 * right on one endpoint and backwards on the other.
 */
export function tradesNewestFirst(rows: TradeRecord[]): TradeRecord[] {
  return [...rows].sort((a, b) => (b.id ?? 0) - (a.id ?? 0));
}

export function cumulative(values: (number | null)[]): (number | null)[] {
  let running = 0;
  let seen = false;
  return values.map((v) => {
    if (v === null) return seen ? running : null;
    running += v;
    seen = true;
    return running;
  });
}

export interface DayBucket {
  day: string;
  cost: number;
  inputTokens: number | null;
  outputTokens: number | null;
}

/**
 * Costs grouped by UTC calendar day — the same day boundary the agent's own
 * `get_today_api_cost` uses, rather than the viewer's midnight.
 */
export function costsByUtcDay(rows: ApiCostRecord[]): DayBucket[] {
  const byDay = new Map<string, ApiCostRecord[]>();
  for (const r of rows) {
    const d = parseTs(r.created_at);
    if (!d) continue;
    const key = utcDayKey(d);
    const list = byDay.get(key);
    if (list) list.push(r);
    else byDay.set(key, [r]);
  }
  return [...byDay.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([day, list]) => ({
      day,
      cost: sum(list.map((r) => r.cost)) ?? 0,
      inputTokens: sum(list.map((r) => r.input_tokens)),
      outputTokens: sum(list.map((r) => r.output_tokens)),
    }));
}

export function costsByProvider(
  rows: ApiCostRecord[],
): { label: string; value: number }[] {
  const byProvider = new Map<string, number>();
  for (const r of rows) {
    const c = num(r.cost);
    if (c === null) continue;
    byProvider.set(r.provider, (byProvider.get(r.provider) ?? 0) + c);
  }
  return [...byProvider.entries()]
    .map(([label, value]) => ({ label, value }))
    .sort((a, b) => b.value - a.value);
}
