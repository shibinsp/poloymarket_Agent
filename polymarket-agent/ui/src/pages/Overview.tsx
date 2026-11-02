import { useDataset } from "../data/useDataset";
import { DatasetCard } from "../components/ui/DatasetCard";
import { Card } from "../components/ui/Card";
import { StatTile } from "../components/ui/StatTile";
import { EmptyState } from "../components/ui/States";
import { Badge } from "../components/ui/Badge";
import { DataTable } from "../components/ui/DataTable";
import { LineChart } from "../components/charts/LineChart";
import { ColumnChart } from "../components/charts/ColumnChart";
import { DonutChart } from "../components/charts/DonutChart";
import { ChartOrTable } from "../components/charts/ChartOrTable";
import { Link } from "../router/Link";
import { RecentStrategyTrades, usePolled, type IndiaIntraday, type RiskSnapshot, type UsOrbTrade } from "../components/live/StrategyLedgers";
import { cyclesAscending } from "../data/derive";
import { num } from "../lib/decimal";
import { int, money, moneyInr, pctFromFraction } from "../lib/format";
import { decimate } from "../lib/scale";
import type {
  CycleRecord,
  Health,
  UsPaperStatusResponse,
} from "../api/types";

export function Overview() {
  const intraday = usePolled<IndiaIntraday>("/api/india/intraday");
  const risk = usePolled<RiskSnapshot>("/api/risk");
  const usOrbTrades = usePolled<{ trades: UsOrbTrade[] }>("/api/us/orb/trades");
  const cycles = useDataset<CycleRecord[]>("cyclesAll");
  const us = useDataset<UsPaperStatusResponse>("usPaper");
  const health = useDataset<Health>("health");

  const FX = 86.5;

  // India: the intraday breakout's own Rs 2,500 ledger, not the older sandbox book.
  const indiaBaseInr = num(intraday?.capital_inr ?? null) ?? 0;
  const indiaProfit = num(intraday?.pnl_total_inr ?? null) ?? 0;
  const indiaCurrentInr = indiaBaseInr + indiaProfit;
  const indiaUsdEquivalent = indiaCurrentInr / FX;
  const indiaProfitPct = indiaBaseInr > 0 ? (indiaProfit / indiaBaseInr) * 100 : 0;
  const indiaTradeCount = intraday?.trades.length ?? 0;

  // US: the Alpaca paper account's own equity. Summing the trades table mixed
  // in trades from an earlier Alpaca account, so it no longer matches Alpaca.
  const usStrategyCapitalUsd = num(us.data?.base_capital_usd ?? null) ?? 0;
  const usCurrentUsd = num(risk?.current_equity ?? null) ?? 0;
  const usDayStartUsd = num(risk?.starting_equity ?? null) ?? usCurrentUsd;
  const usProfit = usCurrentUsd - usDayStartUsd;
  const usInrEquivalent = usCurrentUsd * FX;

  const currentBankroll = usCurrentUsd + indiaUsdEquivalent;

  // Both strategies' closed trades: what the win rate and net profit are about.
  const usTrades = usOrbTrades?.trades ?? [];
  const closedResults = [
    ...(intraday?.trades ?? []).filter((x) => x.pnl !== null).map((x) => num(x.pnl) ?? 0),
    ...usTrades.filter((x) => x.status === "CLOSED").map((x) => num(x.realized_pnl) ?? 0),
  ];
  const wins = closedResults.filter((v) => v > 0).length;
  const usClosedPnl = usTrades
    .filter((x) => x.status === "CLOSED")
    .reduce((sum, x) => sum + (num(x.realized_pnl) ?? 0), 0);
  const netUsd = usClosedPnl + indiaProfit / FX;
  const dailyLossPct = num(risk?.limits?.max_daily_loss_pct ?? null);
  const dailyLossUsd = num(risk?.limits?.max_daily_loss_usd ?? null);
  const drawdownPct = num(risk?.limits?.max_drawdown_pct ?? null);
  const isAgentHalted = health.data?.halted ?? false;
  const signedUsd = (v: number) => `${v >= 0 ? "+" : "-"}$${Math.abs(v).toFixed(2)}`;
  const signedInr = (v: number) => `${v >= 0 ? "+" : "-"}₹${Math.abs(v).toFixed(2)}`;

  // Capital Distribution for Donut Chart
  const capitalSegments = [
    {
      label: "NSE intraday (INR)",
      value: indiaUsdEquivalent,
      color: "var(--series-1)",
      sublabel: `₹${indiaCurrentInr.toLocaleString("en-IN", { maximumFractionDigits: 2 })}`,
    },
    {
      label: "Alpaca paper account (USD)",
      value: usCurrentUsd,
      color: "var(--series-2)",
      sublabel: `$${usCurrentUsd.toFixed(2)}`,
    },
  ];

  return (
    <div className="page">
      {/* 1. EXECUTIVE SUMMARY: PORTFOLIO & REAL-TIME CAPITAL */}
      <div className="card" style={{ padding: "16px 20px" }}>
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "16px", flexWrap: "wrap", gap: "10px" }}>
          <div>
            <h2 style={{ margin: 0, fontSize: "18px", fontWeight: "700", color: "var(--text-primary)" }}>
              Executive Portfolio Overview
            </h2>
            <p style={{ margin: "4px 0 0", fontSize: "13px", color: "var(--text-secondary)" }}>
              Multi-market autonomous trading across NSE India, US Equities (Alpaca), and Prediction Markets.
            </p>
          </div>
          <div style={{ display: "flex", gap: "8px", alignItems: "center" }}>
            <Badge tone={isAgentHalted ? "critical" : "good"}>
              {isAgentHalted ? "🔴 TRADING HALTED" : "🟢 AGENT ACTIVE & RUNNING"}
            </Badge>
            <Badge tone="neutral">Reference FX: $1 = ₹86.50</Badge>
          </div>
        </div>

        {/* 3 Unified Market Tiles */}
        <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(260px, 1fr))", gap: "14px" }}>
          {/* India Tile */}
          <div style={{ padding: "14px", background: "var(--surface-2)", borderRadius: "var(--radius-sm)", border: "1px solid var(--border)" }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "6px" }}>
              <span style={{ fontWeight: "600", fontSize: "13px", color: "var(--text-secondary)" }}>🇮🇳 Indian Equities (NSE)</span>
              <Badge tone="good">₹ Intraday breakout · paper</Badge>
            </div>
            <div style={{ fontSize: "24px", fontWeight: "700", color: "var(--text-primary)", fontVariantNumeric: "tabular-nums" }}>
              {moneyInr(indiaCurrentInr)}
            </div>
            <div style={{ fontSize: "12px", marginTop: "4px", color: indiaProfit >= 0 ? "var(--status-good)" : "var(--status-critical)" }}>
              P&L: <strong>{signedInr(indiaProfit)}</strong> ({indiaProfitPct >= 0 ? "+" : ""}{indiaProfitPct.toFixed(2)}%)
              <span style={{ color: "var(--text-muted)", marginLeft: "6px" }}>(Capital: ₹{indiaBaseInr.toLocaleString("en-IN")})</span>
            </div>
            <div style={{ fontSize: "11px", color: "var(--text-muted)", marginTop: "2px" }}>
              USD Value: ${indiaUsdEquivalent.toFixed(2)} · {indiaTradeCount} {indiaTradeCount === 1 ? "trade" : "trades"}
            </div>
            <div style={{ marginTop: "10px", fontSize: "12px" }}>
              <Link to="/india">India trades →</Link>
            </div>
          </div>

          {/* US Tile */}
          <div style={{ padding: "14px", background: "var(--surface-2)", borderRadius: "var(--radius-sm)", border: "1px solid var(--border)" }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "6px" }}>
              <span style={{ fontWeight: "600", fontSize: "13px", color: "var(--text-secondary)" }}>🇺🇸 US Equities & ETFs</span>
              <Badge tone="good">$ Alpaca paper account</Badge>
            </div>
            <div style={{ fontSize: "24px", fontWeight: "700", color: "var(--text-primary)", fontVariantNumeric: "tabular-nums" }}>
              {money(usCurrentUsd)}
            </div>
            <div style={{ fontSize: "12px", marginTop: "4px", color: usProfit >= 0 ? "var(--status-good)" : "var(--status-critical)" }}>
              Today: <strong>{signedUsd(usProfit)}</strong>
              <span style={{ color: "var(--text-muted)", marginLeft: "6px" }}>(Strategy capital: ${usStrategyCapitalUsd.toFixed(0)})</span>
            </div>
            <div style={{ fontSize: "11px", color: "var(--text-muted)", marginTop: "2px" }}>
              INR Value: ₹{usInrEquivalent.toFixed(0)} · live equity from Alpaca
            </div>
            <div style={{ marginTop: "10px", fontSize: "12px" }}>
              <Link to="/us">US trades →</Link>
            </div>
          </div>

          {/* Combined Total Tile */}
          <div style={{ padding: "14px", background: "var(--surface-2)", borderRadius: "var(--radius-sm)", border: "1px solid var(--border-strong)" }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "6px" }}>
              <span style={{ fontWeight: "600", fontSize: "13px", color: "var(--text-secondary)" }}>🌐 Combined Portfolio</span>
              <Badge tone="neutral">Paper money only</Badge>
            </div>
            <div style={{ fontSize: "24px", fontWeight: "700", color: "var(--status-good)", fontVariantNumeric: "tabular-nums" }}>
              {money(currentBankroll)}
            </div>
            <div style={{ fontSize: "12px", marginTop: "4px", color: "var(--text-secondary)" }}>
              India {signedInr(indiaProfit)} all time · US {signedUsd(usProfit)} today
            </div>
            <div style={{ marginTop: "10px", fontSize: "12px" }}>
              <Link to="/trades">All trades →</Link>
            </div>
          </div>
        </div>
      </div>

      {/* 2. OPERATIONAL TILES: METRICS AT A GLANCE */}
      <Card title="Key Performance & Risk Metrics">
        <div className="grid grid--tiles">
          <StatTile
            label="Win Rate"
            value={closedResults.length ? pctFromFraction(wins / closedResults.length) : "—"}
            sub={`${wins} won of ${closedResults.length} closed trade(s), India and US`}
          />
          <StatTile
            label="Net Profit"
            value={signedUsd(netUsd)}
            tone={netUsd > 0 ? "pos" : netUsd < 0 ? "neg" : null}
            sub={`India ${signedInr(indiaProfit)} · US ${signedUsd(usClosedPnl)} after charges`}
          />
          <StatTile
            label="Trades"
            value={int(indiaTradeCount + usTrades.length)}
            sub={`${indiaTradeCount} India · ${usTrades.length} US`}
          />
          <StatTile
            label="Daily Loss Limit"
            value={dailyLossPct === null ? "—" : pctFromFraction(dailyLossPct)}
            sub={`${dailyLossUsd === null ? "" : `$${dailyLossUsd.toFixed(0)} a day · `}stops at ${drawdownPct === null ? "—" : pctFromFraction(drawdownPct)} down from the peak`}
          />
        </div>
      </Card>

      {/* 3. CHARTS ROW: PIE / DONUT + EQUITY CURVE */}
      <div className="grid grid--2">
        {/* Pie / Donut Chart Card */}
        <Card title="Capital Allocation by Asset Class">
          <DonutChart
            ariaLabel="Asset Class Capital Allocation"
            segments={capitalSegments}
            centerLabel="US + India (paper)"
            centerValue={money(currentBankroll)}
          />
        </Card>

        {/* Portfolio Equity Curve */}
        <DatasetCard
          title="Portfolio equity curve"
          label="Bankroll over cycles"
          dataset={cycles}
          datasetKey="cyclesAll"
        >
          {(rows) => {
            const asc = cyclesAscending(rows);
            const shown = decimate(asc, (c) => num(c.bankroll) ?? 0, 120);
            if (shown.length === 0) {
              return <EmptyState title="No cycles recorded yet" />;
            }
            return (
              <ChartOrTable
                label="Bankroll"
                chart={
                  <LineChart
                    ariaLabel="Bankroll history"
                    categories={shown.map((c) => String(c.cycle_number))}
                    formatX={(c) => "C" + c}
                    formatY={(v) => money(v)}
                    series={[
                      {
                        name: "Total Portfolio Equity ($)",
                        color: "var(--series-1)",
                        values: shown.map((c) => num(c.bankroll) ?? 0),
                      },
                    ]}
                  />
                }
                table={
                  <DataTable
                    rows={[...asc].reverse().slice(0, 100)}
                    rowKey={(c) => String(c.id ?? c.cycle_number)}
                    columns={[
                      { key: "c", header: "Cycle", numeric: true, render: (c) => int(c.cycle_number) },
                      {
                        key: "b",
                        header: "Bankroll",
                        numeric: true,
                        render: (c) =>
                          money(num(c.bankroll) ?? 0),
                      },
                    ]}
                  />
                }
              />
            );
          }}
        </DatasetCard>
      </div>

      {/* 4. ACTIVITY & CYCLE HISTOGRAM */}
      <DatasetCard
        title="Trades placed per cycle"
        label="Cycle history"
        dataset={cycles}
        datasetKey="cyclesAll"
      >
        {(rows) => {
          const asc = cyclesAscending(rows);
          const shown = decimate(asc, (c) => c.trades_placed, 120);
          if (shown.length === 0) {
            return <EmptyState title="No cycles recorded yet" />;
          }
          return (
            <ChartOrTable
              label="Trades placed"
              chart={
                <ColumnChart
                  ariaLabel="Trades placed per cycle"
                  categories={shown.map((c) => String(c.cycle_number))}
                  formatX={(c) => "C" + c}
                  formatY={(v) => int(v)}
                  series={[
                    {
                      name: "Trades placed",
                      color: "var(--series-1)",
                      values: shown.map((c) => c.trades_placed),
                    },
                  ]}
                />
              }
              table={
                <DataTable
                  rows={[...asc].reverse().slice(0, 100)}
                  rowKey={(c) => String(c.id ?? c.cycle_number)}
                  columns={[
                    { key: "c", header: "Cycle", numeric: true, render: (c) => int(c.cycle_number) },
                    { key: "t", header: "Trades", numeric: true, render: (c) => int(c.trades_placed) },
                  ]}
                />
              }
            />
          );
        }}
      </DatasetCard>

      {/* 5. RECENT TRADES FROM BOTH STRATEGIES */}
      <Card title="Recent trade executions" actions={<Link to="/trades">All trades →</Link>}>
        <RecentStrategyTrades />
      </Card>
    </div>
  );
}
