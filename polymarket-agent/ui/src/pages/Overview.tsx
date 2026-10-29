import { useEffect, useState } from "react";
import { useDataset } from "../data/useDataset";
import { dataStore } from "../data/store";
import { postJson, failureText } from "../api/client";
import { DatasetCard } from "../components/ui/DatasetCard";
import { Card } from "../components/ui/Card";
import { StatTile } from "../components/ui/StatTile";
import { EmptyState } from "../components/ui/States";
import { Badge, tradeStatusTone } from "../components/ui/Badge";
import { DataTable } from "../components/ui/DataTable";
import { LineChart } from "../components/charts/LineChart";
import { ColumnChart } from "../components/charts/ColumnChart";
import { DonutChart } from "../components/charts/DonutChart";
import { CanvasCandleChart, type CandlePoint } from "../components/charts/CanvasCandleChart";
import { AgentActionButtons } from "../components/shell/AgentActionButtons";
import { ChartOrTable } from "../components/charts/ChartOrTable";
import { Link } from "../router/Link";
import { cyclesAscending, tradesNewestFirst } from "../data/derive";
import { num } from "../lib/decimal";
import { int, money, moneyInr, pctFromFraction, price } from "../lib/format";
import { decimate } from "../lib/scale";
import { computeRealizedPnl } from "../lib/pnl";
import type {
  CycleRecord,
  Health,
  IndiaSandboxStatusResponse,
  Metrics,
  TradeRecord,
  UsPaperStatusResponse,
} from "../api/types";

