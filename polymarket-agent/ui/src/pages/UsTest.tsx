import { useState, useEffect, useMemo } from "react";
import { Card } from "../components/ui/Card";
import { StatTile } from "../components/ui/StatTile";
import { Badge } from "../components/ui/Badge";
import { money, moneySigned, shares, etTime, pctFromFraction, DASH } from "../lib/format";
import { postJson } from "../api/client";
import { UsLiveChart } from "../components/us/UsLiveChart";
import { computeRealizedPnl } from "../lib/pnl";

interface UsInstrument {
  symbol: string;
  name: string;
  exchange: string;
  tick_size: string;
  lot_size: number;
  min_notional: string;
}

interface UsTradeCosts {
  sec_fee: string;
  finra_taf: string;
  commission: string;
  estimated_spread_cost: string;
  estimated_slippage_cost: string;
  total_regulatory_fees: string;
  total: string;
}

interface UsBacktestTrade {
  id: number;
  symbol: string;
  shares: string;
  entry_time: string;
  entry_price: string;
  entry_costs: UsTradeCosts;
  exit_time?: string;
  exit_price?: string;
  exit_costs?: UsTradeCosts;
  gross_pnl: string;
  total_costs: string;
  net_pnl: string;
  exit_reason: string;
}

interface UsBacktestResults {
  initial_capital: string;
  final_equity: string;
  net_profit: string;
  realized_pnl: string;
  unrealized_pnl: string;
  total_costs: string;
  total_sec_fees: string;
  total_finra_taf: string;
  total_commission: string;
  total_spread_slippage_cost: string;
  roi_pct: string;
  max_drawdown_pct: string;
  max_drawdown_usd: string;
  total_trades: number;
  winning_trades: number;
  losing_trades: number;
  win_rate_pct: string;
  profit_factor: string;
  net_expectancy_per_trade: string;
  benchmark_return_pct: string;
  insufficient_data_warning: boolean;
  trades: UsBacktestTrade[];
}

interface IntradayStatusData {
  timestamp: string;
  emergency_stop_active: boolean;
  fx_rate: {
    base: string;
    quote: string;
    rate: string;
    timestamp: string;
    source: string;
  };
  us: {
    market: string;
    currency: string;
    currency_symbol: string;
    timezone: string;
    local_time: string;
    session_state: string;
    entry_permitted: boolean;
    exit_due: boolean;
    session_close?: string;
    test_mode: string;
    live_orders_blocked: boolean;
    capital_allocation_usd?: string;
    max_exposure_pct?: string;
    max_daily_loss_usd?: string;
  };
}

function toNum(v: unknown): number {
  if (typeof v === "number") return isNaN(v) ? 0 : v;
  if (typeof v === "string") {
    const n = parseFloat(v);
    return isNaN(n) ? 0 : n;
  }
  return 0;
}

