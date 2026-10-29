import { describe, expect, it } from "vitest";
import { equitySeries } from "./equity";
import type { DailyEquity } from "../api/types";

function row(
  day: string,
  closing_equity: string | null,
  starting_equity = "100",
): DailyEquity {
  return {
    day,
    starting_equity,
    high_water_mark: "100",
    closing_equity,
    realized_pnl: null,
    updated_at: null,
  };
}

describe("equitySeries", () => {
  it("does not turn a missing close into opening equity or zero", () => {
    expect(equitySeries([row("2026-10-28", null)])).toEqual([
      { day: "2026-10-28", equity: null, peak: null, drawdown: null },
    ]);
  });

  it("calculates drawdown from the highest prior recorded close", () => {
    expect(
      equitySeries([
        row("2026-10-27", "120"),
        row("2026-10-28", null),
        row("2026-10-29", "90"),
      ]),
    ).toEqual([
      { day: "2026-10-27", equity: 120, peak: 120, drawdown: 0 },
      { day: "2026-10-28", equity: null, peak: 120, drawdown: null },
      { day: "2026-10-29", equity: 90, peak: 120, drawdown: 0.25 },
    ]);
  });
});