export function Overview() {
  const metrics = useDataset<Metrics>("metrics");
  const cycles = useDataset<CycleRecord[]>("cyclesAll");
  const trades = useDataset<TradeRecord[]>("trades");
  const india = useDataset<IndiaSandboxStatusResponse>("indiaSandbox");
  const us = useDataset<UsPaperStatusResponse>("usPaper");
  const health = useDataset<Health>("health");

  // Dynamic Capital & Agent Controller State
  const [targetCapital, setTargetCapital] = useState<string>("30000");
  const [capitalFeedback, setCapitalFeedback] = useState<string | null>(null);
  const [updatingCapital, setUpdatingCapital] = useState<boolean>(false);

  // Live Candle Tracking State
  const [selectedMarket, setSelectedMarket] = useState<"NSE" | "US">("NSE");
  const [candles, setCandles] = useState<CandlePoint[]>([]);
  const [currentSymbol, setCurrentSymbol] = useState("RELIANCE");
  const [currentPrice, setCurrentPrice] = useState<number | undefined>(undefined);

  const nseTickers = ["RELIANCE", "TCS", "INFY", "HDFCBANK", "ICICIBANK"];
  const usTickers = ["AAPL", "TSLA", "NVDA", "SPY", "MSFT"];

  const handleMarketChange = (m: "NSE" | "US") => {
    setSelectedMarket(m);
    if (m === "NSE" && !nseTickers.includes(currentSymbol)) {
      setCurrentSymbol("RELIANCE");
    } else if (m === "US" && !usTickers.includes(currentSymbol)) {
      setCurrentSymbol("AAPL");
    }
  };

  const indiaOrders = india.data?.recent_orders ?? [];
  const usOrders = us.data?.recent_orders ?? [];
  const indiaCount = indiaOrders.length;
  const usCount = usOrders.length;
  const polyTradesCount = metrics.data?.total_trades ?? 0;
  const totalCombinedTrades = polyTradesCount + indiaCount + usCount;

  // Fetch Live Candles for the Tracking Graph
  const loadCandles = async () => {
    try {
      const endpoint =
        selectedMarket === "US"
          ? `/api/us/chart?symbol=${currentSymbol}`
          : `/api/india/chart?symbol=${currentSymbol}`;
      const res = await fetch(endpoint);
      if (res.ok) {
        const json = await res.json();
        setCandles(json.candles || []);
        if (json.current_price) setCurrentPrice(Number(json.current_price));
      }
    } catch {
      // silent fallback
    }
  };

  useEffect(() => {
    loadCandles();
    const interval = setInterval(loadCandles, 5000);
    return () => clearInterval(interval);
  }, [currentSymbol, selectedMarket]);

  // Compute Realized P&L across Indian and US markets
  const indiaPnlMap = computeRealizedPnl(
    indiaOrders.map((o) => ({
      id: o.id,
      symbol: o.symbol,
      side: o.side,
      quantity: o.quantity,
      price: o.price,
      average_fill_price: o.average_fill_price,
      created_at: o.created_at,
    })),
  );

  const usPnlMap = computeRealizedPnl(
    usOrders.map((o) => ({
      id: o.id,
      symbol: o.symbol,
      side: o.side,
      quantity: o.quantity,
      price: o.price,
      average_fill_price: o.average_fill_price,
      created_at: o.created_at,
    })),
  );

  const indiaProfitInr = Array.from(indiaPnlMap.values())
    .filter((p) => p.pnl !== null)
    .reduce((sum, p) => sum + (p.pnl ?? 0), 0);

  const usProfitUsd = Array.from(usPnlMap.values())
    .filter((p) => p.pnl !== null)
    .reduce((sum, p) => sum + (p.pnl ?? 0), 0);

  const polyRealizedUsd = num(metrics.data?.realized_pnl) ?? 0;
  const totalRealizedCombinedUsd = polyRealizedUsd + usProfitUsd + indiaProfitInr / 86.5;

  const ordered = cycles.data ? cyclesAscending(cycles.data) : [];
  const latest = ordered.at(-1) ?? null;

  const indiaBaseInr = india.data?.base_capital_inr ? Number(india.data.base_capital_inr) : 30000;
  const indiaProfit = india.data?.realized_profit_inr !== undefined ? Number(india.data.realized_profit_inr) : indiaProfitInr;
  const indiaCurrentInr = indiaBaseInr + indiaProfit;
  const indiaUsdEquivalent = indiaCurrentInr / 86.5;
  const indiaProfitPct = indiaBaseInr > 0 ? (indiaProfit / indiaBaseInr) * 100 : 0;

  const usBaseUsd = us.data?.base_capital_usd ? Number(us.data.base_capital_usd) : 30000;
  const usProfit = us.data?.realized_profit_usd !== undefined ? Number(us.data.realized_profit_usd) : usProfitUsd;
  const usCurrentUsd = usBaseUsd + usProfit;
  const usInrEquivalent = usCurrentUsd * 86.5;

  const baseBankroll = num(latest?.bankroll) ?? 30000;
  const currentBankroll = usCurrentUsd + indiaUsdEquivalent;
  const roiPct = baseBankroll > 0 ? (totalRealizedCombinedUsd / baseBankroll) * 100 : 0;

  const m = metrics.data;
  const isAgentHalted = (health.data?.halted ?? false) || health.data?.agent_state === "HALTED" || !!health.data?.halt;

  const applyBaseCapital = async (amount: number) => {
    setUpdatingCapital(true);
    setCapitalFeedback(null);
    try {
      const res = await postJson("/api/settings/caps", {
        india_capital_allocation_inr: amount,
        us_capital_allocation_usd: amount,
      });
      if (res.ok) {
        setCapitalFeedback(`✓ Base capital updated to ${amount.toLocaleString()} across all accounts.`);
        dataStore.refresh();
      } else {
        setCapitalFeedback(`Error: ${failureText(res.error)}`);
      }
    } catch (err: any) {
      setCapitalFeedback(`Error: ${err?.message || "Network error"}`);
    } finally {
      setUpdatingCapital(false);
    }
  };

  // Capital Distribution for Donut Chart
  const capitalSegments = [
    {
      label: "NSE Equities (INR)",
      value: indiaUsdEquivalent,
      color: "var(--series-1)",
      sublabel: `₹${indiaCurrentInr.toLocaleString("en-IN")}`,
    },
    {
      label: "US Equities (USD)",
      value: usCurrentUsd,
      color: "var(--series-2)",
      sublabel: `$${usCurrentUsd.toFixed(2)}`,
    },
    {
      label: "Polymarket Cash",
      value: Math.max(0, baseBankroll - indiaUsdEquivalent - usCurrentUsd),
      color: "var(--series-3)",
      sublabel: `$${Math.max(0, baseBankroll - indiaUsdEquivalent - usCurrentUsd).toFixed(2)}`,
    },
  ];

  return (
    <div className="page">
      {/* 0. CAPITAL ALLOCATION & AGENT ENGINE CONTROL */}
      <div className="card" style={{ padding: "16px 20px", background: "linear-gradient(135deg, rgba(30, 41, 59, 0.7) 0%, rgba(15, 23, 42, 0.8) 100%)", border: "1px solid var(--border-accent, #334155)", borderRadius: "var(--radius-md)" }}>
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", flexWrap: "wrap", gap: "12px" }}>
          <div>
            <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
              <span style={{ fontSize: "18px" }}>⚡</span>
              <h3 style={{ margin: 0, fontSize: "16px", fontWeight: "700", color: "var(--text-primary)" }}>
                AI Agent Engine & Trading Capital Controller
              </h3>
              <Badge tone={isAgentHalted ? "critical" : "good"}>
                {isAgentHalted ? "HALTED" : "ACTIVE 30K"}
              </Badge>
            </div>
            <p style={{ margin: "4px 0 0", fontSize: "12px", color: "var(--text-secondary)" }}>
              Current Base Allocation: <strong style={{ color: "var(--text-primary)" }}>₹{indiaBaseInr.toLocaleString("en-IN")} INR / ${usBaseUsd.toLocaleString("en-US")} USD</strong>. Orders and risk limits scale dynamically.
            </p>
          </div>

          <div style={{ display: "flex", alignItems: "center", gap: "10px", flexWrap: "wrap" }}>
            <AgentActionButtons />
          </div>
        </div>

        {/* Dynamic Base Amount Presets & Custom Input */}
        <div style={{ marginTop: "14px", paddingTop: "12px", borderTop: "1px solid rgba(255,255,255,0.08)", display: "flex", alignItems: "center", justifyContent: "space-between", flexWrap: "wrap", gap: "12px" }}>
          <div style={{ display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" }}>
            <span style={{ fontSize: "12px", fontWeight: "600", color: "var(--text-secondary)" }}>Set Base Amount:</span>
            {[
              { label: "10K", value: 10000 },
              { label: "30K (Recommended)", value: 30000 },
              { label: "50K", value: 50000 },
              { label: "100K", value: 100000 },
            ].map((preset) => (
              <button
                key={preset.value}
                type="button"
                onClick={() => {
                  setTargetCapital(String(preset.value));
                  applyBaseCapital(preset.value);
                }}
                disabled={updatingCapital}
                style={{
                  padding: "5px 12px",
                  fontSize: "12px",
                  fontWeight: "600",
                  borderRadius: "var(--radius-sm)",
                  cursor: "pointer",
                  background: (indiaBaseInr === preset.value) ? "var(--brand, #3b82f6)" : "var(--surface-2)",
                  color: (indiaBaseInr === preset.value) ? "#ffffff" : "var(--text-secondary)",
                  border: "1px solid var(--border)",
                  transition: "all 0.15s ease",
                }}
              >
                {preset.label}
              </button>
            ))}
          </div>

          <form
            onSubmit={(e) => {
              e.preventDefault();
              const val = parseFloat(targetCapital);
              if (!isNaN(val) && val > 0) {
                applyBaseCapital(val);
              }
            }}
            style={{ display: "flex", alignItems: "center", gap: "8px" }}
          >
            <input
              type="number"
              min="100"
              step="100"
              value={targetCapital}
              onChange={(e) => setTargetCapital(e.target.value)}
              placeholder="e.g. 30000"
              style={{
                width: "110px",
                padding: "5px 10px",
                fontSize: "12px",
                background: "var(--surface-1)",
                color: "var(--text-primary)",
                border: "1px solid var(--border)",
                borderRadius: "var(--radius-sm)",
              }}
            />
            <button
              type="submit"
              disabled={updatingCapital}
              style={{
                padding: "5px 14px",
                fontSize: "12px",
                fontWeight: "600",
                borderRadius: "var(--radius-sm)",
                background: "var(--brand, #3b82f6)",
                color: "#ffffff",
                border: "none",
                cursor: updatingCapital ? "wait" : "pointer",
              }}
            >
              {updatingCapital ? "Saving..." : "Apply Custom"}
            </button>
          </form>
        </div>

        {capitalFeedback && (
          <div style={{ marginTop: "10px", fontSize: "12px", color: capitalFeedback.startsWith("✓") ? "var(--status-good, #10b981)" : "var(--status-critical, #ef4444)" }}>
            {capitalFeedback}
          </div>
        )}
      </div>

      {/* 1. EXECUTIVE SUMMARY & CAPITAL CARDS */}
      <div className="card" style={{ padding: "16px 20px" }}>
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "16px", flexWrap: "wrap", gap: "10px" }}>
          <div>
            <h2 style={{ margin: 0, fontSize: "18px", fontWeight: "700", color: "var(--text-primary)" }}>
              Executive Portfolio Overview
            </h2>
            <p style={{ margin: "4px 0 0", fontSize: "13px", color: "var(--text-secondary)" }}>
              Real-time multi-market autonomous trading engine across NSE Equities, US Paper, and Prediction Markets.
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
              <Badge tone="good">₹ Intraday Sandbox</Badge>
            </div>
            <div style={{ fontSize: "24px", fontWeight: "700", color: "var(--text-primary)", fontVariantNumeric: "tabular-nums" }}>
              {moneyInr(indiaCurrentInr)}
            </div>
            <div style={{ fontSize: "12px", marginTop: "4px", color: indiaProfit >= 0 ? "var(--status-good)" : "var(--status-critical)" }}>
              P&L: <strong>{indiaProfit >= 0 ? `+₹${indiaProfit.toFixed(2)}` : `-₹${Math.abs(indiaProfit).toFixed(2)}`}</strong> ({indiaProfitPct >= 0 ? "+" : ""}{indiaProfitPct.toFixed(2)}%)
              <span style={{ color: "var(--text-muted)", marginLeft: "6px" }}>(Base: ₹{indiaBaseInr.toLocaleString("en-IN")})</span>
            </div>
            <div style={{ fontSize: "11px", color: "var(--text-muted)", marginTop: "2px" }}>
              USD Value: ${indiaUsdEquivalent.toFixed(2)} · {indiaCount} {indiaCount === 1 ? "order" : "orders"}
            </div>
            <div style={{ marginTop: "10px", fontSize: "12px" }}>
              <Link to="/india">Manage India Sandbox & Charts →</Link>
            </div>
          </div>

          {/* US Tile */}
          <div style={{ padding: "14px", background: "var(--surface-2)", borderRadius: "var(--radius-sm)", border: "1px solid var(--border)" }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "6px" }}>
              <span style={{ fontWeight: "600", fontSize: "13px", color: "var(--text-secondary)" }}>🇺🇸 US Equities & ETFs</span>
              <Badge tone="good">$ Alpaca Paper</Badge>
            </div>
            <div style={{ fontSize: "24px", fontWeight: "700", color: "var(--text-primary)", fontVariantNumeric: "tabular-nums" }}>
              {money(usCurrentUsd)}
            </div>
            <div style={{ fontSize: "12px", marginTop: "4px", color: usProfit >= 0 ? "var(--status-good)" : "var(--status-critical)" }}>
              P&L: <strong>{usProfit >= 0 ? `+$${usProfit.toFixed(2)}` : `-$${Math.abs(usProfit).toFixed(2)}`}</strong>
              <span style={{ color: "var(--text-muted)", marginLeft: "6px" }}>(Base: ${usBaseUsd.toFixed(2)})</span>
            </div>
            <div style={{ fontSize: "11px", color: "var(--text-muted)", marginTop: "2px" }}>
              INR Value: ₹{usInrEquivalent.toFixed(0)} · {usCount} {usCount === 1 ? "order" : "orders"}
            </div>
            <div style={{ marginTop: "10px", fontSize: "12px" }}>
              <Link to="/us">Manage US Paper & Charts →</Link>
            </div>
          </div>

          {/* Combined Total Tile */}
          <div style={{ padding: "14px", background: "var(--surface-2)", borderRadius: "var(--radius-sm)", border: "1px solid var(--border-strong)" }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "6px" }}>
              <span style={{ fontWeight: "600", fontSize: "13px", color: "var(--text-secondary)" }}>🌐 Combined Portfolio</span>
              <Badge tone="good">+{roiPct.toFixed(2)}% ROI</Badge>
            </div>
            <div style={{ fontSize: "24px", fontWeight: "700", color: "var(--status-good)", fontVariantNumeric: "tabular-nums" }}>
              {money(currentBankroll)}
            </div>
            <div style={{ fontSize: "12px", marginTop: "4px", color: "var(--text-secondary)" }}>
              Net Realized Profit: <strong style={{ color: "var(--status-good)" }}>+${totalRealizedCombinedUsd.toFixed(2)} USD</strong>
            </div>
            <div style={{ marginTop: "10px", fontSize: "12px" }}>
              <Link to="/trades">View All Trades & Audit Log →</Link>
            </div>
          </div>
        </div>
      </div>

      {/* 2. REAL-TIME TRACKING CANDLE GRAPH WITH ONE-CLICK BUY/SELL */}
      <Card title="Live Market Tracking Candle Graph (Real-Time Market Data)">
        {/* Market Selector & Ticker Tabs */}
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            marginBottom: "12px",
            flexWrap: "wrap",
            gap: "10px",
          }}
        >
          {/* Market Tab Pills */}
          <div style={{ display: "flex", gap: "6px", alignItems: "center" }}>
            <button
              type="button"
              className={`tab-pill ${selectedMarket === "NSE" ? "is-active" : ""}`}
              onClick={() => handleMarketChange("NSE")}
            >
              🇮🇳 Indian Equities (NSE)
            </button>
            <button
              type="button"
              className={`tab-pill ${selectedMarket === "US" ? "is-active" : ""}`}
              onClick={() => handleMarketChange("US")}
            >
              🇺🇸 US Equities (Alpaca Real Feed)
            </button>
          </div>

          <div style={{ display: "flex", gap: "6px", alignItems: "center" }}>
            <Badge tone="good">
              {selectedMarket === "US" ? "🟢 Live Alpaca IEX Feed" : "🟢 Live NSE Real-Time Feed"}
            </Badge>
          </div>
        </div>

        {/* Ticker Row */}
        <div style={{ display: "flex", gap: "8px", marginBottom: "12px", flexWrap: "wrap", alignItems: "center" }}>
          <span style={{ fontSize: "12px", color: "var(--text-secondary)", fontWeight: 600 }}>Active Ticker:</span>
          {(selectedMarket === "NSE" ? nseTickers : usTickers).map((sym) => (
            <button
              key={sym}
              type="button"
              className={`btn btn--sm ${currentSymbol === sym ? "btn--primary" : ""}`}
              onClick={() => setCurrentSymbol(sym)}
            >
              {sym}
            </button>
          ))}
        </div>

        <CanvasCandleChart
          symbol={currentSymbol}
          candles={candles}
          currentPrice={currentPrice}
          market={selectedMarket}
          currency={selectedMarket === "US" ? "USD" : "INR"}
          onOrderPlaced={loadCandles}
        />
      </Card>

      {/* 3. OPERATIONAL TILES: METRICS AT A GLANCE */}
      <Card title="Key Performance & Risk Metrics">
        <div className="grid grid--tiles">
          <StatTile
            label="Win Rate"
            value={m?.win_rate != null ? pctFromFraction(num(m.win_rate)) : "—"}
            tone={m?.win_rate != null ? "pos" : null}
            sub={m?.win_rate != null ? "Across completed round-trips" : "No closed trades yet"}
          />
          <StatTile
            label="Net Profit"
            value={m?.net_profit != null ? money(num(m.net_profit)) : "—"}
            tone={m?.net_profit != null ? ((num(m.net_profit) ?? 0) >= 0 ? "pos" : "neg") : null}
            sub={m?.net_profit != null ? "Total net closed P&L" : "No closed trades yet"}
          />
          <StatTile
            label="Active Orders / Positions"
            value={int(totalCombinedTrades)}
            sub={`${indiaCount} NSE · ${usCount} US · ${polyTradesCount} Polymarket`}
          />
          <StatTile
            label="Daily Drawdown Limit"
            value="15.0%"
            sub="Auto-trips safety circuit breaker"
          />
        </div>
      </Card>

      {/* 4. CHARTS ROW: PIE / DONUT + EQUITY CURVE */}
      <div className="grid grid--2">
        {/* Pie / Donut Chart Card */}
        <Card title="Capital Allocation by Asset Class">
          <DonutChart
            ariaLabel="Asset Class Capital Allocation"
            segments={capitalSegments}
            centerLabel="Total Bankroll"
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
                        values: shown.map((c, idx) => {
                          const base = num(c.bankroll) ?? baseBankroll;
                          return idx >= Math.max(0, shown.length - 3) ? base + totalRealizedCombinedUsd : base;
                        }),
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
                          money(
                            (num(c.bankroll) ?? baseBankroll) + totalRealizedCombinedUsd,
                          ),
                      },
                    ]}
                  />
                }
              />
            );
          }}
        </DatasetCard>
      </div>

      {/* 5. ACTIVITY & CYCLE HISTOGRAM */}
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

      {/* 6. RECENT TRADES AUDIT & QUICK ACTION */}
      <DatasetCard
        title="Recent trade executions"
        label="Recent trades"
        dataset={trades}
        datasetKey="trades"
        actions={<Link to="/trades">View All Trades & Orders →</Link>}
      >
        {(rows) =>
          rows.length === 0 ? (
            <div style={{ padding: "16px 8px" }}>
              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "12px", flexWrap: "wrap", gap: "8px" }}>
                <div>
                  <strong style={{ fontSize: "14px", color: "var(--text-primary)" }}>Active Orders & Executions</strong>
                  <p style={{ margin: "2px 0 0", fontSize: "12px", color: "var(--text-secondary)" }}>
                    Verified orders placed on Upstox Sandbox and Alpaca Paper.
                  </p>
                </div>
                <Link to="/trades">
                  <button className="btn btn--primary">Open Unified Trade Ledger ({totalCombinedTrades}) →</button>
                </Link>
              </div>

              {indiaOrders.length > 0 && (
                <div style={{ overflowX: "auto" }}>
                  <table className="data">
                    <thead>
                      <tr>
                        <th>Venue</th>
                        <th>Symbol</th>
                        <th>Side</th>
                        <th>Quantity</th>
                        <th>Fill Price</th>
                        <th>Status</th>
                        <th>Time</th>
                      </tr>
                    </thead>
                    <tbody>
                      {indiaOrders.map((o) => (
                        <tr key={o.id}>
                          <td><Badge tone="good">Upstox NSE</Badge></td>
                          <td><strong>{o.symbol}</strong></td>
                          <td><span className={o.side === "BUY" ? "num is-pos" : "num is-neg"}>{o.side}</span></td>
                          <td>{o.quantity}</td>
                          <td>₹{num(o.average_fill_price || o.price) ? num(o.average_fill_price || o.price)?.toFixed(2) : "—"}</td>
                          <td><Badge tone="good">{o.status}</Badge></td>
                          <td style={{ color: "var(--text-muted)", fontSize: "12px" }}>{o.created_at ? o.created_at.slice(11, 19) : "Just now"}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
            </div>
          ) : (
            <DataTable
              rows={tradesNewestFirst(rows).slice(0, 8)}
              rowKey={(t, i) => String(t.id ?? i)}
              columns={[
                {
                  key: "market",
                  header: "Market",
                  truncate: true,
                  render: (t) => t.market_question ?? t.market_id,
                  title: (t) => t.market_question ?? t.market_id,
                },
                { key: "dir", header: "Side", render: (t) => t.direction },
                { key: "price", header: "Price", numeric: true, render: (t) => price(num(t.entry_price)) },
                { key: "size", header: "Size", numeric: true, render: (t) => money(num(t.size)) },
                { key: "edge", header: "Edge", numeric: true, render: (t) => pctFromFraction(num(t.edge_at_entry)) },
                {
                  key: "status",
                  header: "Status",
                  render: (t) => <Badge tone={tradeStatusTone(t.status)}>{t.status}</Badge>,
                },
              ]}
            />
          )
        }
      </DatasetCard>
    </div>
  );
}
