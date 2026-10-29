import { num } from "./decimal";
import type { DailyEquity } from "../api/types";

export interface EquityPoint {
  day: string;
  equity: number | null;
  peak: number | null;
  drawdown: number | null;
}

/** Derive drawdown from recorded closing balances without filling data gaps. */
export function equitySeries(rows: DailyEquity[]): EquityPoint[] {
  let peak = -Infinity;
  return rows.map((row) => {
    const equity = num(row.closing_equity);
    if (equity !== null) peak = Math.max(peak, equity);
    const runningPeak = Number.isFinite(peak) ? peak : null;
    return {
      day: row.day,
      equity,
      peak: runningPeak,
      drawdown:
        equity !== null && runningPeak !== null && runningPeak > 0
          ? Math.max(0, (runningPeak - equity) / runningPeak)
          : null,
    };
  });
}
