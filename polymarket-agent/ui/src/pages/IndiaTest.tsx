import { useState, useEffect, useMemo } from "react";
import { Card } from "../components/ui/Card";
import { StatTile } from "../components/ui/StatTile";
import { Badge } from "../components/ui/Badge";
import { inr, inrSigned, shares, istTime, pctFromFraction, DASH } from "../lib/format";
import { postJson } from "../api/client";
import { NseLiveChart } from "../components/india/NseLiveChart";
import { computeRealizedPnl } from "../lib/pnl";

interface Instrument {
  symbol: string;
  name: string;
  isin: string;
  upstox_token: string;
  tick_size: string;
  lot_size: number;
  fractional: boolean;
}

interface StatutoryCosts {
  stt: string;
  brokerage: string;
  exchange_turnover_charges: string;
  sebi_charges: string;
  stamp_duty: string;
  gst: string;
  ipft: string;
  total_costs: string;
}

interface BacktestTrade {
  id: number;
  symbol: string;
  shares: number;
  entry_time: string;
  entry_price: string;
  entry_costs: StatutoryCosts;
  exit_time?: string;
  exit_price?: string;
  exit_costs?: StatutoryCosts;
  gross_pnl: string;
  total_costs: string;
  net_pnl: string;
  exit_reason: string;
}

interface BacktestResults {
  initial_capital: string;
  final_equity: string;
  net_profit: string;
  realized_pnl: string;
  unrealized_pnl: string;
  total_costs: string;
  total_trades: number;
  winning_trades: number;
  losing_trades: number;
  win_rate_pct: string;
  profit_factor?: string;
  max_drawdown_pct: string;
  benchmark_return_pct: string;
  insufficient_data_warning: boolean;
  total_stt: string;
  total_brokerage: string;
  total_exchange_fees: string;
  total_sebi_fees: string;
  total_stamp_duty: string;
  total_gst: string;
  trades: BacktestTrade[];
}

interface SandboxOrder {
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
  rejection_reason?: string;
  created_at: string;
}

const NSE_DEFAULT_PRICES: Record<string, string> = {
  ITC: "480.00",
  SBIN: "820.00",
  TATAMOTORS: "890.00",
  RELIANCE: "1198.70",
  ICICIBANK: "1250.00",
  HDFCBANK: "1650.00",
  BHARTIARTL: "1680.00",
  INFY: "1850.00",
  LT: "3550.00",
  TCS: "3800.00",
};

interface SandboxStatus {
  venue_id: string;
  name: string;
  currency: string;
  currency_symbol: string;
  timezone: string;
  trading_hours: string;
  allowlisted_host: string;
  live_trading_allowed: boolean;
  mode_description: string;
  base_capital_inr?: number;
  realized_profit_inr?: number;
  open_positions_cost_inr?: number;
  available_cash_inr?: number;
  total_equity_inr?: number;
  holdings?: Record<string, number>;
  recent_orders: SandboxOrder[];
  watchlist: Instrument[];
}

