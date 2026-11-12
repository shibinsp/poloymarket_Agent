import { Card } from "../ui/Card";
import { DataTable } from "../ui/DataTable";
import { EmptyState } from "../ui/States";
import { usePolled } from "./StrategyLedgers";

interface TargetDay {
  day: string;
  us_account_usd: string;
  india_inr: string;
  polymarket_usd: string;
  total_inr: string;
  hit: boolean;
  live: boolean;
}

interface Target {
  target_inr: string;
  usd_inr: string;
  since: string;
  days: TargetDay[];
  total_inr: string;
  average_inr: string;
  days_hit: number;
}

const n = (v: string | null | undefined) => Number(v ?? 0);
const inr = (v: number) => `${v >= 0 ? "+" : "−"}₹${Math.abs(v).toLocaleString("en-IN", { maximumFractionDigits: 0 })}`;
const usd = (v: number) => `${v >= 0 ? "+" : "−"}$${Math.abs(v).toFixed(2)}`;

/** The user's daily goal (Rs 500 a day) across every book: today, the running average, each day. */
export function TargetTracker() {
  const data = usePolled<Target>("/api/target");
  const target = n(data?.target_inr) || 500;
  const days = data?.days ?? [];
  const today = days.find((d) => d.live) ?? null;
  const todayInr = today ? n(today.total_inr) : 0;
  const avg = n(data?.average_inr);
  const pct = Math.max(0, Math.min(100, (todayInr / target) * 100));
  const tone = (v: number) => (v >= target ? "var(--good, #16a34a)" : v >= 0 ? "var(--warning, #d97706)" : "var(--critical, #dc2626)");
  return (
    <Card title={`🎯 Daily target: ₹${target.toLocaleString("en-IN")} a day, all books (paper)`}>
      <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(200px, 1fr))", gap: "14px", marginBottom: "12px" }}>
        <div>
          <div style={{ fontSize: "12px", opacity: 0.7 }}>Today{today ? " (live)" : ""}</div>
          <div style={{ fontSize: "26px", fontWeight: 700, color: tone(todayInr), fontVariantNumeric: "tabular-nums" }}>{inr(todayInr)}</div>
          <div style={{ height: "8px", background: "var(--surface-2, #eee)", borderRadius: "4px", marginTop: "6px", overflow: "hidden" }}>
            <div style={{ width: `${pct}%`, height: "100%", background: tone(todayInr) }} />
          </div>
          <div style={{ fontSize: "12px", opacity: 0.7, marginTop: "4px" }}>{todayInr >= target ? "Target reached" : `₹${Math.max(0, target - todayInr).toLocaleString("en-IN", { maximumFractionDigits: 0 })} to go`}</div>
        </div>
        <div>
          <div style={{ fontSize: "12px", opacity: 0.7 }}>Average per day since {data?.since ?? "—"}</div>
          <div style={{ fontSize: "26px", fontWeight: 700, color: tone(avg), fontVariantNumeric: "tabular-nums" }}>{inr(avg)}</div>
          <div style={{ fontSize: "12px", opacity: 0.7, marginTop: "4px" }}>Total {inr(n(data?.total_inr))} over {days.length} day(s)</div>
        </div>
        <div>
          <div style={{ fontSize: "12px", opacity: 0.7 }}>Days at or above target</div>
          <div style={{ fontSize: "26px", fontWeight: 700, fontVariantNumeric: "tabular-nums" }}>{data ? `${data.days_hit} of ${days.length}` : "—"}</div>
          <div style={{ fontSize: "12px", opacity: 0.7, marginTop: "4px" }}>$1 = ₹{n(data?.usd_inr).toFixed(2)} · display only, no strategy reads it</div>
        </div>
      </div>
      <DataTable
        rows={[...days].reverse()}
        rowKey={(d) => d.day}
        empty={<EmptyState title="No days recorded yet" />}
        columns={[
          { key: "d", header: "Day (UTC)", render: (d) => `${d.day}${d.live ? " (live)" : ""}` },
          { key: "u", header: "Alpaca account (US stocks, options, crypto)", numeric: true, render: (d) => usd(n(d.us_account_usd)) },
          { key: "i", header: "India books", numeric: true, render: (d) => inr(n(d.india_inr)) },
          { key: "p", header: "Polymarket", numeric: true, render: (d) => usd(n(d.polymarket_usd)) },
          { key: "t", header: "Total (₹)", numeric: true, render: (d) => <span style={{ color: tone(n(d.total_inr)), fontWeight: 600 }}>{inr(n(d.total_inr))}</span> },
          { key: "h", header: "Target", render: (d) => (d.hit ? "✅ hit" : "—") },
        ]}
      />
    </Card>
  );
}
