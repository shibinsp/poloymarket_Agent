import { Card } from "../components/ui/Card";
import { StatTile } from "../components/ui/StatTile";
import { DataTable } from "../components/ui/DataTable";
import { EmptyState } from "../components/ui/States";
import { usePolled } from "../components/live/StrategyLedgers";
import { num } from "../lib/decimal";
import { etTime, inrSigned, istTime, moneySigned } from "../lib/format";
import { parseTs } from "../lib/time";

/**
 * Post-mortems of every closed strategy trade, from /api/reviews (src/review.rs).
 * Each loser is filed under the one failure that explains it, with the fix to
 * test. Nothing here changes a strategy: fixes are tested on past data first.
 */

interface Review {
  market: "IN" | "US";
  strategy: string;
  symbol: string;
  side: string;
  day: string;
  entry_at: string;
  exit_reason: string | null;
  pnl: string;
  minutes_held: number;
  mfe_r: string | null;
  mae_r: string | null;
  index_move_pct: string | null;
  label: string;
  lesson: string;
}

interface SummaryRow {
  market: "IN" | "US";
  strategy: string;
  label: string;
  count: number;
  pnl: string | number;
}

const LABEL: Record<string, string> = {
  WIN: "Win",
  COSTS: "Charges ate it",
  GAVE_BACK: "Gave back profit",
  FALSE_BREAKOUT: "False breakout",
  FOUGHT_MARKET: "Fought the market",
  STOPPED_LATER: "Stopped later",
  FADED: "Faded to the close",
};
const STRATEGY: Record<string, string> = {
  india_orb: "India breakout",
  india_burst: "India opening burst",
  india_gapgo: "India gap and go",
  india_orb_tfm_skip: "India breakout skipped by TimesFM (shadow)",
  india_orb_track: "India breakout, tracking only (no money)",
  india_burst_tfm_skip: "India burst skipped by TimesFM (shadow)",
  orb: "US breakout",
  us_burst: "US opening burst",
};
const label = (l: string) => LABEL[l] ?? l;
const strategy = (s: string) => STRATEGY[s] ?? s;
const pnlText = (market: string, v: string | number | null) =>
  market === "IN" ? inrSigned(v ?? 0) : moneySigned(num(v ?? null));
const r = (v: string | null) => (v === null ? "—" : `${Number(v).toFixed(2)}R`);

export function LearnFromMistakes() {
  const data = usePolled<{ summary: SummaryRow[]; reviews: Review[] }>("/api/reviews");
  const reviews = data?.reviews ?? [];
  const summary = data?.summary ?? [];
  const losers = summary.filter((s) => s.label !== "WIN");
  const lossCount = losers.reduce((n, s) => n + s.count, 0);
  const counts = new Map<string, number>();
  for (const s of losers) counts.set(s.label, (counts.get(s.label) ?? 0) + s.count);
  const top = [...counts.entries()].sort((a, b) => b[1] - a[1])[0];

  return (
    <div className="page">
      <Card title="Learn from mistakes — every closed trade, reviewed">
        <p style={{ margin: "0 0 12px", opacity: 0.8 }}>
          After each trade closes, the agent measures how far it went for and against us (in R, the
          planned risk to the stop), what the index did meanwhile, and files every loser under the one
          failure that explains it. A fix is only adopted after it beats the current rules on past data.
        </p>
        <div className="grid grid--tiles">
          <StatTile label="Trades reviewed" value={String(reviews.length)} sub="India and US, all strategies" />
          <StatTile label="Losing trades" value={String(lossCount)} tone={lossCount ? "neg" : null} sub="Each with one failure label" />
          <StatTile
            label="Most common failure"
            value={top ? label(top[0]) : "—"}
            sub={top ? `${top[1]} of ${lossCount} losing trade(s)` : "No losing trades reviewed yet"}
          />
        </div>
      </Card>

      <Card title="Failures by strategy">
        <DataTable
          rows={summary}
          rowKey={(s) => `${s.market}-${s.strategy}-${s.label}`}
          empty={<EmptyState title="No trades reviewed yet" />}
          columns={[
            { key: "s", header: "Strategy", render: (s) => strategy(s.strategy) },
            { key: "l", header: "Result", render: (s) => label(s.label) },
            { key: "c", header: "Trades", numeric: true, render: (s) => String(s.count) },
            { key: "p", header: "P&L", numeric: true, render: (s) => pnlText(s.market, s.pnl) },
          ]}
        />
      </Card>

      <Card title="Trade by trade">
        <DataTable
          rows={reviews}
          rowKey={(v) => `${v.market}-${v.strategy}-${v.entry_at}-${v.symbol}`}
          empty={<EmptyState title="No trades reviewed yet" />}
          columns={[
            { key: "t", header: "Entered", render: (v) => (v.market === "IN" ? istTime(parseTs(v.entry_at)) : etTime(parseTs(v.entry_at))) },
            { key: "s", header: "Strategy", render: (v) => strategy(v.strategy) },
            { key: "y", header: "Stock", render: (v) => `${v.symbol} ${v.side}` },
            { key: "p", header: "P&L", numeric: true, render: (v) => pnlText(v.market, v.pnl) },
            { key: "h", header: "Held", numeric: true, render: (v) => `${v.minutes_held} min` },
            { key: "b", header: "Best", numeric: true, render: (v) => r(v.mfe_r) },
            { key: "w", header: "Worst", numeric: true, render: (v) => r(v.mae_r) },
            { key: "i", header: "Index", numeric: true, render: (v) => (v.index_move_pct === null ? "—" : `${Number(v.index_move_pct) > 0 ? "+" : ""}${v.index_move_pct}%`) },
            { key: "l", header: "Result", render: (v) => label(v.label) },
            { key: "n", header: "What happened, and the fix to test", render: (v) => v.lesson },
          ]}
        />
      </Card>
    </div>
  );
}
