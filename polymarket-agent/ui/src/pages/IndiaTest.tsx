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
  recent_orders: SandboxOrder[];
  watchlist: Instrument[];
}

export function IndiaTest() {
  const [activeTab, setActiveTab] = useState<"live" | "sandbox" | "backtest" | "specs">("live");
  
  // Backtest form state
  const [initialCapital, setInitialCapital] = useState("500000");
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

  // Sandbox state
  const [sandboxStatus, setSandboxStatus] = useState<SandboxStatus | null>(null);
  const [orderSymbol, setOrderSymbol] = useState("RELIANCE");
  const [orderSide, setOrderSide] = useState<"BUY" | "SELL">("BUY");
  const [orderQty, setOrderQty] = useState("10");
  const [orderPrice, setOrderPrice] = useState("2500");
  const [orderSubmitting, setOrderSubmitting] = useState(false);
  const [orderError, setOrderError] = useState<string | null>(null);
  const [orderSuccess, setOrderSuccess] = useState<string | null>(null);

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
      }
    } catch (e) {
      console.error("Failed to load sandbox status", e);
    }
  };

  useEffect(() => {
    loadSandboxStatus();
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
            style={{ fontSize: "0.825rem", padding: "0.35rem 0.75rem" }}
          >
            Upstox Sandbox Fills
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
                    onChange={(e) => setOrderSymbol(e.target.value)}
                    style={{ width: "100%", padding: "0.4rem", borderRadius: "4px", border: "1px solid var(--border)", background: "var(--surface)" }}
                  >
                    {sandboxStatus?.watchlist.map((inst) => (
                      <option key={inst.symbol} value={inst.symbol}>
                        {inst.symbol} — {inst.name}
                      </option>
                    )) ?? <option value="RELIANCE">RELIANCE</option>}
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