export function UsTest() {
  const [activeTab, setActiveTab] = useState<"live" | "paper" | "specs" | "backtest">("live");

  // Backtest form state
  const [initialCapital, setInitialCapital] = useState("100000");
  const [positionPct, setPositionPct] = useState("0.10");
  const [stopLossPct, setStopLossPct] = useState("0.015");
  const [takeProfitPct, setTakeProfitPct] = useState("0.030");
  const [useCustomCsv, setUseCustomCsv] = useState(false);
  const [customCsv, setCustomCsv] = useState("");
  const [backtestRunning, setBacktestRunning] = useState(false);
  const [backtestError, setBacktestError] = useState<string | null>(null);
  const [backtestResult, setBacktestResult] = useState<UsBacktestResults | null>(null);

  // Status & Watchlist state
  const [status, setStatus] = useState<IntradayStatusData | null>(null);
  const [watchlist, setWatchlist] = useState<UsInstrument[]>([]);

  // Paper order submit state ($5.00 Micro Capital)
  const [orderSymbol, setOrderSymbol] = useState("SIRI");
  const [orderSide, setOrderSide] = useState<"BUY" | "SELL">("BUY");
  const [orderQty, setOrderQty] = useState("1");
  const [orderPrice, setOrderPrice] = useState("4.80");
  const [orderSubmitting, setOrderSubmitting] = useState(false);
  const [orderSuccess, setOrderSuccess] = useState<string | null>(null);
  const [orderError, setOrderError] = useState<string | null>(null);
  const [baseCapitalUsd, setBaseCapitalUsd] = useState<number>(5.0);
  const [realizedProfitUsd, setRealizedProfitUsd] = useState<number>(0.0);
  const [openPositionsCostUsd, setOpenPositionsCostUsd] = useState<number>(0.0);
  const [availableCashUsd, setAvailableCashUsd] = useState<number>(5.0);
  const [totalEquityUsd, setTotalEquityUsd] = useState<number>(5.0);
  const [holdings, setHoldings] = useState<Record<string, number>>({});
  const [resetting, setResetting] = useState(false);
  const [resetMsg, setResetMsg] = useState<string | null>(null);
  const [alpacaAccount, setAlpacaAccount] = useState<{
    connected: boolean;
    account_number: string;
    status: string;
    cash: string;
    buying_power: string;
    portfolio_value: string;
  } | null>(null);
  const [recentOrders, setRecentOrders] = useState<Array<{
    id: string;
    client_order_id: string;
    venue_order_id?: string;
    symbol: string;
    side: string;
    order_type: string;
    quantity: number;
    price: string;
    status: string;
    fill_quantity: number;
    average_fill_price?: string;
    created_at: string;
  }>>([]);

  // Compute Realized P&L for US paper orders
  const usPnlMap = useMemo(() => {
    return computeRealizedPnl(
      recentOrders.map((o) => ({
        id: o.id,
        symbol: o.symbol,
        side: o.side,
        quantity: o.quantity,
        price: o.price,
        average_fill_price: o.average_fill_price,
        created_at: o.created_at,
      })),
    );
  }, [recentOrders]);

  useEffect(() => {
    fetchStatus();
    fetchWatchlist();
    fetchPaperStatus();
    const interval = setInterval(() => {
      fetchStatus();
      fetchPaperStatus();
    }, 15000);
    return () => clearInterval(interval);
  }, []);

  async function fetchPaperStatus() {
    try {
      const res = await fetch("/api/us/paper/status");
      if (res.ok) {
        const data = await res.json();
        setRecentOrders(data.recent_orders || []);
        if (data.alpaca_account) setAlpacaAccount(data.alpaca_account);
        if (data.base_capital_usd !== undefined) setBaseCapitalUsd(toNum(data.base_capital_usd));
        if (data.realized_profit_usd !== undefined) setRealizedProfitUsd(toNum(data.realized_profit_usd));
        if (data.open_positions_cost_usd !== undefined) setOpenPositionsCostUsd(toNum(data.open_positions_cost_usd));
        if (data.available_cash_usd !== undefined) setAvailableCashUsd(toNum(data.available_cash_usd));
        if (data.total_equity_usd !== undefined) setTotalEquityUsd(toNum(data.total_equity_usd));
        if (data.holdings) setHoldings(data.holdings);
      }
    } catch {
      // Ignore
    }
  }

  async function handleResetUsMarket() {
    if (!window.confirm("Are you sure you want to reset the US Paper Market to strictly $5.00 initial capital? All previous US paper orders will be deleted.")) {
      return;
    }
    setResetting(true);
    setResetMsg(null);
    setOrderError(null);
    setOrderSuccess(null);
    try {
      const res = await postJson<{ status: string; message: string }>("/api/us/paper/reset", {});
      if (res.ok) {
        setResetMsg("US Market successfully reset to $5.00 USD initial capital! Orders cleared.");
        fetchPaperStatus();
        fetchStatus();
      } else {
        setOrderError(res.error.kind === "http" ? res.error.message : "Failed to reset US paper market");
      }
    } catch (err: unknown) {
      setOrderError(err instanceof Error ? err.message : String(err));
    } finally {
      setResetting(false);
    }
  }

  async function handleOrderSubmit(e: React.FormEvent) {
    e.preventDefault();
    setOrderSubmitting(true);
    setOrderSuccess(null);
    setOrderError(null);
    try {
      const payload = {
        symbol: orderSymbol,
        side: orderSide,
        order_type: "LIMIT",
        quantity: parseInt(orderQty, 10),
        price: orderPrice,
      };
      const res = await postJson<{ status: string; order: unknown; disclaimer: string }>(
        "/api/us/paper/order",
        payload
      );
      if (res.ok) {
        setOrderSuccess(`US Paper Order placed successfully! Symbol: ${orderSymbol}, Qty: ${orderQty}`);
        fetchPaperStatus();
      } else {
        setOrderError(res.error.kind === "http" ? res.error.message : "US Paper order failed");
      }
    } catch (err: unknown) {
      setOrderError(err instanceof Error ? err.message : String(err));
    } finally {
      setOrderSubmitting(false);
    }
  }

  async function executeQuickOrder(sym: string, side: "BUY" | "SELL", qty: number, orderType: "MARKET" | "LIMIT" = "MARKET", priceStr?: string) {
    setOrderSubmitting(true);
    setOrderSuccess(null);
    setOrderError(null);
    try {
      const payload: Record<string, unknown> = {
        symbol: sym,
        side,
        order_type: orderType,
        quantity: qty,
      };
      if (orderType === "LIMIT" && priceStr) {
        payload.price = priceStr;
      }
      const res = await postJson<{ status: string; order: unknown; disclaimer: string }>(
        "/api/us/paper/order",
        payload
      );
      if (res.ok) {
        setOrderSuccess(`🏛️ Real Market ${side} order submitted to Alpaca for ${qty}x ${sym}!`);
        fetchPaperStatus();
      } else {
        setOrderError(res.error.kind === "http" ? res.error.message : `Order failed`);
      }
    } catch (err: unknown) {
      setOrderError(err instanceof Error ? err.message : String(err));
    } finally {
      setOrderSubmitting(false);
    }
  }

  async function fetchStatus() {
    try {
      const res = await fetch("/api/intraday/status");
      if (res.ok) {
        const data = await res.json();
        setStatus(data);
      }
    } catch {
      // Ignore network errors in demo/offline
    }
  }

  async function fetchWatchlist() {
    try {
      const res = await fetch("/api/us/watchlist");
      if (res.ok) {
        const data = await res.json();
        setWatchlist(data);
      }
    } catch {
      // Fallback
    }
  }

  async function runBacktest() {
    setBacktestRunning(true);
    setBacktestError(null);
    try {
      const payload: Record<string, unknown> = {
        name: `US Backtest (${new Date().toLocaleTimeString()})`,
        initial_capital: parseFloat(initialCapital),
        risk_pct_per_trade: parseFloat(positionPct),
        stop_loss_pct: parseFloat(stopLossPct),
        take_profit_pct: parseFloat(takeProfitPct),
      };

      if (useCustomCsv && customCsv.trim()) {
        payload.csv_content = customCsv;
      } else {
        payload.csv_path = "data/us_sample_data.csv";
      }

      const res = await postJson<UsBacktestResults>("/api/us/backtest/run", payload);
      if (res.ok) {
        setBacktestResult(res.value);
      } else {
        setBacktestError(res.error.kind === "http" ? res.error.message : "Backtest execution failed");
      }
    } catch (err: unknown) {
      setBacktestError(err instanceof Error ? err.message : String(err));
    } finally {
      setBacktestRunning(false);
    }
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "20px" }}>
      {/* Top Header Bar Matching Reference Terminal Design */}
      <div style={{ background: "var(--card-bg, #1a1a1e)", border: "1px solid var(--border-color, #2a2a30)", borderRadius: "10px", padding: "16px 20px", display: "flex", justifyContent: "space-between", alignItems: "center", flexWrap: "wrap", gap: "12px" }}>
        <div style={{ display: "flex", alignItems: "center", gap: "10px", flexWrap: "wrap" }}>
          <span style={{ fontSize: "18px", fontWeight: "800", color: "#fff", display: "flex", alignItems: "center", gap: "8px" }}>
            <span>🇺🇸</span> US Stock Market (NYSE & NASDAQ Equities & ETFs)
          </span>
          <Badge tone="good">INTRADAY REGULAR</Badge>
          <Badge tone="neutral">USD ($)</Badge>
          <Badge tone="neutral">America/New_York (09:30 - 16:00 ET)</Badge>
          <Badge tone={status?.us.session_state === "OPEN" ? "good" : "neutral"}>
            {status?.us.session_state === "OPEN" ? "SESSION OPEN" : "REGULAR SESSION"}
          </Badge>
          <Badge tone="warning">ALPACA PAPER SIMULATION</Badge>
          {status?.fx_rate && (
            <Badge tone="neutral">1 USD = ₹{(parseFloat(String(status.fx_rate.rate)) || 86.5).toFixed(2)}</Badge>
          )}
        </div>

        {/* Tab Navigation */}
        <div style={{ display: "flex", gap: "6px", flexWrap: "wrap" }}>
          <button
            className={`btn ${activeTab === "live" ? "btn--primary" : "btn--secondary"}`}
            onClick={() => setActiveTab("live")}
            style={{ fontSize: "0.825rem", padding: "0.35rem 0.75rem", display: "flex", alignItems: "center", gap: "0.4rem" }}
          >
            <span style={{ display: "inline-block", width: "8px", height: "8px", borderRadius: "50%", background: "#10b981" }} />
            Live Market Chart & Trades
          </button>
          <button
            className={`btn ${activeTab === "paper" ? "btn--primary" : "btn--secondary"}`}
            onClick={() => setActiveTab("paper")}
            style={{ fontSize: "0.825rem", padding: "0.35rem 0.75rem" }}
          >
            Alpaca Paper Fills
          </button>
          <button
            className={`btn ${activeTab === "specs" ? "btn--primary" : "btn--secondary"}`}
            onClick={() => setActiveTab("specs")}
            style={{ fontSize: "0.825rem", padding: "0.35rem 0.75rem" }}
          >
            US Watchlist & Rules
          </button>
          <button
            className={`btn ${activeTab === "backtest" ? "btn--primary" : "btn--secondary"}`}
            onClick={() => setActiveTab("backtest")}
            style={{ fontSize: "0.825rem", padding: "0.35rem 0.75rem" }}
          >
            CSV Backtester
          </button>
        </div>
      </div>

      {/* Tab 0: Live Market Graph & Detailed Trade Ledger */}
      {activeTab === "live" && (
        <UsLiveChart onOrderPlaced={fetchStatus} />
      )}

      {/* TAB 1: Historical Backtesting */}
      {activeTab === "backtest" && (
        <div style={{ display: "flex", flexDirection: "column", gap: "20px" }}>
          <Card title="Backtest Configuration (Zero Look-Ahead Bias & Outside-Bar SL/TP)">
            <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(200px, 1fr))", gap: "16px", marginBottom: "16px" }}>
              <div>
                <label style={{ display: "block", fontSize: "12px", color: "var(--text-secondary)", marginBottom: "4px" }}>
                  Initial Testing Capital (USD $)
                </label>
                <input
                  type="number"
                  value={initialCapital}
                  onChange={(e) => setInitialCapital(e.target.value)}
                  style={{ width: "100%", padding: "8px", background: "var(--bg-input)", border: "1px solid var(--border-color)", borderRadius: "4px", color: "var(--text-primary)" }}
                />
              </div>

              <div>
                <label style={{ display: "block", fontSize: "12px", color: "var(--text-secondary)", marginBottom: "4px" }}>
                  Max Position Size (% of Equity)
                </label>
                <input
                  type="number"
                  step="0.01"
                  value={positionPct}
                  onChange={(e) => setPositionPct(e.target.value)}
                  style={{ width: "100%", padding: "8px", background: "var(--bg-input)", border: "1px solid var(--border-color)", borderRadius: "4px", color: "var(--text-primary)" }}
                />
              </div>

              <div>
                <label style={{ display: "block", fontSize: "12px", color: "var(--text-secondary)", marginBottom: "4px" }}>
                  Stop-Loss Threshold (% below entry)
                </label>
                <input
                  type="number"
                  step="0.005"
                  value={stopLossPct}
                  onChange={(e) => setStopLossPct(e.target.value)}
                  style={{ width: "100%", padding: "8px", background: "var(--bg-input)", border: "1px solid var(--border-color)", borderRadius: "4px", color: "var(--text-primary)" }}
                />
              </div>

              <div>
                <label style={{ display: "block", fontSize: "12px", color: "var(--text-secondary)", marginBottom: "4px" }}>
                  Take-Profit Target (% above entry)
                </label>
                <input
                  type="number"
                  step="0.005"
                  value={takeProfitPct}
                  onChange={(e) => setTakeProfitPct(e.target.value)}
                  style={{ width: "100%", padding: "8px", background: "var(--bg-input)", border: "1px solid var(--border-color)", borderRadius: "4px", color: "var(--text-primary)" }}
                />
              </div>
            </div>

            {/* Custom CSV Option */}
            <div style={{ marginBottom: "16px" }}>
              <label style={{ display: "flex", alignItems: "center", gap: "8px", cursor: "pointer", fontSize: "13px" }}>
                <input
                  type="checkbox"
                  checked={useCustomCsv}
                  onChange={(e) => setUseCustomCsv(e.target.checked)}
                />
                Use Custom OHLCV CSV Data (Defaults to synthetic demonstration dataset data/us_sample_data.csv)
              </label>

              {useCustomCsv && (
                <textarea
                  rows={6}
                  placeholder="Paste CSV: timestamp,symbol,open,high,low,close,volume (America/New_York timestamps)"
                  value={customCsv}
                  onChange={(e) => setCustomCsv(e.target.value)}
                  style={{ width: "100%", marginTop: "8px", padding: "8px", background: "var(--bg-input)", border: "1px solid var(--border-color)", borderRadius: "4px", color: "var(--text-primary)", fontFamily: "monospace", fontSize: "12px" }}
                />
              )}
            </div>

            <button
              onClick={runBacktest}
              disabled={backtestRunning}
              style={{
                padding: "10px 20px",
                backgroundColor: "var(--accent-primary)",
                color: "#fff",
                border: "none",
                borderRadius: "6px",
                cursor: "pointer",
                fontWeight: "600",
              }}
            >
              {backtestRunning ? "Running Simulation..." : "Run US Backtest Simulation"}
            </button>

            {backtestError && (
              <div style={{ marginTop: "12px", padding: "8px 12px", background: "rgba(239, 68, 68, 0.1)", border: "1px solid var(--accent-negative)", borderRadius: "4px", color: "var(--accent-negative)", fontSize: "13px" }}>
                Error: {backtestError}
              </div>
            )}
          </Card>

          {/* Backtest Results */}
          {backtestResult && (
            <div style={{ display: "flex", flexDirection: "column", gap: "20px" }}>
              <Card title="Simulation Results Summary">
                <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))", gap: "16px", marginBottom: "20px" }}>
                  <StatTile label="Initial Capital" value={money(parseFloat(backtestResult.initial_capital))} />
                  <StatTile label="Final Equity" value={money(parseFloat(backtestResult.final_equity))} />
                  <StatTile label="Net Profit (USD)" value={moneySigned(parseFloat(backtestResult.net_profit))} />
                  <StatTile label="ROI %" value={pctFromFraction(parseFloat(backtestResult.roi_pct))} />
                  <StatTile label="Win Rate" value={`${backtestResult.win_rate_pct}% (${backtestResult.winning_trades}W / ${backtestResult.losing_trades}L)`} />
                  <StatTile label="Max Drawdown" value={`${pctFromFraction(parseFloat(backtestResult.max_drawdown_pct))} (${money(parseFloat(backtestResult.max_drawdown_usd))})`} />
                  <StatTile label="SPY Benchmark Return" value={pctFromFraction(parseFloat(backtestResult.benchmark_return_pct))} />
                  <StatTile label="Total Regulatory Costs" value={money(parseFloat(backtestResult.total_costs))} />
                </div>

                {/* Regulatory Costs Breakdown */}
                <div style={{ padding: "12px", background: "var(--bg-input)", borderRadius: "6px", fontSize: "13px", display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(160px, 1fr))", gap: "12px" }}>
                  <div><strong>SEC Section 31 Fees:</strong> {money(parseFloat(backtestResult.total_sec_fees))}</div>
                  <div><strong>FINRA TAF Charges:</strong> {money(parseFloat(backtestResult.total_finra_taf))}</div>
                  <div><strong>Broker Commission:</strong> {money(parseFloat(backtestResult.total_commission))}</div>
                  <div><strong>Spread & Slippage:</strong> {money(parseFloat(backtestResult.total_spread_slippage_cost))}</div>
                </div>
              </Card>

              {/* Individual Trades Table */}
              <Card title={`Simulated Trades (${backtestResult.trades.length})`}>
                <div style={{ overflowX: "auto" }}>
                  <table style={{ width: "100%", borderCollapse: "collapse", fontSize: "13px" }}>
                    <thead>
                      <tr style={{ borderBottom: "1px solid var(--border-color)", textAlign: "left" }}>
                        <th style={{ padding: "8px" }}>#</th>
                        <th style={{ padding: "8px" }}>Symbol</th>
                        <th style={{ padding: "8px" }}>Shares</th>
                        <th style={{ padding: "8px" }}>Entry Time (ET)</th>
                        <th style={{ padding: "8px" }}>Entry Price</th>
                        <th style={{ padding: "8px" }}>Exit Time (ET)</th>
                        <th style={{ padding: "8px" }}>Exit Price</th>
                        <th style={{ padding: "8px" }}>Gross P&L</th>
                        <th style={{ padding: "8px" }}>Fees</th>
                        <th style={{ padding: "8px" }}>Net P&L</th>
                        <th style={{ padding: "8px" }}>Exit Reason</th>
                      </tr>
                    </thead>
                    <tbody>
                      {backtestResult.trades.map((t) => {
                        const net = parseFloat(t.net_pnl);
                        const isWin = net > 0;
                        return (
                          <tr key={t.id} style={{ borderBottom: "1px solid var(--border-color)" }}>
                            <td style={{ padding: "8px" }}>{t.id}</td>
                            <td style={{ padding: "8px", fontWeight: "600" }}>{t.symbol}</td>
                            <td style={{ padding: "8px" }}>{shares(parseFloat(t.shares))}</td>
                            <td style={{ padding: "8px" }}>{etTime(new Date(t.entry_time))}</td>
                            <td style={{ padding: "8px" }}>{money(parseFloat(t.entry_price))}</td>
                            <td style={{ padding: "8px" }}>{t.exit_time ? etTime(new Date(t.exit_time)) : DASH}</td>
                            <td style={{ padding: "8px" }}>{t.exit_price ? money(parseFloat(t.exit_price)) : DASH}</td>
                            <td style={{ padding: "8px" }}>{moneySigned(parseFloat(t.gross_pnl))}</td>
                            <td style={{ padding: "8px", color: "var(--accent-negative)" }}>{money(parseFloat(t.total_costs))}</td>
                            <td style={{ padding: "8px", fontWeight: "600", color: isWin ? "var(--accent-positive)" : "var(--accent-negative)" }}>
                              {moneySigned(net)}
                            </td>
                            <td style={{ padding: "8px" }}>
                              <Badge tone={t.exit_reason === "TAKE_PROFIT" ? "good" : t.exit_reason === "STOP_LOSS" ? "critical" : "neutral"}>
                                {t.exit_reason}
                              </Badge>
                            </td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              </Card>
            </div>
          )}
        </div>
      )}

      {/* TAB 2: Alpaca Paper Trading Integration */}
      {activeTab === "paper" && (
        <div style={{ display: "flex", flexDirection: "column", gap: "20px" }}>
          {/* Strict Isolation & $5.00 Seed Fund Banner */}
          <div style={{
            background: "linear-gradient(135deg, rgba(30, 41, 59, 0.9) 0%, rgba(15, 23, 42, 0.95) 100%)",
            border: "1px solid rgba(59, 130, 246, 0.3)",
            borderRadius: "12px",
            padding: "20px",
            boxShadow: "0 4px 20px rgba(0, 0, 0, 0.25)"
          }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "16px", flexWrap: "wrap", gap: "12px" }}>
              <div>
                <div style={{ display: "flex", alignItems: "center", gap: "10px" }}>
                  <span style={{ fontSize: "20px" }}>🇺🇸</span>
                  <h3 style={{ margin: 0, fontSize: "18px", fontWeight: "800", color: "#fff" }}>
                    US Market Paper Trading Pool — $5.00 Seed Capital
                  </h3>
                  <Badge tone="good">Strict Isolation Active</Badge>
                </div>
                <p style={{ margin: "6px 0 0", fontSize: "13px", color: "#94a3b8" }}>
                  Trades start with strictly <strong>$5.00 USD</strong>. Profits from closed trades compound directly back into trading cash. <strong>Indian Market funds (₹1,000 INR) are completely isolated and never used.</strong>
                </p>
              </div>

              <button
                onClick={handleResetUsMarket}
                disabled={resetting}
                style={{
                  padding: "8px 16px",
                  background: "linear-gradient(135deg, #ef4444 0%, #dc2626 100%)",
                  color: "#fff",
                  border: "none",
                  borderRadius: "6px",
                  fontWeight: "700",
                  fontSize: "12px",
                  cursor: resetting ? "not-allowed" : "pointer",
                  boxShadow: "0 2px 8px rgba(239, 68, 68, 0.3)",
                  display: "flex",
                  alignItems: "center",
                  gap: "6px"
                }}
              >
                🔄 {resetting ? "Resetting..." : "Reset to $5.00 Base"}
              </button>
            </div>

            {resetMsg && (
              <div style={{ marginBottom: "16px", padding: "10px 14px", background: "rgba(34, 197, 94, 0.15)", border: "1px solid rgba(34, 197, 94, 0.3)", borderRadius: "6px", color: "#4ade80", fontSize: "13px" }}>
                ✓ {resetMsg}
              </div>
            )}

            {alpacaAccount && (
              <div style={{ marginBottom: "16px", padding: "12px 16px", background: "rgba(16, 185, 129, 0.12)", border: "1px solid rgba(16, 185, 129, 0.35)", borderRadius: "8px", display: "flex", alignItems: "center", justifyContent: "space-between", flexWrap: "wrap", gap: "10px" }}>
                <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
                  <span style={{ fontSize: "16px" }}>🟢</span>
                  <strong style={{ color: "#34d399", fontSize: "13px" }}>Connected to Alpaca Paper Account:</strong>
                  <span style={{ color: "#fff", fontWeight: "700", fontFamily: "monospace", fontSize: "13px" }}>{alpacaAccount.account_number}</span>
                  <Badge tone="good">{alpacaAccount.status}</Badge>
                </div>
                <div style={{ fontSize: "12px", color: "#94a3b8", display: "flex", gap: "16px" }}>
                  <span>Alpaca Cash: <strong style={{ color: "#34d399" }}>${alpacaAccount.cash}</strong></span>
                  <span>Buying Power: <strong style={{ color: "#38bdf8" }}>${alpacaAccount.buying_power}</strong></span>
                </div>
              </div>
            )}

            {/* Micro Capital Stat Tiles */}
            <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))", gap: "14px" }}>
              <div style={{ padding: "14px", background: "rgba(15, 23, 42, 0.7)", borderRadius: "8px", border: "1px solid rgba(255, 255, 255, 0.08)" }}>
                <div style={{ fontSize: "11px", color: "#94a3b8", fontWeight: "600", textTransform: "uppercase" }}>Initial Seed Capital</div>
                <div style={{ fontSize: "20px", fontWeight: "800", color: "#fff", marginTop: "4px" }}>${toNum(baseCapitalUsd).toFixed(2)} USD</div>
                <div style={{ fontSize: "11px", color: "#64748b", marginTop: "2px" }}>Fixed base allocation</div>
              </div>

              <div style={{ padding: "14px", background: "rgba(15, 23, 42, 0.7)", borderRadius: "8px", border: "1px solid rgba(255, 255, 255, 0.08)" }}>
                <div style={{ fontSize: "11px", color: "#94a3b8", fontWeight: "600", textTransform: "uppercase" }}>Realized Profit (Compounded)</div>
                <div style={{ fontSize: "20px", fontWeight: "800", color: toNum(realizedProfitUsd) >= 0 ? "#4ade80" : "#f87171", marginTop: "4px" }}>
                  {toNum(realizedProfitUsd) >= 0 ? `+$${toNum(realizedProfitUsd).toFixed(2)}` : `-$${Math.abs(toNum(realizedProfitUsd)).toFixed(2)}`} USD
                </div>
                <div style={{ fontSize: "11px", color: "#64748b", marginTop: "2px" }}>Available to re-trade</div>
              </div>

              <div style={{ padding: "14px", background: "rgba(15, 23, 42, 0.7)", borderRadius: "8px", border: "1px solid rgba(59, 130, 246, 0.3)" }}>
                <div style={{ fontSize: "11px", color: "#60a5fa", fontWeight: "700", textTransform: "uppercase" }}>Available Trading Cash</div>
                <div style={{ fontSize: "22px", fontWeight: "800", color: "#38bdf8", marginTop: "4px" }}>${toNum(availableCashUsd).toFixed(2)} USD</div>
                <div style={{ fontSize: "11px", color: "#93c5fd", marginTop: "2px" }}>Budget limit for BUY orders</div>
              </div>

              <div style={{ padding: "14px", background: "rgba(15, 23, 42, 0.7)", borderRadius: "8px", border: "1px solid rgba(255, 255, 255, 0.08)" }}>
                <div style={{ fontSize: "11px", color: "#94a3b8", fontWeight: "600", textTransform: "uppercase" }}>Tied Up in Open Shares</div>
                <div style={{ fontSize: "20px", fontWeight: "800", color: "#facc15", marginTop: "4px" }}>${toNum(openPositionsCostUsd).toFixed(2)} USD</div>
                <div style={{ fontSize: "11px", color: "#64748b", marginTop: "2px" }}>
                  {Object.entries(holdings).filter(([_, q]) => Number(q) > 0).map(([s, q]) => `${q}x ${s}`).join(", ") || "No open positions"}
                </div>
              </div>

              <div style={{ padding: "14px", background: "rgba(15, 23, 42, 0.7)", borderRadius: "8px", border: "1px solid rgba(34, 197, 94, 0.2)" }}>
                <div style={{ fontSize: "11px", color: "#4ade80", fontWeight: "600", textTransform: "uppercase" }}>Total US Equity</div>
                <div style={{ fontSize: "20px", fontWeight: "800", color: "#4ade80", marginTop: "4px" }}>${toNum(totalEquityUsd).toFixed(2)} USD</div>
                <div style={{ fontSize: "11px", color: "#64748b", marginTop: "2px" }}>Cash + Open Positions</div>
              </div>
            </div>
          </div>

          {/* Interactive US Paper Order Placement Form */}
          <Card title="Place US Paper Order (Real-World Market Execution)">
            {/* 🏛️ Real-World Live Market Order Panel */}
            <div style={{
              background: "linear-gradient(135deg, rgba(30, 41, 59, 0.85) 0%, rgba(15, 23, 42, 0.95) 100%)",
              border: "1px solid rgba(59, 130, 246, 0.4)",
              borderRadius: "10px",
              padding: "16px",
              marginBottom: "18px",
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
              flexWrap: "wrap",
              gap: "14px",
              boxShadow: "0 4px 14px rgba(0, 0, 0, 0.25)"
            }}>
              <div>
                <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
                  <span style={{ fontSize: "18px" }}>🏛️</span>
                  <strong style={{ color: "#38bdf8", fontSize: "15px" }}>Real-World Live Market Orders (Alpaca Exchange Direct)</strong>
                  <Badge tone="good">Live Market Fills</Badge>
                </div>
                <div style={{ fontSize: "12px", color: "#94a3b8", marginTop: "4px" }}>
                  Real live prices direct from Wall Street. Fills only occur when actual trades print on NASDAQ/NYSE.
                </div>
              </div>

              <div style={{ display: "flex", gap: "10px", flexWrap: "wrap" }}>
                <button
                  type="button"
                  disabled={orderSubmitting || toNum(availableCashUsd) < 1.40}
                  onClick={() => executeQuickOrder("PLUG", "BUY", 1, "MARKET")}
                  style={{
                    padding: "10px 16px",
                    background: toNum(availableCashUsd) >= 1.40
                      ? "linear-gradient(135deg, #10b981 0%, #059669 100%)"
                      : "#334155",
                    color: "#fff",
                    border: "none",
                    borderRadius: "6px",
                    fontWeight: "800",
                    fontSize: "13px",
                    cursor: (orderSubmitting || toNum(availableCashUsd) < 1.40) ? "not-allowed" : "pointer",
                    boxShadow: toNum(availableCashUsd) >= 1.40 ? "0 2px 10px rgba(16, 185, 129, 0.35)" : "none",
                    display: "flex",
                    alignItems: "center",
                    gap: "6px"
                  }}
                >
                  🟢 Buy 1x PLUG @ Market (~$1.40)
                </button>

                <button
                  type="button"
                  disabled={orderSubmitting || toNum(availableCashUsd) < 3.27}
                  onClick={() => executeQuickOrder("NIO", "BUY", 1, "MARKET")}
                  style={{
                    padding: "10px 16px",
                    background: toNum(availableCashUsd) >= 3.27
                      ? "linear-gradient(135deg, #3b82f6 0%, #2563eb 100%)"
                      : "#334155",
                    color: "#fff",
                    border: "none",
                    borderRadius: "6px",
                    fontWeight: "800",
                    fontSize: "13px",
                    cursor: (orderSubmitting || toNum(availableCashUsd) < 3.27) ? "not-allowed" : "pointer",
                    boxShadow: toNum(availableCashUsd) >= 3.27 ? "0 2px 10px rgba(59, 130, 246, 0.35)" : "none",
                    display: "flex",
                    alignItems: "center",
                    gap: "6px"
                  }}
                >
                  🟢 Buy 1x NIO @ Market (~$3.27)
                </button>

                {(holdings["PLUG"] || 0) > 0 && (
                  <button
                    type="button"
                    disabled={orderSubmitting}
                    onClick={() => executeQuickOrder("PLUG", "SELL", 1, "MARKET")}
                    style={{
                      padding: "10px 16px",
                      background: "linear-gradient(135deg, #f59e0b 0%, #d97706 100%)",
                      color: "#fff",
                      border: "none",
                      borderRadius: "6px",
                      fontWeight: "800",
                      fontSize: "13px",
                      cursor: "pointer",
                      boxShadow: "0 2px 10px rgba(245, 158, 11, 0.35)",
                      display: "flex",
                      alignItems: "center",
                      gap: "6px"
                    }}
                  >
                    🟠 Sell 1x PLUG @ Market (~$1.39)
                  </button>
                )}
              </div>
            </div>

            {/* Quick Stock Selector for $5.00 budget */}
            <div style={{ display: "flex", alignItems: "center", gap: "8px", marginBottom: "16px", flexWrap: "wrap" }}>
              <span style={{ fontSize: "12px", color: "var(--text-secondary)", fontWeight: "600" }}>
                Verified Real-Time Stocks:
              </span>
              <button
                type="button"
                onClick={() => {
                  setOrderSymbol("SIRI");
                  setOrderPrice("4.80");
                  setOrderQty("1");
                }}
                style={{
                  padding: "4px 10px",
                  background: orderSymbol === "SIRI" ? "var(--accent-primary)" : "var(--bg-input)",
                  color: "#fff",
                  border: "1px solid var(--border-color)",
                  borderRadius: "4px",
                  fontSize: "12px",
                  cursor: "pointer",
                  fontWeight: "600"
                }}
              >
                SIRI (Sirius XM @ $4.80)
              </button>
              <button
                type="button"
                onClick={() => {
                  setOrderSymbol("NOK");
                  setOrderPrice("4.25");
                  setOrderQty("1");
                }}
                style={{
                  padding: "4px 10px",
                  background: orderSymbol === "NOK" ? "var(--accent-primary)" : "var(--bg-input)",
                  color: "#fff",
                  border: "1px solid var(--border-color)",
                  borderRadius: "4px",
                  fontSize: "12px",
                  cursor: "pointer",
                  fontWeight: "600"
                }}
              >
                NOK (Nokia ADR @ $4.25)
              </button>
            </div>

            <form onSubmit={handleOrderSubmit} style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))", gap: "16px", alignItems: "end" }}>
              <div>
                <label style={{ display: "block", fontSize: "12px", color: "var(--text-secondary)", marginBottom: "4px" }}>Symbol</label>
                <select
                  value={orderSymbol}
                  onChange={(e) => {
                    const sym = e.target.value;
                    setOrderSymbol(sym);
                    if (sym === "SIRI") setOrderPrice("4.80");
                    else if (sym === "NOK") setOrderPrice("4.25");
                  }}
                  style={{ width: "100%", padding: "8px", background: "var(--bg-input)", border: "1px solid var(--border-color)", borderRadius: "4px", color: "var(--text-primary)" }}
                >
                  <option value="SIRI">SIRI - Sirius XM Holdings ($4.85)</option>
                  <option value="NOK">NOK - Nokia Corp ADR ($4.25)</option>
                  {watchlist.filter(w => w.symbol !== "SIRI" && w.symbol !== "NOK").map((inst) => (
                    <option key={inst.symbol} value={inst.symbol}>{inst.symbol} ({inst.name})</option>
                  ))}
                  <option value="SPY">SPY (S&P 500 ETF)</option>
                  <option value="QQQ">QQQ (Invesco Nasdaq ETF)</option>
                  <option value="AAPL">AAPL (Apple Inc)</option>
                  <option value="MSFT">MSFT (Microsoft Corp)</option>
                  <option value="NVDA">NVDA (NVIDIA Corp)</option>
                </select>
              </div>

              <div>
                <label style={{ display: "block", fontSize: "12px", color: "var(--text-secondary)", marginBottom: "4px" }}>Side</label>
                <select
                  value={orderSide}
                  onChange={(e) => setOrderSide(e.target.value as "BUY" | "SELL")}
                  style={{ width: "100%", padding: "8px", background: "var(--bg-input)", border: "1px solid var(--border-color)", borderRadius: "4px", color: "var(--text-primary)" }}
                >
                  <option value="BUY">BUY</option>
                  <option value="SELL">SELL</option>
                </select>
              </div>

              <div>
                <label style={{ display: "block", fontSize: "12px", color: "var(--text-secondary)", marginBottom: "4px" }}>Quantity (Shares)</label>
                <input
                  type="number"
                  min="1"
                  step="1"
                  value={orderQty}
                  onChange={(e) => setOrderQty(e.target.value)}
                  style={{ width: "100%", padding: "8px", background: "var(--bg-input)", border: "1px solid var(--border-color)", borderRadius: "4px", color: "var(--text-primary)" }}
                />
              </div>

              <div>
                <label style={{ display: "block", fontSize: "12px", color: "var(--text-secondary)", marginBottom: "4px" }}>Limit Price (USD $)</label>
                <input
                  type="number"
                  step="0.01"
                  value={orderPrice}
                  onChange={(e) => setOrderPrice(e.target.value)}
                  style={{ width: "100%", padding: "8px", background: "var(--bg-input)", border: "1px solid var(--border-color)", borderRadius: "4px", color: "var(--text-primary)" }}
                />
              </div>

              <div>
                <button
                  type="submit"
                  disabled={orderSubmitting || (orderSide === "BUY" && ((parseFloat(orderQty) || 0) * (parseFloat(orderPrice) || 0)) > availableCashUsd)}
                  style={{
                    width: "100%",
                    padding: "10px",
                    background: (orderSide === "BUY" && ((parseFloat(orderQty) || 0) * (parseFloat(orderPrice) || 0)) > availableCashUsd)
                      ? "#64748b"
                      : "var(--accent-primary)",
                    color: "#fff",
                    border: "none",
                    borderRadius: "6px",
                    fontWeight: "600",
                    cursor: (orderSide === "BUY" && ((parseFloat(orderQty) || 0) * (parseFloat(orderPrice) || 0)) > toNum(availableCashUsd))
                      ? "not-allowed"
                      : "pointer",
                  }}
                >
                  {orderSubmitting ? "Submitting..." : `Place ${orderSide} Order ($)`}
                </button>
              </div>
            </form>

            {/* Order Cost & Available Cash Feedback */}
            <div style={{ marginTop: "12px", padding: "10px 14px", background: "var(--bg-input)", borderRadius: "6px", fontSize: "12px", display: "flex", justifyContent: "space-between", alignItems: "center", flexWrap: "wrap", gap: "8px" }}>
              <span>
                Order Notional: <strong>${(((parseFloat(orderQty) || 0) * (parseFloat(orderPrice) || 0))).toFixed(2)} USD</strong>
                {orderSide === "BUY" && (
                  <span style={{ marginLeft: "10px", color: ((parseFloat(orderQty) || 0) * (parseFloat(orderPrice) || 0)) <= toNum(availableCashUsd) ? "#4ade80" : "#f87171" }}>
                    ({((parseFloat(orderQty) || 0) * (parseFloat(orderPrice) || 0)) <= toNum(availableCashUsd) ? "✓ Within Available Cash" : "✗ Exceeds Available Cash"})
                  </span>
                )}
              </span>
              <span>
                Available US Cash: <strong style={{ color: "#38bdf8" }}>${toNum(availableCashUsd).toFixed(2)} USD</strong>
              </span>
            </div>

            {orderSide === "BUY" && ((parseFloat(orderQty) || 0) * (parseFloat(orderPrice) || 0)) > toNum(availableCashUsd) && (
              <div style={{ marginTop: "10px", padding: "8px 12px", background: "rgba(239, 68, 68, 0.15)", border: "1px solid rgba(239, 68, 68, 0.3)", borderRadius: "6px", color: "#f87171", fontSize: "12px" }}>
                ⚠️ <strong>Order exceeds budget:</strong> You can only trade up to <strong>${toNum(availableCashUsd).toFixed(2)} USD</strong> (Initial $5.00 + realized profits). Indian funds (₹) are completely separate and cannot be used.
              </div>
            )}

            {orderSuccess && (
              <div style={{ marginTop: "12px", padding: "10px", background: "rgba(34, 197, 94, 0.15)", border: "1px solid rgba(34, 197, 94, 0.3)", borderRadius: "6px", color: "var(--accent-positive)", fontSize: "13px" }}>
                {orderSuccess}
              </div>
            )}

            {orderError && (
              <div style={{ marginTop: "12px", padding: "10px", background: "rgba(239, 68, 68, 0.15)", border: "1px solid rgba(239, 68, 68, 0.3)", borderRadius: "6px", color: "var(--accent-negative)", fontSize: "13px" }}>
                {orderError}
              </div>
            )}
          </Card>

          {/* Recent Paper Orders Audit Trail */}
          <Card title="Recent US Paper Orders (Audit Trail)">
            <div style={{ overflowX: "auto" }}>
              <table style={{ width: "100%", borderCollapse: "collapse", fontSize: "13px" }}>
                <thead>
                  <tr style={{ borderBottom: "1px solid var(--border-color)", textAlign: "left" }}>
                    <th style={{ padding: "8px" }}>Time</th>
                    <th style={{ padding: "8px" }}>Client Order ID</th>
                    <th style={{ padding: "8px" }}>Symbol</th>
                    <th style={{ padding: "8px" }}>Side</th>
                    <th style={{ padding: "8px" }}>Qty</th>
                    <th style={{ padding: "8px" }}>Price</th>
                    <th style={{ padding: "8px" }}>Status</th>
                    <th style={{ padding: "8px" }}>Realized P&L (Profit)</th>
                  </tr>
                </thead>
                <tbody>
                  {recentOrders.length === 0 ? (
                    <tr>
                      <td colSpan={8} style={{ padding: "16px", textAlign: "center", color: "var(--text-secondary)" }}>
                        No paper orders placed yet. Submit an order above to test execution!
                      </td>
                    </tr>
                  ) : (
                    recentOrders.map((ord) => {
                      const match = usPnlMap.get(ord.id);
                      const isSell = ord.side === "SELL";
                      const pnl = match?.pnl ?? null;
                      const pnlPct = match?.pnlPct ?? null;
                      const isPos = (pnl ?? 0) > 0;
                      const isNeg = (pnl ?? 0) < 0;

                      return (
                        <tr key={ord.id} style={{ borderBottom: "1px solid var(--border-color)" }}>
                          <td style={{ padding: "8px", fontSize: "12px", color: "var(--text-secondary)" }}>
                            {new Date(ord.created_at).toLocaleTimeString()}
                          </td>
                          <td style={{ padding: "8px", fontFamily: "monospace", fontSize: "12px" }}>{ord.client_order_id}</td>
                          <td style={{ padding: "8px", fontWeight: "600" }}>{ord.symbol}</td>
                          <td style={{ padding: "8px" }}>
                            <Badge tone={ord.side === "BUY" ? "good" : "critical"}>{ord.side}</Badge>
                          </td>
                          <td style={{ padding: "8px" }}>{ord.quantity}</td>
                          <td style={{ padding: "8px" }}>${ord.price}</td>
                          <td style={{ padding: "8px" }}>
                            <Badge tone="good">{ord.status}</Badge>
                          </td>
                          <td style={{ padding: "8px" }}>
                            {isSell ? (
                              pnl !== null ? (
                                <div>
                                  <span style={{ fontWeight: 700, color: isPos ? "var(--accent-positive, #10b981)" : isNeg ? "var(--accent-negative, #ef4444)" : "inherit" }}>
                                    {isPos ? `+$${toNum(pnl).toFixed(2)}` : isNeg ? `-$${Math.abs(toNum(pnl)).toFixed(2)}` : "$0.00"}
                                  </span>
                                  {pnlPct !== null && (
                                    <div style={{ fontSize: "11px", fontWeight: 600, color: isPos ? "var(--accent-positive, #10b981)" : isNeg ? "var(--accent-negative, #ef4444)" : "var(--text-secondary)" }}>
                                      {isPos ? `+${toNum(pnlPct).toFixed(2)}%` : `${toNum(pnlPct).toFixed(2)}%`}
                                    </div>
                                  )}
                                </div>
                              ) : (
                                <span className="muted">—</span>
                              )
                            ) : (
                              <span style={{ fontSize: "12px", color: "var(--text-secondary)" }}>— (Open Long)</span>
                            )}
                          </td>
                        </tr>
                      );
                    })
                  )}
                </tbody>
              </table>
            </div>
          </Card>

          <Card title="Liquid Watchlist (US Universe)">
            <div style={{ overflowX: "auto" }}>
              <table style={{ width: "100%", borderCollapse: "collapse", fontSize: "13px" }}>
                <thead>
                  <tr style={{ borderBottom: "1px solid var(--border-color)", textAlign: "left" }}>
                    <th style={{ padding: "8px" }}>Symbol</th>
                    <th style={{ padding: "8px" }}>Name</th>
                    <th style={{ padding: "8px" }}>Exchange</th>
                    <th style={{ padding: "8px" }}>Tick Size</th>
                    <th style={{ padding: "8px" }}>Lot Size</th>
                    <th style={{ padding: "8px" }}>Min Notional</th>
                  </tr>
                </thead>
                <tbody>
                  {watchlist.map((inst) => (
                    <tr key={inst.symbol} style={{ borderBottom: "1px solid var(--border-color)" }}>
                      <td style={{ padding: "8px", fontWeight: "600" }}>{inst.symbol}</td>
                      <td style={{ padding: "8px" }}>{inst.name}</td>
                      <td style={{ padding: "8px" }}>{inst.exchange}</td>
                      <td style={{ padding: "8px" }}>${inst.tick_size}</td>
                      <td style={{ padding: "8px" }}>{inst.lot_size} sh</td>
                      <td style={{ padding: "8px" }}>${inst.min_notional}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </Card>
        </div>
      )}

      {/* TAB 3: Exchange Calendar & Regulatory Specs */}
      {activeTab === "specs" && (
        <Card title="US Equities Regulatory Cost Schedule & Market Session Rules">
          <div style={{ display: "flex", flexDirection: "column", gap: "16px", fontSize: "14px", lineHeight: "1.6" }}>
            <div>
              <h3 style={{ fontSize: "16px", fontWeight: "600", marginBottom: "6px" }}>1. Trading Hours & Intraday Cutoffs</h3>
              <ul>
                <li><strong>Regular Session:</strong> 09:30 - 16:00 ET (America/New_York), Monday through Friday.</li>
                <li><strong>Intraday Entry Cutoff:</strong> New entries are strictly blocked 30 minutes before session close (at 15:30 ET).</li>
                <li><strong>Intraday Controlled Exit:</strong> Open intraday positions are scheduled for controlled square-off 15 minutes before close (at 15:45 ET).</li>
                <li><strong>Daylight Saving:</strong> Automatically adapts to US EDT (UTC-4) and EST (UTC-5) clock shifts.</li>
              </ul>
            </div>

            <div>
              <h3 style={{ fontSize: "16px", fontWeight: "600", marginBottom: "6px" }}>2. Statutory & Regulatory Fee Schedule</h3>
              <ul>
                <li><strong>SEC Section 31 Fee:</strong> $27.80 per $1,000,000 of principal (rate = 0.0000278), applicable to SELL orders only, rounded up to the nearest cent.</li>
                <li><strong>FINRA Trading Activity Fee (TAF):</strong> $0.000166 per share, applicable to SELL orders only, minimum $0.01, maximum $8.30 per trade.</li>
                <li><strong>Broker Commission:</strong> $0.00 for Alpaca paper / retail zero-commission brokers.</li>
                <li><strong>Spread & Slippage:</strong> Conservative modeling of 1 bp half-spread and 2 bps execution slippage for liquid equities.</li>
              </ul>
            </div>

            <div>
              <h3 style={{ fontSize: "16px", fontWeight: "600", marginBottom: "6px" }}>3. Anti-Martingale & Capital Protection Rules</h3>
              <ul>
                <li><strong>No Martingale Multipliers:</strong> Position sizes are determined by volatility targeting (ATR) and strictly decrease or remain fixed after losses. Sizing is never increased to recover losses.</li>
                <li><strong>Long-Only Without Leverage:</strong> All cash equity trades are 100% equity-backed with zero borrowing or margin leverage.</li>
                <li><strong>Account-Wide Emergency Stop:</strong> Immediately halts new entries and closes resting orders upon operator activation.</li>
              </ul>
            </div>
          </div>
        </Card>
      )}
    </div>
  );
}
