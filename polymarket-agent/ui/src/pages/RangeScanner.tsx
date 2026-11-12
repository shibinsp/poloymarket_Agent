import { useState } from "react";
import { Card } from "../components/ui/Card";
import { DataTable } from "../components/ui/DataTable";
import { EmptyState } from "../components/ui/States";
import { usePolled } from "../components/live/StrategyLedgers";
import { etTime, istTime } from "../lib/format";
import { parseTs } from "../lib/time";

interface RangeRow {
  symbol: string;
  price: number;
  high_so_far: number;
  low_so_far: number;
  likely_high: number;
  likely_low: number;
  p_new_high: number;
  p_new_low: number;
  typical_range_pct: number;
  range_used_pct: number;
}

interface MarketScan {
  as_of: string | null;
  note: string;
  rows: RangeRow[];
}

type SortKey = "range_used_pct" | "p_new_high" | "p_new_low" | "symbol";

const SORTS: { key: SortKey; label: string }[] = [
  { key: "range_used_pct", label: "Range used" },
  { key: "p_new_high", label: "Chance of new high" },
  { key: "p_new_low", label: "Chance of new low" },
  { key: "symbol", label: "Name" },
];

const p2 = (x: number) => x.toFixed(2);

function ScanTable({ title, scan, time }: { title: string; scan: MarketScan | null | undefined; time: (d: Date | null) => string }) {
  const [sort, setSort] = useState<SortKey>("range_used_pct");
  const [filter, setFilter] = useState("");
  const rows = (scan?.rows ?? [])
    .filter((r) => r.symbol.toLowerCase().includes(filter.trim().toLowerCase()))
    .sort((a, b) => (sort === "symbol" ? a.symbol.localeCompare(b.symbol) : b[sort] - a[sort]));
  const asOf = scan?.as_of ? time(parseTs(scan.as_of)) : null;
  return (
    <Card title={title}>
      <div style={{ display: "flex", gap: "8px", flexWrap: "wrap", alignItems: "center", marginBottom: "8px", fontSize: "13px" }}>
        <span>Sort by:</span>
        {SORTS.map((s) => (
          <button
            key={s.key}
            onClick={() => setSort(s.key)}
            style={{ padding: "3px 8px", borderRadius: "6px", border: "1px solid var(--border, #444)", background: sort === s.key ? "var(--accent, #3b82f6)" : "transparent", color: "inherit", cursor: "pointer" }}
          >
            {s.label}
          </button>
        ))}
        <input
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="Find a stock"
          style={{ padding: "3px 8px", borderRadius: "6px", border: "1px solid var(--border, #444)", background: "transparent", color: "inherit" }}
        />
        <span style={{ opacity: 0.7 }}>
          {asOf ? `Data as of ${asOf}` : "No scan yet: it runs every 5 minutes while the market is open"}
          {scan?.note ? ` · ${scan.note}` : ""}
        </span>
      </div>
      <DataTable
        rows={rows}
        rowKey={(r) => r.symbol}
        empty={<EmptyState title="Nothing scanned yet" />}
        columns={[
          { key: "s", header: "Stock", render: (r) => r.symbol },
          { key: "p", header: "Price", numeric: true, render: (r) => p2(r.price) },
          { key: "lo", header: "Low so far", numeric: true, render: (r) => p2(r.low_so_far) },
          { key: "hi", header: "High so far", numeric: true, render: (r) => p2(r.high_so_far) },
          { key: "llo", header: "Likely low (80%)", numeric: true, render: (r) => p2(r.likely_low) },
          { key: "lhi", header: "Likely high (80%)", numeric: true, render: (r) => p2(r.likely_high) },
          { key: "pnh", header: "New high later", numeric: true, render: (r) => `${r.p_new_high}%` },
          { key: "pnl", header: "New low later", numeric: true, render: (r) => `${r.p_new_low}%` },
          { key: "tr", header: "Usual day's range", numeric: true, render: (r) => `${r.typical_range_pct.toFixed(2)}%` },
          { key: "ru", header: "Range used", numeric: true, render: (r) => `${r.range_used_pct}%` },
        ]}
      />
    </Card>
  );
}

/** How high and how low each stock is likely to trade for the rest of today. */
export function RangeScanner() {
  const data = usePolled<{ india: MarketScan | null; us: MarketScan | null }>("/api/range-scanner");
  return (
    <div className="page">
      <Card title="How to read this">
        <p style={{ margin: 0, fontSize: "13px", lineHeight: 1.5 }}>
          For each stock: where the rest of today is likely to reach, not which way it will go. On 80% of days the
          high stayed at or below <b>Likely high</b> and the low at or above <b>Likely low</b>. <b>New high later</b> is
          the chance the stock trades above today&apos;s high so far before the close. <b>Range used</b> is today&apos;s
          range so far against the stock&apos;s usual day; past 100% it has already moved more than usual. Tested on
          May–Oct 2026 days the model never saw: predicted 66% → 65% happened, 37% → 36%. Display only; no strategy
          trades on it.
        </p>
      </Card>
      <ScanTable title="🇮🇳 NIFTY 50 — range for the rest of today" scan={data?.india} time={istTime} />
      <ScanTable title="🇺🇸 US stocks — range for the rest of today (live IEX prices)" scan={data?.us} time={etTime} />
    </div>
  );
}