export function IndiaTest() {
  const [activeTab, setActiveTab] = useState<"live" | "sandbox" | "backtest" | "specs">("live");
  
  // Backtest form state
  const [initialCapital, setInitialCapital] = useState("1000");
  const [positionPct, setPositionPct] = useState("0.10");
  const [stopLossPct, setStopLossPct] = useState("0.015");
  const [takeProfitPct, setTakeProfitPct] = useState("0.030");
  const [trainSplitPct, setTrainSplitPct] = useState("0.60");
  const [valSplitPct, setValSplitPct] = useState("0.20");
  const [useCustomCsv, setUseCustomCsv] = useState(false);
  const [customCsv, setCustomCsv] = useState("");
  const [backtestRunning, setBacktestRunning] = useState(false);
  const [backtestError, setBacktestError] = useState<string | null>(null);
  const [backtestResult, setBacktestResult] = useState<BacktestResults | null>(null);

  // Sandbox state (Strictly ₹1,000 INR micro-capital isolation)
  const [sandboxStatus, setSandboxStatus] = useState<SandboxStatus | null>(null);
  const [orderSymbol, setOrderSymbol] = useState("ITC");
  const [orderSide, setOrderSide] = useState<"BUY" | "SELL">("BUY");
  const [orderQty, setOrderQty] = useState("1");
  const [orderPrice, setOrderPrice] = useState("480.00");
  const [orderSubmitting, setOrderSubmitting] = useState(false);
  const [orderError, setOrderError] = useState<string | null>(null);
  const [orderSuccess, setOrderSuccess] = useState<string | null>(null);
  const [baseCapitalInr, setBaseCapitalInr] = useState<number>(1000.0);
  const [realizedProfitInr, setRealizedProfitInr] = useState<number>(0.0);
  const [openPositionsCostInr, setOpenPositionsCostInr] = useState<number>(0.0);
  const [availableCashInr, setAvailableCashInr] = useState<number>(1000.0);
  const [totalEquityInr, setTotalEquityInr] = useState<number>(1000.0);
  const [holdings, setHoldings] = useState<Record<string, number>>({});
  const [resetting, setResetting] = useState(false);
  const [resetMsg, setResetMsg] = useState<string | null>(null);

  // Upstox Live Feed & Strategy Tester State
  const [feedStatus, setFeedStatus] = useState<{
    connected: boolean;
    status: string;
    feed_type: string;
    token_configured: boolean;
    token_preview?: string;
    redirect_uri?: string;
    auth_dialog_url?: string;
    symbols_subscribed: string[];
    last_tick_time?: string;
    live_prices: Record<string, { price: number | string; time: string; source: string; change_pct: number | string }>;
  } | null>(null);

  const [clientIdInput, setClientIdInput] = useState("");
  const [clientSecretInput, setClientSecretInput] = useState("");
  const [accessTokenInput, setAccessTokenInput] = useState("");
  const [authSaving, setAuthSaving] = useState(false);
  const [authMsg, setAuthMsg] = useState<string | null>(null);

  const [stratSymbol, setStratSymbol] = useState("ITC");
  const [stratPrice, setStratPrice] = useState("");
  const [stratExecuting, setStratExecuting] = useState(false);
  const [stratResult, setStratResult] = useState<{
    action: string;
    symbol: string;
    price: number;
    quantity: number;
    reason: string;
    confidence: number;
    timestamp: string;
    executed: boolean;
  } | null>(null);

  // Compute Realized P&L for Indian sandbox orders
  const indiaPnlMap = useMemo(() => {
    const list = sandboxStatus?.recent_orders ?? [];
    return computeRealizedPnl(
      list.map((o) => ({
        id: o.id,
        symbol: o.symbol,
        side: o.side,
        quantity: o.quantity,
        price: o.price,
        average_fill_price: o.average_fill_price,
        created_at: o.created_at,
      })),
    );
  }, [sandboxStatus]);

  // Fetch sandbox status and watchlist
  const loadSandboxStatus = async () => {
    try {
      const res = await fetch("/api/india/sandbox/status");
      if (res.ok) {
        const data = await res.json();
        setSandboxStatus(data);
        if (data.base_capital_inr !== undefined) setBaseCapitalInr(Number(data.base_capital_inr));
        if (data.realized_profit_inr !== undefined) setRealizedProfitInr(Number(data.realized_profit_inr));
        if (data.open_positions_cost_inr !== undefined) setOpenPositionsCostInr(Number(data.open_positions_cost_inr));
        if (data.available_cash_inr !== undefined) setAvailableCashInr(Number(data.available_cash_inr));
        if (data.total_equity_inr !== undefined) setTotalEquityInr(Number(data.total_equity_inr));
        if (data.holdings) setHoldings(data.holdings);
      }
    } catch (e) {
      console.error("Failed to load sandbox status", e);
    }
  };

  async function handleResetIndiaMarket() {
    if (!window.confirm("Are you sure you want to reset the Indian Sandbox Market to strictly ₹1,000 initial capital? All previous sandbox orders will be deleted.")) {
      return;
    }
    setResetting(true);
    setResetMsg(null);
    setOrderError(null);
    setOrderSuccess(null);
    try {
      const res = await postJson<{ status: string; message: string }>("/api/india/sandbox/reset", {});
      if (res.ok) {
        setResetMsg("Indian Market successfully reset to ₹1,000 INR initial capital! Sandbox orders cleared.");
        loadSandboxStatus();
      } else {
        setOrderError(res.error.kind === "http" ? res.error.message : "Failed to reset Indian sandbox market");
      }
    } catch (err: unknown) {
      setOrderError(err instanceof Error ? err.message : String(err));
    } finally {
      setResetting(false);
    }
  }

  const loadFeedStatus = async () => {
    try {
      const res = await fetch("/api/upstox/feed/status");
      if (res.ok) {
        const d = await res.json();
        const feedInfo = d.feed || d;
        let lpMap: Record<string, { price: number; time: string; source: string; change_pct: number }> = {};
        if (d.live_prices && Object.keys(d.live_prices).length > 0) {
          for (const [sym, lp] of Object.entries(d.live_prices as Record<string, { price: string | number; time: string; source: string; change_pct: string | number }>)) {
            lpMap[sym] = {
              price: Number(lp?.price || 0),
              time: lp?.time || "",
              source: lp?.source || "SIMULATED",
              change_pct: Number(lp?.change_pct || 0),
            };
          }
        } else if (d.ticks) {
          for (const [sym, t] of Object.entries(d.ticks as Record<string, { last_price: string | number; timestamp: string; source: string; close: string | number }>)) {
            const last = Number(t.last_price || 0);
            const close = Number(t.close || last);
            const changePct = close > 0 ? ((last - close) / close) * 100 : 0;
            lpMap[sym] = {
              price: last,
              time: t.timestamp,
              source: t.source,
              change_pct: changePct,
            };
          }
        }
        setFeedStatus({
          connected: Boolean(feedInfo.connected),
          status: feedInfo.status || "SIMULATED",
          feed_type: feedInfo.feed_type || "HIGH_FIDELITY_SIMULATED",
          token_configured: Boolean(feedInfo.token_configured),
          token_preview: feedInfo.token_preview,
          redirect_uri: feedInfo.redirect_uri,
          auth_dialog_url: feedInfo.auth_dialog_url,
          symbols_subscribed: feedInfo.symbols_subscribed || feedInfo.symbols || ["ITC", "SBIN", "RELIANCE", "INFY", "TATAMOTORS"],
          last_tick_time: feedInfo.last_tick_time || feedInfo.last_update,
          live_prices: lpMap,
        });
      }
    } catch {
      // ignore
    }
  };

  const handleSaveUpstoxAuth = async (e: React.FormEvent) => {
    e.preventDefault();
    setAuthSaving(true);
    setAuthMsg(null);
    try {
      const body: Record<string, string> = {};
      if (clientIdInput.trim()) body.client_id = clientIdInput.trim();
      if (clientSecretInput.trim()) body.client_secret = clientSecretInput.trim();
      if (accessTokenInput.trim()) body.access_token = accessTokenInput.trim();

      const res = await fetch("/api/upstox/auth/token", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });

      if (res.ok) {
        setAuthMsg("Upstox credentials updated successfully! Market data feed is connecting.");
        setAccessTokenInput("");
        loadFeedStatus();
      } else {
        const t = await res.text();
        setAuthMsg(`Failed to save Upstox auth: ${t}`);
      }
    } catch (err: unknown) {
      setAuthMsg(`Error: ${err instanceof Error ? err.message : String(err)}`);
    } finally {
      setAuthSaving(false);
    }
  };

  const handleTestStrategyTick = async (executeOrder: boolean) => {
    setStratExecuting(true);
    setStratResult(null);
    try {
      const payload: Record<string, unknown> = {
        symbol: stratSymbol,
        execute: executeOrder,
      };
      if (stratPrice.trim()) {
        payload.price = parseFloat(stratPrice.trim());
      }

      const res = await fetch("/api/upstox/strategy/tick", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
      });

      if (res.ok) {
        const data = await res.json();
        setStratResult({
          action: data.decision.action,
          symbol: data.decision.symbol,
          price: data.decision.price,
          quantity: data.decision.quantity,
          reason: data.decision.reason,
          confidence: data.decision.confidence,
          timestamp: data.decision.timestamp,
          executed: data.paper_order_executed,
        });
        if (data.paper_order_executed) {
          loadSandboxStatus();
        }
      } else {
        const t = await res.text();
        alert(`Strategy evaluation error: ${t}`);
      }
    } catch (err: unknown) {
      alert(`Strategy test failed: ${err instanceof Error ? err.message : String(err)}`);
    } finally {
      setStratExecuting(false);
    }
  };

  useEffect(() => {
    loadSandboxStatus();
    loadFeedStatus();
    const interval = setInterval(() => {
      loadFeedStatus();
    }, 3000);
    return () => clearInterval(interval);
  }, []);

  const handleRunBacktest = async () => {
    setBacktestRunning(true);
    setBacktestError(null);
    try {
      const payload: Record<string, unknown> = {
        name: `Backtest NSE ${new Date().toLocaleTimeString()}`,
        initial_capital: initialCapital,
        risk_pct_per_trade: positionPct,
        stop_loss_pct: stopLossPct,
        take_profit_pct: takeProfitPct,
        train_ratio: trainSplitPct,
        val_ratio: valSplitPct,
      };
      if (useCustomCsv && customCsv.trim()) {
        payload.csv_content = customCsv.trim();
      }

      const res = await postJson<BacktestResults>("/api/india/backtest/run", payload);
      if (res.ok) {
        setBacktestResult(res.value);
      } else {
        setBacktestError(res.error.kind === "http" ? res.error.message : "Backtest execution failed");
      }
    } catch (e) {
      setBacktestError(e instanceof Error ? e.message : String(e));
    } finally {
      setBacktestRunning(false);
    }
  };

  const handleSandboxOrder = async (e: React.FormEvent) => {
    e.preventDefault();
    setOrderSubmitting(true);
    setOrderError(null);
    setOrderSuccess(null);

    const qty = parseInt(orderQty, 10);
    if (isNaN(qty) || qty <= 0) {
      setOrderError("Quantity must be a positive whole integer for NSE cash equities");
      setOrderSubmitting(false);
      return;
    }

    try {
      const payload = {
        symbol: orderSymbol,
        side: orderSide,
        order_type: "LIMIT",
        quantity: qty,
        price: orderPrice,
      };

      const res = await postJson<{ status: string; order: SandboxOrder; disclaimer: string }>(
        "/api/india/sandbox/order",
        payload
      );

      if (res.ok) {
        setOrderSuccess(`Order submitted successfully! Client ID: ${res.value.order.client_order_id}`);
        loadSandboxStatus();
      } else {
        setOrderError(res.error.kind === "http" ? res.error.message : "Sandbox order failed");
      }
    } catch (err) {
      setOrderError(err instanceof Error ? err.message : String(err));
    } finally {
      setOrderSubmitting(false);
    }
  };

  return (
    <div className="page" style={{ maxWidth: "1280px", margin: "0 auto", padding: "16px" }}>
      {/* Sleek Top Header & Tab Navigation */}
      <div style={{
        display: "flex",
        justifyContent: "space-between",
        alignItems: "center",
        flexWrap: "wrap",
        gap: "12px",
        background: "var(--card-bg, #1a1a1e)",
        border: "1px solid var(--border-color, #2a2a30)",
        borderRadius: "10px",
        padding: "12px 20px",
        marginBottom: "16px",
      }}>
        <div style={{ display: "flex", alignItems: "center", gap: "10px", flexWrap: "wrap" }}>
          <span style={{ fontSize: "16px", fontWeight: "700", color: "#fff", display: "flex", alignItems: "center", gap: "6px" }}>
            <span>🇮🇳</span> Indian Stock Market (NSE Cash Equities)
          </span>
          <Badge tone="good">INTRADAY MIS</Badge>
          <Badge tone="neutral">INR (₹)</Badge>
          <Badge tone="neutral">Asia/Kolkata (09:15 - 15:30 IST)</Badge>
          <Badge tone="warning">SANDBOX SIMULATION</Badge>
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
            className={`btn ${activeTab === "sandbox" ? "btn--primary" : "btn--secondary"}`}
            onClick={() => setActiveTab("sandbox")}
            style={{ fontSize: "0.825rem", padding: "0.35rem 0.75rem", fontWeight: activeTab === "sandbox" ? 700 : 500 }}
          >
            ⚡ Upstox Feed & Paper Simulator
          </button>
          <button
            className={`btn ${activeTab === "specs" ? "btn--primary" : "btn--secondary"}`}
            onClick={() => setActiveTab("specs")}
            style={{ fontSize: "0.825rem", padding: "0.35rem 0.75rem" }}
          >
            NSE Watchlist & Rules
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
        <NseLiveChart onOrderPlaced={loadSandboxStatus} />
      )}

      {/* Tab 1: Historical Backtester */}
      {activeTab === "backtest" && (
        <>
          <Card title="Strategy Backtest Configuration">
            <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(200px, 1fr))", gap: "1rem", marginBottom: "1rem" }}>
              <div>
                <label style={{ display: "block", fontSize: "0.8rem", color: "var(--muted)", marginBottom: "0.25rem" }}>
                  Initial Capital (INR)
                </label>
                <input
                  type="number"
                  value={initialCapital}
                  onChange={(e) => setInitialCapital(e.target.value)}
                  style={{ width: "100%", padding: "0.4rem", borderRadius: "4px", border: "1px solid var(--border)", background: "var(--surface)" }}
                />
              </div>
              <div>
                <label style={{ display: "block", fontSize: "0.8rem", color: "var(--muted)", marginBottom: "0.25rem" }}>
                  Max Position Exposure (fraction)
                </label>
                <input
                  type="text"
                  value={positionPct}
                  onChange={(e) => setPositionPct(e.target.value)}
                  style={{ width: "100%", padding: "0.4rem", borderRadius: "4px", border: "1px solid var(--border)", background: "var(--surface)" }}
                />
              </div>
              <div>
                <label style={{ display: "block", fontSize: "0.8rem", color: "var(--muted)", marginBottom: "0.25rem" }}>
                  Stop Loss (fraction, e.g. 0.015 = 1.5%)
                </label>
                <input
                  type="text"
                  value={stopLossPct}
                  onChange={(e) => setStopLossPct(e.target.value)}
                  style={{ width: "100%", padding: "0.4rem", borderRadius: "4px", border: "1px solid var(--border)", background: "var(--surface)" }}
                />
              </div>
              <div>
                <label style={{ display: "block", fontSize: "0.8rem", color: "var(--muted)", marginBottom: "0.25rem" }}>
                  Take Profit (fraction, e.g. 0.030 = 3.0%)
                </label>
                <input
                  type="text"
                  value={takeProfitPct}
                  onChange={(e) => setTakeProfitPct(e.target.value)}
                  style={{ width: "100%", padding: "0.4rem", borderRadius: "4px", border: "1px solid var(--border)", background: "var(--surface)" }}
                />
              </div>
              <div>
                <label style={{ display: "block", fontSize: "0.8rem", color: "var(--muted)", marginBottom: "0.25rem" }}>
                  Train Split Ratio (0.60 = 60%)
                </label>
                <input
                  type="text"
                  value={trainSplitPct}
                  onChange={(e) => setTrainSplitPct(e.target.value)}
                  style={{ width: "100%", padding: "0.4rem", borderRadius: "4px", border: "1px solid var(--border)", background: "var(--surface)" }}
                />
              </div>
              <div>
                <label style={{ display: "block", fontSize: "0.8rem", color: "var(--muted)", marginBottom: "0.25rem" }}>
                  Val Split Ratio (0.20 = 20%)
                </label>
                <input
                  type="text"
                  value={valSplitPct}
                  onChange={(e) => setValSplitPct(e.target.value)}
                  style={{ width: "100%", padding: "0.4rem", borderRadius: "4px", border: "1px solid var(--border)", background: "var(--surface)" }}
                />
              </div>
            </div>

            <div style={{ marginBottom: "1rem" }}>
              <label style={{ display: "flex", alignItems: "center", gap: "0.5rem", fontSize: "0.875rem", cursor: "pointer", marginBottom: "0.5rem" }}>
                <input
                  type="checkbox"
                  checked={useCustomCsv}
                  onChange={(e) => setUseCustomCsv(e.target.checked)}
                />
                Supply Custom OHLCV CSV text (default: <code>data/nse_sample_data.csv</code>)
              </label>

              {useCustomCsv && (
                <div>
                  <textarea
                    rows={6}
                    value={customCsv}
                    onChange={(e) => setCustomCsv(e.target.value)}
                    placeholder="timestamp,symbol,open,high,low,close,volume&#10;2024-01-01 09:15:00,RELIANCE,2500.00,2515.00,2495.00,2510.00,125000"
                    style={{
                      width: "100%",
                      fontFamily: "monospace",
                      fontSize: "0.8rem",
                      padding: "0.5rem",
                      borderRadius: "4px",
                      border: "1px solid var(--border)",
                      background: "var(--surface)"
                    }}
                  />
                  <div style={{ fontSize: "0.75rem", color: "var(--muted)", marginTop: "0.25rem" }}>
                    Schema: <code>timestamp,symbol,open,high,low,close,volume</code>. Timestamps assumed IST (Asia/Kolkata).
                  </div>
                </div>
              )}
            </div>

            <div style={{ display: "flex", gap: "1rem", alignItems: "center" }}>
              <button
                className="btn btn--primary"
                onClick={handleRunBacktest}
                disabled={backtestRunning}
                style={{ padding: "0.5rem 1.25rem" }}
              >
                {backtestRunning ? "Running Simulation..." : "Run Backtest Simulation"}
              </button>
              {backtestRunning && <span style={{ fontSize: "0.85rem", color: "var(--muted)" }}>Validating chronological bars and executing trades...</span>}
            </div>

            {backtestError && (
              <div style={{ marginTop: "1rem", padding: "0.75rem", background: "rgba(239, 68, 68, 0.1)", border: "1px solid var(--danger)", borderRadius: "4px", color: "var(--danger)", fontSize: "0.85rem" }}>
                <strong>Execution Error:</strong> {backtestError}
              </div>
            )}
          </Card>

          {/* Backtest Results */}
          {backtestResult && (
            <>
              {backtestResult.insufficient_data_warning && (
                <div style={{
                  padding: "0.75rem 1rem",
                  background: "rgba(245, 158, 11, 0.15)",
                  border: "1px solid #f59e0b",
                  borderRadius: "6px",
                  color: "#d97706",
                  fontSize: "0.875rem",
                  fontWeight: 500,
                  display: "flex",
                  alignItems: "center",
                  gap: "0.5rem"
                }}>
                  <span>⚠️</span>
                  <span>
                    <strong>Statistical Significance Notice:</strong> This backtest contains fewer than 10 trades ({backtestResult.total_trades}).
                    Metrics (win rate, profit factor, expectancy) carry high statistical uncertainty and should not be used as evidence of an edge.
                  </span>
                </div>
              )}

              <Card title="Backtest Performance & Equity">
                <div className="grid grid--tiles">
                  <StatTile
                    hero
                    label="Final Equity"
                    value={inr(parseFloat(backtestResult.final_equity))}
                    sub={`Initial capital ${inr(parseFloat(backtestResult.initial_capital))}`}
                  />
                  <StatTile
                    label="Net Profit / Loss"
                    value={inrSigned(parseFloat(backtestResult.net_profit))}
                    sub={`Realized: ${inrSigned(parseFloat(backtestResult.realized_pnl))}`}
                  />
                  <StatTile
                    label="Statutory Costs"
                    value={inr(parseFloat(backtestResult.total_costs))}
                    sub="STT + Exch + SEBI + Stamp + GST"
                  />
                  <StatTile
                    label="Win Rate"
                    value={pctFromFraction(parseFloat(backtestResult.win_rate_pct) / 100)}
                    sub={`${backtestResult.winning_trades}W / ${backtestResult.losing_trades}L (${backtestResult.total_trades} trades)`}
                  />
                  <StatTile
                    label="Max Drawdown"
                    value={pctFromFraction(parseFloat(backtestResult.max_drawdown_pct))}
                    sub="From high water mark"
                  />
                  <StatTile
                    label="Buy & Hold Benchmark"
                    value={pctFromFraction(parseFloat(backtestResult.benchmark_return_pct))}
                    sub="Asset price return"
                  />
                </div>
              </Card>

              {/* Statutory Fee Breakdown */}
              <Card title="Indian Statutory Transaction Cost Breakdown">
                <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(160px, 1fr))", gap: "1rem" }}>
                  <div style={{ padding: "0.75rem", background: "var(--surface)", borderRadius: "4px", border: "1px solid var(--border)" }}>
                    <div style={{ fontSize: "0.75rem", color: "var(--muted)" }}>STT (Delivery)</div>
                    <div style={{ fontSize: "1.1rem", fontWeight: 600 }}>{inr(parseFloat(backtestResult.total_stt))}</div>
                    <div style={{ fontSize: "0.7rem", color: "var(--muted)" }}>0.1% Buy & Sell</div>
                  </div>
                  <div style={{ padding: "0.75rem", background: "var(--surface)", borderRadius: "4px", border: "1px solid var(--border)" }}>
                    <div style={{ fontSize: "0.75rem", color: "var(--muted)" }}>NSE Turnover Charges</div>
                    <div style={{ fontSize: "1.1rem", fontWeight: 600 }}>{inr(parseFloat(backtestResult.total_exchange_fees))}</div>
                    <div style={{ fontSize: "0.7rem", color: "var(--muted)" }}>0.00297% turnover</div>
                  </div>
                  <div style={{ padding: "0.75rem", background: "var(--surface)", borderRadius: "4px", border: "1px solid var(--border)" }}>
                    <div style={{ fontSize: "0.75rem", color: "var(--muted)" }}>SEBI Charges</div>
                    <div style={{ fontSize: "1.1rem", fontWeight: 600 }}>{inr(parseFloat(backtestResult.total_sebi_fees))}</div>
                    <div style={{ fontSize: "0.7rem", color: "var(--muted)" }}>₹10 per crore</div>
                  </div>
                  <div style={{ padding: "0.75rem", background: "var(--surface)", borderRadius: "4px", border: "1px solid var(--border)" }}>
                    <div style={{ fontSize: "0.75rem", color: "var(--muted)" }}>Stamp Duty</div>
                    <div style={{ fontSize: "1.1rem", fontWeight: 600 }}>{inr(parseFloat(backtestResult.total_stamp_duty))}</div>
                    <div style={{ fontSize: "0.7rem", color: "var(--muted)" }}>0.015% on Buy</div>
                  </div>
                  <div style={{ padding: "0.75rem", background: "var(--surface)", borderRadius: "4px", border: "1px solid var(--border)" }}>
                    <div style={{ fontSize: "0.75rem", color: "var(--muted)" }}>GST (18%)</div>
                    <div style={{ fontSize: "1.1rem", fontWeight: 600 }}>{inr(parseFloat(backtestResult.total_gst))}</div>
                    <div style={{ fontSize: "0.7rem", color: "var(--muted)" }}>On brokerage & charges</div>
                  </div>
                  <div style={{ padding: "0.75rem", background: "var(--surface)", borderRadius: "4px", border: "1px solid var(--border)" }}>
                    <div style={{ fontSize: "0.75rem", color: "var(--muted)" }}>Brokerage</div>
                    <div style={{ fontSize: "1.1rem", fontWeight: 600 }}>{inr(parseFloat(backtestResult.total_brokerage))}</div>
                    <div style={{ fontSize: "0.7rem", color: "var(--muted)" }}>Capped ₹20/order</div>
                  </div>
                </div>
              </Card>

              {/* Trade Log */}
              <Card title={`Simulated Trades (${backtestResult.trades.length})`}>
                <div style={{ overflowX: "auto" }}>
                  <table style={{ width: "100%", borderCollapse: "collapse", fontSize: "0.85rem", textAlign: "left" }}>
                    <thead>
                      <tr style={{ borderBottom: "2px solid var(--border)", color: "var(--muted)" }}>
                        <th style={{ padding: "0.5rem" }}>#</th>
                        <th style={{ padding: "0.5rem" }}>Symbol</th>
                        <th style={{ padding: "0.5rem" }}>Quantity</th>
                        <th style={{ padding: "0.5rem" }}>Entry (IST)</th>
                        <th style={{ padding: "0.5rem" }}>Entry Px</th>
                        <th style={{ padding: "0.5rem" }}>Exit (IST)</th>
                        <th style={{ padding: "0.5rem" }}>Exit Px</th>
                        <th style={{ padding: "0.5rem" }}>Gross P&L</th>
                        <th style={{ padding: "0.5rem" }}>Costs</th>
                        <th style={{ padding: "0.5rem" }}>Net P&L</th>
                        <th style={{ padding: "0.5rem" }}>Exit Reason</th>
                      </tr>
                    </thead>
                    <tbody>
                      {backtestResult.trades.map((t) => {
                        const net = parseFloat(t.net_pnl);
                        const gross = parseFloat(t.gross_pnl);
                        return (
                          <tr key={t.id} style={{ borderBottom: "1px solid var(--border)" }}>
                            <td style={{ padding: "0.5rem" }}>{t.id}</td>
                            <td style={{ padding: "0.5rem", fontWeight: 600 }}>{t.symbol}</td>
                            <td style={{ padding: "0.5rem" }}>{shares(t.shares)}</td>
                            <td style={{ padding: "0.5rem", whiteSpace: "nowrap" }}>{istTime(new Date(t.entry_time))}</td>
                            <td style={{ padding: "0.5rem" }}>{inr(parseFloat(t.entry_price))}</td>
                            <td style={{ padding: "0.5rem", whiteSpace: "nowrap" }}>{t.exit_time ? istTime(new Date(t.exit_time)) : DASH}</td>
                            <td style={{ padding: "0.5rem" }}>{t.exit_price ? inr(parseFloat(t.exit_price)) : DASH}</td>
                            <td style={{ padding: "0.5rem", color: gross > 0 ? "var(--good)" : gross < 0 ? "var(--danger)" : "inherit" }}>
                              {inrSigned(gross)}
                            </td>
                            <td style={{ padding: "0.5rem", color: "var(--muted)" }}>
                              {inr(parseFloat(t.total_costs))}
                            </td>
                            <td style={{ padding: "0.5rem", fontWeight: 600, color: net > 0 ? "var(--good)" : net < 0 ? "var(--danger)" : "inherit" }}>
                              {inrSigned(net)}
                            </td>
                            <td style={{ padding: "0.5rem" }}>
                              <Badge tone={t.exit_reason.includes("PROFIT") ? "good" : t.exit_reason.includes("LOSS") ? "critical" : "neutral"}>
                                {t.exit_reason}
                              </Badge>
                            </td>
                          </tr>
                        );
                      })}
                      {backtestResult.trades.length === 0 && (
                        <tr>
                          <td colSpan={11} style={{ padding: "1.5rem", textAlign: "center", color: "var(--muted)" }}>
                            No trade signals triggered with the selected parameters.
                          </td>
                        </tr>
                      )}
                    </tbody>
                  </table>
                </div>
              </Card>
            </>
          )}
        </>
      )}

      {/* Tab 2: Upstox Sandbox Adapter */}
      {activeTab === "sandbox" && (
        <>
          {/* Strictly Isolated Capital Allocation Banner & Reset Button */}
          <div style={{
            background: "linear-gradient(135deg, rgba(30, 41, 59, 0.9) 0%, rgba(15, 23, 42, 0.95) 100%)",
            border: "1px solid rgba(16, 185, 129, 0.3)",
            borderRadius: "10px",
            padding: "18px",
            marginBottom: "20px",
            boxShadow: "0 4px 16px rgba(0, 0, 0, 0.3)"
          }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", flexWrap: "wrap", gap: "12px", marginBottom: "16px" }}>
              <div>
                <div style={{ display: "flex", alignItems: "center", gap: "10px" }}>
                  <span style={{ fontSize: "20px" }}>🇮🇳</span>
                  <h3 style={{ margin: 0, fontSize: "17px", fontWeight: "800", color: "#fff" }}>
                    Strictly Isolated Indian Market Sandbox (₹1,000 INR Allocation)
                  </h3>
                  <Badge tone="good">Strict Isolation Active</Badge>
                </div>
                <p style={{ margin: "6px 0 0", fontSize: "13px", color: "#94a3b8" }}>
                  Trades start with strictly <strong>₹1,000.00 INR</strong>. Profits from closed trades compound directly back into trading cash. <strong>US Market funds ($5.00 USD) are completely isolated and never used.</strong>
                </p>
              </div>

              <button
                onClick={handleResetIndiaMarket}
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
                🔄 {resetting ? "Resetting..." : "Reset to ₹1,000 Base"}
              </button>
            </div>

            {resetMsg && (
              <div style={{ marginBottom: "16px", padding: "10px 14px", background: "rgba(34, 197, 94, 0.15)", border: "1px solid rgba(34, 197, 94, 0.3)", borderRadius: "6px", color: "#4ade80", fontSize: "13px" }}>
                ✓ {resetMsg}
              </div>
            )}

            {/* Micro Capital Stat Tiles */}
            <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))", gap: "14px" }}>
              <div style={{ padding: "14px", background: "rgba(15, 23, 42, 0.7)", borderRadius: "8px", border: "1px solid rgba(255, 255, 255, 0.08)" }}>
                <div style={{ fontSize: "11px", color: "#94a3b8", fontWeight: "600", textTransform: "uppercase" }}>Initial Seed Capital</div>
                <div style={{ fontSize: "20px", fontWeight: "800", color: "#fff", marginTop: "4px" }}>₹{Number(baseCapitalInr).toLocaleString("en-IN", { minimumFractionDigits: 2 })} INR</div>
                <div style={{ fontSize: "11px", color: "#64748b", marginTop: "2px" }}>Fixed base allocation</div>
              </div>

              <div style={{ padding: "14px", background: "rgba(15, 23, 42, 0.7)", borderRadius: "8px", border: "1px solid rgba(255, 255, 255, 0.08)" }}>
                <div style={{ fontSize: "11px", color: "#94a3b8", fontWeight: "600", textTransform: "uppercase" }}>Realized Profit (Compounded)</div>
                <div style={{ fontSize: "20px", fontWeight: "800", color: Number(realizedProfitInr) >= 0 ? "#4ade80" : "#f87171", marginTop: "4px" }}>
                  {Number(realizedProfitInr) >= 0 ? `+₹${Number(realizedProfitInr).toFixed(2)}` : `-₹${Math.abs(Number(realizedProfitInr)).toFixed(2)}`} INR
                </div>
                <div style={{ fontSize: "11px", color: "#64748b", marginTop: "2px" }}>Available to re-trade</div>
              </div>

              <div style={{ padding: "14px", background: "rgba(15, 23, 42, 0.7)", borderRadius: "8px", border: "1px solid rgba(59, 130, 246, 0.3)" }}>
                <div style={{ fontSize: "11px", color: "#60a5fa", fontWeight: "700", textTransform: "uppercase" }}>Available Trading Cash</div>
                <div style={{ fontSize: "22px", fontWeight: "800", color: "#38bdf8", marginTop: "4px" }}>₹{Number(availableCashInr).toFixed(2)} INR</div>
                <div style={{ fontSize: "11px", color: "#93c5fd", marginTop: "2px" }}>Budget limit for BUY orders</div>
              </div>

              <div style={{ padding: "14px", background: "rgba(15, 23, 42, 0.7)", borderRadius: "8px", border: "1px solid rgba(255, 255, 255, 0.08)" }}>
                <div style={{ fontSize: "11px", color: "#94a3b8", fontWeight: "600", textTransform: "uppercase" }}>Tied Up in Open Shares</div>
                <div style={{ fontSize: "20px", fontWeight: "800", color: "#facc15", marginTop: "4px" }}>₹{Number(openPositionsCostInr).toFixed(2)} INR</div>
                <div style={{ fontSize: "11px", color: "#64748b", marginTop: "2px" }}>
                  {Object.entries(holdings).filter(([_, q]) => Number(q) > 0).map(([s, q]) => `${q}x ${s}`).join(", ") || "No open positions"}
                </div>
              </div>

              <div style={{ padding: "14px", background: "rgba(15, 23, 42, 0.7)", borderRadius: "8px", border: "1px solid rgba(34, 197, 94, 0.2)" }}>
                <div style={{ fontSize: "11px", color: "#4ade80", fontWeight: "600", textTransform: "uppercase" }}>Total Indian Equity</div>
                <div style={{ fontSize: "20px", fontWeight: "800", color: "#4ade80", marginTop: "4px" }}>₹{Number(totalEquityInr).toFixed(2)} INR</div>
                <div style={{ fontSize: "11px", color: "#64748b", marginTop: "2px" }}>Cash + Open Positions</div>
              </div>
            </div>
          </div>

          {/* Live Candlestick & Volume Market Chart (TradingView Mode matching Image 1) */}
          <div style={{ marginBottom: "20px" }}>
            <NseLiveChart initialSymbol={stratSymbol} onOrderPlaced={loadSandboxStatus} />
          </div>

          {/* Upstox Live Market Data Feed & Strategy Decision Engine */}
          <Card title="⚡ Upstox Live Market Feed → Agent Strategy → Local Paper Simulator">
            {/* Live Ticker Ribbon */}
            <div style={{ marginBottom: "16px" }}>
              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "8px" }}>
                <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
                  <span
                    style={{
                      width: "8px",
                      height: "8px",
                      borderRadius: "50%",
                      background: feedStatus?.connected ? "#10b981" : "#f59e0b",
                      display: "inline-block",
                      boxShadow: feedStatus?.connected ? "0 0 8px #10b981" : "0 0 6px #f59e0b",
                    }}
                  />
                  <span style={{ fontSize: "12px", fontWeight: "700", color: "#e2e8f0" }}>
                    {feedStatus?.connected
                      ? "UPSTOX LIVE FEED (WEBSOCKET/QUOTE)"
                      : feedStatus?.token_configured
                        ? "UPSTOX FEED (CONNECTING...)"
                        : "HIGH-FIDELITY LIVE DRIFT SIMULATOR"}
                  </span>
                  <span style={{ fontSize: "11px", color: "#64748b" }}>
                    · Updated: {feedStatus?.last_tick_time ? new Date(feedStatus.last_tick_time).toLocaleTimeString() : "Live"}
                  </span>
                </div>
                <Badge tone="good">Upstox Live Orders Blocked · Paper Simulator Active</Badge>
              </div>

              {/* Ticker Cards */}
              <div
                style={{
                  display: "grid",
                  gridTemplateColumns: "repeat(auto-fit, minmax(130px, 1fr))",
                  gap: "10px",
                }}
              >
                {Object.entries(feedStatus?.live_prices ?? {}).map(([sym, item]) => {
                  const chg = Number(item?.change_pct ?? 0);
                  const isUp = chg >= 0;
                  const itemPrice = Number(item?.price ?? 0);
                  const isSelected = stratSymbol === sym;
                  return (
                    <div
                      key={sym}
                      onClick={() => {
                        setStratSymbol(sym);
                        setStratPrice(itemPrice.toFixed(2));
                      }}
                      style={{
                        padding: "10px 12px",
                        background: isSelected
                          ? "rgba(59, 130, 246, 0.2)"
                          : "rgba(15, 23, 42, 0.6)",
                        border: isSelected
                          ? "1px solid #3b82f6"
                          : "1px solid rgba(255, 255, 255, 0.08)",
                        borderRadius: "8px",
                        cursor: "pointer",
                        transition: "all 0.15s ease",
                      }}
                    >
                      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                        <span style={{ fontSize: "12px", fontWeight: "700", color: "#fff" }}>{sym}</span>
                        <span
                          style={{
                            fontSize: "10px",
                            fontWeight: "600",
                            color: isUp ? "#4ade80" : "#f87171",
                          }}
                        >
                          {isUp ? `+${chg.toFixed(2)}%` : `${chg.toFixed(2)}%`}
                        </span>
                      </div>
                      <div style={{ fontSize: "14px", fontWeight: "800", color: "#38bdf8", marginTop: "4px" }}>
                        ₹{itemPrice.toFixed(2)}
                      </div>
                      <div style={{ fontSize: "10px", color: "#64748b", marginTop: "2px" }}>
                        {item?.source === "UPSTOX_LIVE" ? "🟢 Live Tick" : "🟡 Drift Tick"}
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>

            {/* Strategy Tester & Paper Order Submission Grid */}
            <div
              style={{
                display: "grid",
                gridTemplateColumns: "repeat(auto-fit, minmax(320px, 1fr))",
                gap: "16px",
                padding: "16px",
                background: "rgba(15, 23, 42, 0.5)",
                borderRadius: "8px",
                border: "1px solid rgba(255, 255, 255, 0.06)",
                marginBottom: "16px",
              }}
            >
              {/* Left Column: Live Strategy Evaluator */}
              <div>
                <h4 style={{ margin: "0 0 10px 0", fontSize: "14px", fontWeight: "700", color: "#f8fafc" }}>
                  1. Evaluate Strategy on Live Price Update
                </h4>
                <p style={{ margin: "0 0 12px 0", fontSize: "12px", color: "#94a3b8" }}>
                  Feeds the real-time quote to the agent's strategy decision engine. Evaluates Take-Profit (+2.0%), Stop-Loss (-1.5%), and ₹1,000 budget capacity.
                </p>

                <div style={{ display: "flex", gap: "10px", marginBottom: "12px" }}>
                  <div style={{ flex: 1 }}>
                    <label style={{ display: "block", fontSize: "11px", color: "#94a3b8", marginBottom: "4px" }}>
                      Instrument
                    </label>
                    <select
                      value={stratSymbol}
                      onChange={(e) => {
                        const s = e.target.value;
                        setStratSymbol(s);
                        const p = feedStatus?.live_prices?.[s]?.price;
                        if (p !== undefined) setStratPrice(Number(p).toFixed(2));
                      }}
                      style={{
                        width: "100%",
                        padding: "8px",
                        borderRadius: "6px",
                        border: "1px solid rgba(255, 255, 255, 0.15)",
                        background: "#0f172a",
                        color: "#fff",
                      }}
                    >
                      {Object.keys(feedStatus?.live_prices || { ITC: 1, SBIN: 1, RELIANCE: 1, INFY: 1, TATAMOTORS: 1 }).map((s) => (
                        <option key={s} value={s}>{s}</option>
                      ))}
                    </select>
                  </div>

                  <div style={{ flex: 1 }}>
                    <label style={{ display: "block", fontSize: "11px", color: "#94a3b8", marginBottom: "4px" }}>
                      Tick Price (INR)
                    </label>
                    <input
                      type="number"
                      step="0.05"
                      value={stratPrice}
                      placeholder={feedStatus?.live_prices?.[stratSymbol]?.price !== undefined ? Number(feedStatus.live_prices[stratSymbol].price).toFixed(2) : "480.00"}
                      onChange={(e) => setStratPrice(e.target.value)}
                      style={{
                        width: "100%",
                        padding: "8px",
                        borderRadius: "6px",
                        border: "1px solid rgba(255, 255, 255, 0.15)",
                        background: "#0f172a",
                        color: "#fff",
                      }}
                    />
                  </div>
                </div>

                <div style={{ display: "flex", gap: "8px" }}>
                  <button
                    type="button"
                    onClick={() => handleTestStrategyTick(false)}
                    disabled={stratExecuting}
                    style={{
                      flex: 1,
                      padding: "8px 14px",
                      background: "linear-gradient(135deg, #3b82f6 0%, #2563eb 100%)",
                      color: "#fff",
                      border: "none",
                      borderRadius: "6px",
                      fontWeight: "700",
                      fontSize: "12px",
                      cursor: stratExecuting ? "not-allowed" : "pointer",
                    }}
                  >
                    {stratExecuting ? "Evaluating..." : "Evaluate Decision (Dry Run)"}
                  </button>
                  <button
                    type="button"
                    onClick={() => handleTestStrategyTick(true)}
                    disabled={stratExecuting}
                    style={{
                      flex: 1,
                      padding: "8px 14px",
                      background: "linear-gradient(135deg, #10b981 0%, #059669 100%)",
                      color: "#fff",
                      border: "none",
                      borderRadius: "6px",
                      fontWeight: "700",
                      fontSize: "12px",
                      cursor: stratExecuting ? "not-allowed" : "pointer",
                    }}
                  >
                    Execute Paper Trade
                  </button>
                </div>

                {stratResult && (
                  <div
                    style={{
                      marginTop: "12px",
                      padding: "12px",
                      background:
                        stratResult.action === "BUY"
                          ? "rgba(16, 185, 129, 0.15)"
                          : stratResult.action === "SELL"
                            ? "rgba(239, 68, 68, 0.15)"
                            : "rgba(59, 130, 246, 0.15)",
                      border: `1px solid ${
                        stratResult.action === "BUY"
                          ? "#10b981"
                          : stratResult.action === "SELL"
                            ? "#ef4444"
                            : "#3b82f6"
                      }`,
                      borderRadius: "6px",
                    }}
                  >
                    <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                      <span
                        style={{
                          fontWeight: "800",
                          fontSize: "14px",
                          color:
                            stratResult.action === "BUY"
                              ? "#4ade80"
                              : stratResult.action === "SELL"
                                ? "#f87171"
                                : "#60a5fa",
                        }}
                      >
                        Signal: {stratResult.action} ({stratResult.quantity} shares of {stratResult.symbol})
                      </span>
                      <span style={{ fontSize: "11px", color: "#94a3b8" }}>
                        Confidence: {(stratResult.confidence * 100).toFixed(0)}%
                      </span>
                    </div>
                    <div style={{ fontSize: "12px", color: "#e2e8f0", marginTop: "4px" }}>
                      {stratResult.reason}
                    </div>
                    {stratResult.executed && (
                      <div
                        style={{
                          marginTop: "6px",
                          fontSize: "11px",
                          fontWeight: "700",
                          color: "#4ade80",
                        }}
                      >
                        ✓ Order recorded in local Paper Simulator database (Zero live Upstox orders).
                      </div>
                    )}
                  </div>
                )}
              </div>

              {/* Right Column: Upstox Developer App Authorization */}
              <div>
                <h4 style={{ margin: "0 0 10px 0", fontSize: "14px", fontWeight: "700", color: "#f8fafc" }}>
                  2. Upstox Developer API App Authorization
                </h4>
                <p style={{ margin: "0 0 12px 0", fontSize: "12px", color: "#94a3b8" }}>
                  Authorizes Upstox market data access for WebSocket feeds and live quotes. Orders are strictly routed to the paper simulator.
                </p>

                <form onSubmit={handleSaveUpstoxAuth}>
                  <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "10px", marginBottom: "8px" }}>
                    <div>
                      <label style={{ display: "block", fontSize: "11px", color: "#94a3b8", marginBottom: "2px" }}>
                        Client ID (API Key)
                      </label>
                      <input
                        type="text"
                        placeholder="e.g. 524e93fb-..."
                        value={clientIdInput}
                        onChange={(e) => setClientIdInput(e.target.value)}
                        style={{
                          width: "100%",
                          padding: "6px 8px",
                          borderRadius: "4px",
                          border: "1px solid rgba(255, 255, 255, 0.15)",
                          background: "#0f172a",
                          color: "#fff",
                          fontSize: "11px",
                        }}
                      />
                    </div>
                    <div>
                      <label style={{ display: "block", fontSize: "11px", color: "#94a3b8", marginBottom: "2px" }}>
                        Client Secret
                      </label>
                      <input
                        type="password"
                        placeholder="Upstox API Secret"
                        value={clientSecretInput}
                        onChange={(e) => setClientSecretInput(e.target.value)}
                        style={{
                          width: "100%",
                          padding: "6px 8px",
                          borderRadius: "4px",
                          border: "1px solid rgba(255, 255, 255, 0.15)",
                          background: "#0f172a",
                          color: "#fff",
                          fontSize: "11px",
                        }}
                      />
                    </div>
                  </div>

                  <div style={{ marginBottom: "10px" }}>
                    <label style={{ display: "block", fontSize: "11px", color: "#94a3b8", marginBottom: "2px" }}>
                      Direct Access Token (Optional)
                    </label>
                    <input
                      type="password"
                      placeholder="Paste daily Upstox access token directly..."
                      value={accessTokenInput}
                      onChange={(e) => setAccessTokenInput(e.target.value)}
                      style={{
                        width: "100%",
                        padding: "6px 8px",
                        borderRadius: "4px",
                        border: "1px solid rgba(255, 255, 255, 0.15)",
                        background: "#0f172a",
                        color: "#fff",
                        fontSize: "11px",
                      }}
                    />
                  </div>

                  <div style={{ display: "flex", gap: "8px", alignItems: "center" }}>
                    <button
                      type="submit"
                      disabled={authSaving}
                      style={{
                        padding: "6px 12px",
                        background: "linear-gradient(135deg, #475569 0%, #334155 100%)",
                        color: "#fff",
                        border: "none",
                        borderRadius: "4px",
                        fontWeight: "600",
                        fontSize: "11px",
                        cursor: authSaving ? "not-allowed" : "pointer",
                      }}
                    >
                      {authSaving ? "Saving..." : "Save Credentials"}
                    </button>

                    {feedStatus?.auth_dialog_url && (
                      <a
                        href={feedStatus.auth_dialog_url}
                        target="_blank"
                        rel="noreferrer"
                        style={{
                          padding: "6px 12px",
                          background: "linear-gradient(135deg, #8b5cf6 0%, #7c3aed 100%)",
                          color: "#fff",
                          textDecoration: "none",
                          borderRadius: "4px",
                          fontWeight: "600",
                          fontSize: "11px",
                          display: "inline-block",
                        }}
                      >
                        Authorize via Upstox Login ↗
                      </a>
                    )}
                  </div>

                  {authMsg && (
                    <div style={{ marginTop: "8px", fontSize: "11px", color: authMsg.includes("Error") ? "#f87171" : "#4ade80" }}>
                      {authMsg}
                    </div>
                  )}
                </form>
              </div>
            </div>
          </Card>

          <Card title="Upstox Sandbox Venue Status">
            <div className="grid grid--tiles">
              <StatTile
                hero
                label="Environment"
                value="SANDBOX"
                sub="Live order gateway blocked"
              />
              <StatTile
                label="Allowed Host"
                value="api.upstox.com"
                sub="Strict host allowlist enforced"
              />
              <StatTile
                label="Trading Session"
                value="NSE (09:15 - 15:30 IST)"
                sub="Asia/Kolkata timezone"
              />
              <StatTile
                label="Currency & Lot Sizing"
                value="INR (₹) / Whole shares"
                sub="No fractional shares on NSE"
              />
            </div>
          </Card>

          {/* Place Sandbox Test Order */}
          <Card title="Sandbox Order Submission Test">
            <form onSubmit={handleSandboxOrder}>
              <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))", gap: "1rem", marginBottom: "1rem" }}>
                <div>
                  <label style={{ display: "block", fontSize: "0.8rem", color: "var(--muted)", marginBottom: "0.25rem" }}>
                    Instrument (NSE Equity)
                  </label>
                  <select
                    value={orderSymbol}
                    onChange={(e) => {
                      const sym = e.target.value;
                      setOrderSymbol(sym);
                      if (NSE_DEFAULT_PRICES[sym]) {
                        setOrderPrice(NSE_DEFAULT_PRICES[sym]);
                      }
                    }}
                    style={{ width: "100%", padding: "0.4rem", borderRadius: "4px", border: "1px solid var(--border)", background: "var(--surface)" }}
                  >
                    {sandboxStatus?.watchlist.map((inst) => (
                      <option key={inst.symbol} value={inst.symbol}>
                        {inst.symbol} — {inst.name}
                      </option>
                    )) ?? <option value="ITC">ITC</option>}
                  </select>
                </div>
                <div>
                  <label style={{ display: "block", fontSize: "0.8rem", color: "var(--muted)", marginBottom: "0.25rem" }}>
                    Side
                  </label>
                  <select
                    value={orderSide}
                    onChange={(e) => setOrderSide(e.target.value as "BUY" | "SELL")}
                    style={{ width: "100%", padding: "0.4rem", borderRadius: "4px", border: "1px solid var(--border)", background: "var(--surface)" }}
                  >
                    <option value="BUY">BUY</option>
                    <option value="SELL">SELL</option>
                  </select>
                </div>
                <div>
                  <label style={{ display: "block", fontSize: "0.8rem", color: "var(--muted)", marginBottom: "0.25rem" }}>
                    Quantity (Whole Shares)
                  </label>
                  <input
                    type="number"
                    min="1"
                    step="1"
                    value={orderQty}
                    onChange={(e) => setOrderQty(e.target.value)}
                    style={{ width: "100%", padding: "0.4rem", borderRadius: "4px", border: "1px solid var(--border)", background: "var(--surface)" }}
                  />
                </div>
                <div>
                  <label style={{ display: "block", fontSize: "0.8rem", color: "var(--muted)", marginBottom: "0.25rem" }}>
                    Limit Price (INR)
                  </label>
                  <input
                    type="number"
                    step="0.05"
                    value={orderPrice}
                    onChange={(e) => setOrderPrice(e.target.value)}
                    style={{ width: "100%", padding: "0.4rem", borderRadius: "4px", border: "1px solid var(--border)", background: "var(--surface)" }}
                  />
                </div>
              </div>

              <div style={{ display: "flex", gap: "1rem", alignItems: "center" }}>
                <button
                  type="submit"
                  className="btn btn--primary"
                  disabled={orderSubmitting}
                  style={{ padding: "0.5rem 1.25rem" }}
                >
                  {orderSubmitting ? "Submitting to Sandbox..." : "Submit Test Order to Sandbox"}
                </button>
                <span style={{ fontSize: "0.8rem", color: "var(--muted)" }}>
                  Generates unique client order ID and tests mock/sandbox execution pipeline without real money.
                </span>
              </div>

              {orderSuccess && (
                <div style={{ marginTop: "1rem", padding: "0.75rem", background: "rgba(34, 197, 94, 0.1)", border: "1px solid var(--good)", borderRadius: "4px", color: "var(--good)", fontSize: "0.85rem" }}>
                  ✓ {orderSuccess}
                </div>
              )}
              {orderError && (
                <div style={{ marginTop: "1rem", padding: "0.75rem", background: "rgba(239, 68, 68, 0.1)", border: "1px solid var(--danger)", borderRadius: "4px", color: "var(--danger)", fontSize: "0.85rem" }}>
                  ✕ {orderError}
                </div>
              )}
            </form>
          </Card>

          {/* Recent Sandbox Orders */}
          <Card title="Sandbox Order Audit Trail">
            <div style={{ overflowX: "auto" }}>
              <table style={{ width: "100%", borderCollapse: "collapse", fontSize: "0.85rem", textAlign: "left" }}>
                <thead>
                  <tr style={{ borderBottom: "2px solid var(--border)", color: "var(--muted)" }}>
                    <th style={{ padding: "0.5rem" }}>Client Order ID</th>
                    <th style={{ padding: "0.5rem" }}>Venue Order ID</th>
                    <th style={{ padding: "0.5rem" }}>Symbol</th>
                    <th style={{ padding: "0.5rem" }}>Side</th>
                    <th style={{ padding: "0.5rem" }}>Quantity</th>
                    <th style={{ padding: "0.5rem" }}>Price (INR)</th>
                    <th style={{ padding: "0.5rem" }}>Status</th>
                    <th style={{ padding: "0.5rem" }}>Realized P&L (Profit)</th>
                    <th style={{ padding: "0.5rem" }}>Time (IST)</th>
                  </tr>
                </thead>
                <tbody>
                  {sandboxStatus?.recent_orders.map((o) => {
                    const match = indiaPnlMap.get(o.id);
                    const isSell = o.side === "SELL";
                    const pnl = match?.pnl ?? null;
                    const pnlPct = match?.pnlPct ?? null;
                    const isPos = (pnl ?? 0) > 0;
                    const isNeg = (pnl ?? 0) < 0;

                    return (
                      <tr key={o.id} style={{ borderBottom: "1px solid var(--border)" }}>
                        <td style={{ padding: "0.5rem", fontFamily: "monospace", fontSize: "0.8rem" }}>{o.client_order_id}</td>
                        <td style={{ padding: "0.5rem", fontFamily: "monospace", fontSize: "0.8rem" }}>{o.venue_order_id ?? DASH}</td>
                        <td style={{ padding: "0.5rem", fontWeight: 600 }}>{o.symbol}</td>
                        <td style={{ padding: "0.5rem" }}>
                          <span style={{ color: o.side === "BUY" ? "var(--good)" : "var(--danger)", fontWeight: 600 }}>
                            {o.side}
                          </span>
                        </td>
                        <td style={{ padding: "0.5rem" }}>{shares(o.quantity)}</td>
                        <td style={{ padding: "0.5rem" }}>{inr(parseFloat(o.price))}</td>
                        <td style={{ padding: "0.5rem" }}>
                          <Badge tone={o.status === "ACKNOWLEDGED" || o.status === "FILLED" ? "good" : "neutral"}>
                            {o.status}
                          </Badge>
                        </td>
                        <td style={{ padding: "0.5rem" }}>
                          {isSell ? (
                            pnl !== null ? (
                              <div>
                                <span style={{ fontWeight: 700, color: isPos ? "var(--good, #10b981)" : isNeg ? "var(--danger, #ef4444)" : "inherit" }}>
                                  {isPos ? `+₹${(Number(pnl) || 0).toFixed(2)}` : isNeg ? `-₹${Math.abs(Number(pnl) || 0).toFixed(2)}` : "₹0.00"}
                                </span>
                                {pnlPct !== null && (
                                  <div style={{ fontSize: "11px", fontWeight: 600, color: isPos ? "var(--good, #10b981)" : isNeg ? "var(--danger, #ef4444)" : "var(--muted)" }}>
                                    {isPos ? `+${(Number(pnlPct) || 0).toFixed(2)}%` : `${(Number(pnlPct) || 0).toFixed(2)}%`}
                                  </div>
                                )}
                              </div>
                            ) : (
                              <span className="muted">—</span>
                            )
                          ) : (
                            <span style={{ fontSize: "12px", color: "var(--muted)" }}>— (Open Entry)</span>
                          )}
                        </td>
                        <td style={{ padding: "0.5rem", whiteSpace: "nowrap" }}>{istTime(new Date(o.created_at))}</td>
                      </tr>
                    );
                  })}
                  {(!sandboxStatus?.recent_orders || sandboxStatus.recent_orders.length === 0) && (
                    <tr>
                      <td colSpan={9} style={{ padding: "1.5rem", textAlign: "center", color: "var(--muted)" }}>
                        No sandbox test orders placed yet.
                      </td>
                    </tr>
                  )}
                </tbody>
              </table>
            </div>
          </Card>
        </>
      )}

      {/* Tab 3: NSE Market Specs */}
      {activeTab === "specs" && (
        <>
          <Card title="Curated NSE Cash Equity Watchlist">
            <div style={{ overflowX: "auto" }}>
              <table style={{ width: "100%", borderCollapse: "collapse", fontSize: "0.85rem", textAlign: "left" }}>
                <thead>
                  <tr style={{ borderBottom: "2px solid var(--border)", color: "var(--muted)" }}>
                    <th style={{ padding: "0.5rem" }}>Symbol</th>
                    <th style={{ padding: "0.5rem" }}>Company Name</th>
                    <th style={{ padding: "0.5rem" }}>ISIN</th>
                    <th style={{ padding: "0.5rem" }}>Upstox Instrument Token</th>
                    <th style={{ padding: "0.5rem" }}>Tick Size</th>
                    <th style={{ padding: "0.5rem" }}>Lot Size</th>
                    <th style={{ padding: "0.5rem" }}>Fractional?</th>
                  </tr>
                </thead>
                <tbody>
                  {sandboxStatus?.watchlist.map((inst) => (
                    <tr key={inst.symbol} style={{ borderBottom: "1px solid var(--border)" }}>
                      <td style={{ padding: "0.5rem", fontWeight: 600 }}>{inst.symbol}</td>
                      <td style={{ padding: "0.5rem" }}>{inst.name}</td>
                      <td style={{ padding: "0.5rem", fontFamily: "monospace", fontSize: "0.8rem" }}>{inst.isin}</td>
                      <td style={{ padding: "0.5rem", fontFamily: "monospace", fontSize: "0.8rem" }}>{inst.upstox_token}</td>
                      <td style={{ padding: "0.5rem" }}>₹{inst.tick_size}</td>
                      <td style={{ padding: "0.5rem" }}>{inst.lot_size} share</td>
                      <td style={{ padding: "0.5rem" }}>
                        <Badge tone={inst.fractional ? "warning" : "good"}>
                          {inst.fractional ? "Allowed" : "Whole Only"}
                        </Badge>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </Card>

          <Card title="Statutory Indian Transaction Cost Structure">
            <div style={{ lineHeight: "1.6", fontSize: "0.875rem" }}>
              <p>All trades simulated or executed through the Indian testing framework incur date-effective statutory charges:</p>
              <ul style={{ paddingLeft: "1.25rem", marginTop: "0.5rem" }}>
                <li><strong>Securities Transaction Tax (STT):</strong> 0.1% on delivery Buy and Sell.</li>
                <li><strong>Brokerage:</strong> 0.05% capped at ₹20 per executed order.</li>
                <li><strong>NSE Turnover Charges:</strong> 0.00297% on turnover value.</li>
                <li><strong>SEBI Turnover Charges:</strong> ₹10 per crore (0.0001% of turnover).</li>
                <li><strong>Stamp Duty:</strong> 0.015% on Buy turnover.</li>
                <li><strong>Goods & Services Tax (GST):</strong> 18% applied to brokerage, turnover charges, and SEBI fees.</li>
                <li><strong>Investor Protection Fund Trust (IPFT):</strong> 0.0001% on turnover.</li>
              </ul>
              <p style={{ marginTop: "0.5rem", color: "var(--muted)", fontSize: "0.8rem" }}>
                Conservative execution simulation additionally includes a 5 bps bid/ask spread and 5 bps adverse slippage model.
              </p>
            </div>
          </Card>
        </>
      )}
    </div>
  );
}
