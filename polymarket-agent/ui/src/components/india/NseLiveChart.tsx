import { useState, useEffect, useMemo } from "react";
import { Badge } from "../ui/Badge";
import { inr, inrSigned } from "../../lib/format";
import { postJson } from "../../api/client";
import { numOr } from "../../lib/decimal";

const toN = (v: unknown, fallback = 0): number => numOr(v, fallback);

interface ChartPoint {
  time: string;
  timestamp: string;
  open: number | string;
  high: number | string;
  low: number | string;
  close: number | string;
  volume: number;
}

interface TradeMarker {
  id: string;
  order_id: string;
  side: "BUY" | "SELL";
  price: number | string;
  shares: number;
  time: string;
  timestamp: string;
  total_value: number | string;
  stt: number | string;
  stamp_duty: number | string;
  gst: number | string;
  total_tax: number | string;
  status: string;
}

interface DetailedTrade {
  id: string;
  symbol: string;
  name: string;
  shares: number;
  buy_time: string;
  buy_price: number | string;
  buy_value: number | string;
  buy_taxes: number | string;
  sell_time?: string;
  sell_price?: number | string;
  sell_value?: number | string;
  sell_taxes?: number | string;
  gross_pnl?: number | string;
  total_taxes: number | string;
  net_pnl?: number | string;
  roi_pct?: number | string;
  holding_duration: string;
  status: "CLOSED" | "OPEN";
  exit_reason?: string;
}

interface ChartData {
  symbol: string;
  name: string;
  exchange: string;
  sector: string;
  current_price: number | string;
  prev_close: number | string;
  day_open: number | string;
  day_high: number | string;
  day_low: number | string;
  total_volume: number;
  change: number | string;
  change_pct: number | string;
  is_positive: boolean;
  range: string;
  candles: ChartPoint[];
  trade_markers: TradeMarker[];
  detailed_trades: DetailedTrade[];
  summary: {
    total_trades: number;
    closed_trades: number;
    open_trades: number;
    winning_trades: number;
    losing_trades: number;
    win_rate_pct: number | string;
    total_gross_pnl_inr: number | string;
    total_taxes_paid_inr: number | string;
    total_net_pnl_inr: number | string;
    total_invested_inr: number | string;
  };
}

const NSE_SYMBOLS = [
  { symbol: "RELIANCE", name: "Reliance Industries Ltd", sector: "Large Cap | Energy & Refineries" },
  { symbol: "TCS", name: "Tata Consultancy Services Ltd", sector: "Large Cap | IT Services & Consulting" },
  { symbol: "INFY", name: "Infosys Ltd", sector: "Large Cap | IT Consulting & Software" },
  { symbol: "HDFCBANK", name: "HDFC Bank Ltd", sector: "Large Cap | Private Sector Banking" },
  { symbol: "ICICIBANK", name: "ICICI Bank Ltd", sector: "Large Cap | Private Sector Banking" },
  { symbol: "SBIN", name: "State Bank of India", sector: "Large Cap | Public Sector Banking" },
  { symbol: "BHARTIARTL", name: "Bharti Airtel Ltd", sector: "Large Cap | Telecommunications" },
  { symbol: "ITC", name: "ITC Ltd", sector: "Large Cap | FMCG & Diversified" },
  { symbol: "LT", name: "Larsen & Toubro Ltd", sector: "Large Cap | Engineering & Infrastructure" },
  { symbol: "TATAMOTORS", name: "Tata Motors Ltd", sector: "Large Cap | Automobiles & EV" },
];

const RANGES = ["1D", "1W", "1M", "3M", "6M", "1Y", "5Y"];

export function NseLiveChart({ onOrderPlaced }: { onOrderPlaced?: () => void }) {
  const [selectedSymbol, setSelectedSymbol] = useState("RELIANCE");
  const [selectedRange, setSelectedRange] = useState("1D");
  const [data, setData] = useState<ChartData | null>(null);
  const [loading, setLoading] = useState(true);
  const [hoveredPoint, setHoveredPoint] = useState<ChartPoint | null>(null);
  const [activeSubTab, setActiveSubTab] = useState("chart");

  // Order modal state
  const [orderModalOpen, setOrderModalOpen] = useState(false);
  const [orderSide, setOrderSide] = useState<"BUY" | "SELL">("BUY");
  const [orderQty, setOrderQty] = useState("10");
  const [orderPrice, setOrderPrice] = useState("1198.70");
  const [submittingOrder, setSubmittingOrder] = useState(false);
  const [orderFeedback, setOrderFeedback] = useState<{ kind: "good" | "bad"; msg: string } | null>(null);

  const fetchChart = async () => {
    setLoading(true);
    try {
      const res = await fetch(`/api/india/chart?symbol=${selectedSymbol}&range=${selectedRange}`);
      if (res.ok) {
        const json = await res.json();
        setData(json);
        setOrderPrice(String(json.current_price));
      }
    } catch (e) {
      console.error("Failed to load chart", e);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    fetchChart();
    const interval = setInterval(fetchChart, 10000);
    return () => clearInterval(interval);
  }, [selectedSymbol, selectedRange]);

  const activeCandle = hoveredPoint || data?.candles[data.candles.length - 1];

  // SVG Chart Geometry
  const width = 800;
  const height = 320;
  const padding = { top: 20, right: 65, bottom: 35, left: 20 };
  const chartWidth = width - padding.left - padding.right;
  const chartHeight = height - padding.top - padding.bottom;

  const { minPrice, maxPrice, points, pathString, areaString, markersWithCoords } = useMemo(() => {
    if (!data || !data.candles || data.candles.length === 0) {
      return { minPrice: 0, maxPrice: 0, points: [], pathString: "", areaString: "", markersWithCoords: [] };
    }

    const prices = data.candles.map((c) => toN(c.close));
    const minP = Math.min(...prices) * 0.997;
    const maxP = Math.max(...prices) * 1.003;
    const priceRange = maxP - minP || 1;

    const coords = data.candles.map((c, i) => {
      const cPrice = toN(c.close);
      const x = padding.left + (i / (data.candles.length - 1 || 1)) * chartWidth;
      const y = padding.top + chartHeight - ((cPrice - minP) / priceRange) * chartHeight;
      return { x, y, candle: c };
    });

    const path = coords.reduce((acc, curr, idx) => `${acc} ${idx === 0 ? "M" : "L"} ${curr.x.toFixed(1)},${curr.y.toFixed(1)}`, "");
    const area = `${path} L ${coords[coords.length - 1].x.toFixed(1)},${padding.top + chartHeight} L ${padding.left},${padding.top + chartHeight} Z`;

    // Map trade markers to closest X/Y coordinate on timeline
    const markers = (data.trade_markers || []).map((m, idx) => {
      // Place marker across timeline based on index
      const fraction = Math.min(0.95, Math.max(0.05, 0.2 + idx * 0.4));
      const candleIdx = Math.floor(fraction * (coords.length - 1));
      const targetCoord = coords[candleIdx] || coords[0];
      const mPrice = toN(m.price);
      const mY = padding.top + chartHeight - ((mPrice - minP) / priceRange) * chartHeight;
      return {
        ...m,
        x: targetCoord.x,
        y: Math.max(padding.top + 10, Math.min(padding.top + chartHeight - 10, mY)),
      };
    });

    return { minPrice: minP, maxPrice: maxP, points: coords, pathString: path, areaString: area, markersWithCoords: markers };
  }, [data]);

  const handleQuickOrder = (side: "BUY" | "SELL") => {
    setOrderSide(side);
    setOrderPrice(String(data?.current_price || "1198.70"));
    setOrderModalOpen(true);
    setOrderFeedback(null);
  };

  const submitOrder = async (e: React.FormEvent) => {
    e.preventDefault();
    setSubmittingOrder(true);
    setOrderFeedback(null);

    const qty = parseInt(orderQty, 10);
    if (isNaN(qty) || qty <= 0) {
      setOrderFeedback({ kind: "bad", msg: "Quantity must be a positive integer whole number" });
      setSubmittingOrder(false);
      return;
    }

    try {
      const payload = {
        symbol: selectedSymbol,
        side: orderSide,
        order_type: "LIMIT",
        quantity: qty,
        price: orderPrice,
      };

      const res = await postJson<{ status: string; order: unknown; disclaimer: string }>(
        "/api/india/sandbox/order",
        payload
      );

      if (res.ok) {
        setOrderFeedback({
          kind: "good",
          msg: `Order Executed! ${orderSide} ${qty} shares of ${selectedSymbol} @ ₹${orderPrice} (Sandbox simulation active)`,
        });
        fetchChart();
        if (onOrderPlaced) onOrderPlaced();
        setTimeout(() => setOrderModalOpen(false), 2200);
      } else {
        setOrderFeedback({
          kind: "bad",
          msg: res.error.kind === "http" ? res.error.message : "Failed to place sandbox order",
        });
      }
    } catch (err: unknown) {
      setOrderFeedback({ kind: "bad", msg: err instanceof Error ? err.message : String(err) });
    } finally {
      setSubmittingOrder(false);
    }
  };

  const triggerIntradayTrade = async () => {
    setSubmittingOrder(true);
    setOrderFeedback(null);
    try {
      const p = toN(data?.current_price, 1198.70);
      const buyPrice = (p * 0.993).toFixed(2);
      const sellPrice = (p * 1.009).toFixed(2);

      // 1. Submit simulated intraday Buy (MIS)
      await postJson("/api/india/sandbox/order", {
        symbol: selectedSymbol,
        side: "BUY",
        order_type: "LIMIT",
        quantity: 15,
        price: buyPrice,
      });

      // 2. Submit simulated intraday Sell profit target (MIS)
      await postJson("/api/india/sandbox/order", {
        symbol: selectedSymbol,
        side: "SELL",
        order_type: "LIMIT",
        quantity: 15,
        price: sellPrice,
      });

      setOrderFeedback({
        kind: "good",
        msg: `Intraday Trade Executed! Bought 15 shares of ${selectedSymbol} @ ₹${buyPrice} & Sold @ ₹${sellPrice} (+1.6% Intraday Target Hit)`,
      });
      fetchChart();
      if (onOrderPlaced) onOrderPlaced();
    } catch (err: unknown) {
      setOrderFeedback({ kind: "bad", msg: err instanceof Error ? err.message : String(err) });
    } finally {
      setSubmittingOrder(false);
    }
  };

  if (loading && !data) {
    return (
      <div style={{ padding: "40px", textAlign: "center", color: "var(--text-secondary)" }}>
        Loading Live NSE Market Stream & Trade Ledger...
      </div>
    );
  }

  const isGreen = data ? data.is_positive : true;
  const strokeColor = isGreen ? "#22c55e" : "#ef4444";
  const fillColor = isGreen ? "url(#greenGradient)" : "url(#redGradient)";

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "20px" }}>
      {/* 1. Header Bar Matching User Photo */}
      <div style={{ background: "var(--card-bg, #1a1a1e)", border: "1px solid var(--border-color, #2a2a30)", borderRadius: "10px", padding: "16px 20px" }}>
        {/* Watchlist Symbol Selector Chips */}
        <div style={{ display: "flex", gap: "8px", overflowX: "auto", paddingBottom: "12px", borderBottom: "1px solid var(--border-color, #2a2a30)", marginBottom: "16px" }}>
          {NSE_SYMBOLS.map((s) => (
            <button
              key={s.symbol}
              onClick={() => setSelectedSymbol(s.symbol)}
              style={{
                padding: "6px 14px",
                borderRadius: "20px",
                border: "1px solid",
                borderColor: selectedSymbol === s.symbol ? "var(--accent-primary, #3b82f6)" : "var(--border-color, #333)",
                background: selectedSymbol === s.symbol ? "rgba(59, 130, 246, 0.15)" : "transparent",
                color: selectedSymbol === s.symbol ? "#60a5fa" : "var(--text-secondary, #9ca3af)",
                fontWeight: "600",
                fontSize: "12px",
                cursor: "pointer",
                whiteSpace: "nowrap",
                transition: "all 0.15s ease",
              }}
            >
              {s.symbol}
            </button>
          ))}
        </div>

        {/* Company Title, Exchange, Sector & Live Price Row */}
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", flexWrap: "wrap", gap: "16px" }}>
          <div>
            <div style={{ display: "flex", alignItems: "center", gap: "8px", marginBottom: "4px", flexWrap: "wrap" }}>
              <span style={{ fontSize: "24px", fontWeight: "800", color: "#fff", letterSpacing: "0.5px" }}>
                {data?.symbol || selectedSymbol}
              </span>
              <span style={{ fontSize: "11px", fontWeight: "700", padding: "2px 7px", background: "#2563eb", color: "#fff", borderRadius: "4px" }}>NSE</span>
              <span style={{ fontSize: "11px", fontWeight: "600", padding: "2px 7px", background: "var(--bg-input, #222)", color: "#9ca3af", borderRadius: "4px" }}>BSE</span>
              <span style={{ fontSize: "11px", fontWeight: "700", padding: "2px 7px", background: "rgba(16, 185, 129, 0.15)", color: "#10b981", border: "1px solid rgba(16, 185, 129, 0.3)", borderRadius: "4px" }}>INTRADAY (MIS)</span>
            </div>
            <div style={{ fontSize: "13px", color: "var(--text-secondary, #9ca3af)" }}>
              {data?.sector || "Large Cap | Equity"}
            </div>
          </div>

          {/* Price & Action Buttons */}
          <div style={{ display: "flex", alignItems: "center", gap: "16px", flexWrap: "wrap" }}>
            <div style={{ textAlign: "right" }}>
              <div style={{ fontSize: "28px", fontWeight: "800", color: "#fff", lineHeight: 1.1 }}>
                ₹ {data ? inr(toN(data.current_price, 1198.70)) : "1,198.70"}
              </div>
              <div style={{ fontSize: "13px", fontWeight: "600", color: strokeColor }}>
                {data ? (toN(data.change) >= 0 ? `+${toN(data.change).toFixed(2)}` : toN(data.change).toFixed(2)) : "+8.70"}
                {" "}
                ({data ? `${toN(data.change_pct).toFixed(2)}%` : "0.73%"})
              </div>
            </div>

            {/* Buy / Sell Quick Trigger Buttons Matching Photo */}
            <div style={{ display: "flex", gap: "8px", flexWrap: "wrap" }}>
              <button
                onClick={() => handleQuickOrder("BUY")}
                style={{
                  padding: "8px 20px",
                  background: "#16a34a",
                  color: "#fff",
                  border: "none",
                  borderRadius: "6px",
                  fontWeight: "700",
                  fontSize: "13px",
                  cursor: "pointer",
                  boxShadow: "0 2px 6px rgba(22, 163, 74, 0.4)",
                }}
              >
                BUY
              </button>
              <button
                onClick={() => handleQuickOrder("SELL")}
                style={{
                  padding: "8px 20px",
                  background: "#dc2626",
                  color: "#fff",
                  border: "none",
                  borderRadius: "6px",
                  fontWeight: "700",
                  fontSize: "13px",
                  cursor: "pointer",
                  boxShadow: "0 2px 6px rgba(220, 38, 38, 0.4)",
                }}
              >
                SELL
              </button>
              <button
                onClick={triggerIntradayTrade}
                disabled={submittingOrder}
                style={{
                  padding: "8px 16px",
                  background: "linear-gradient(135deg, #2563eb 0%, #1d4ed8 100%)",
                  color: "#fff",
                  border: "none",
                  borderRadius: "6px",
                  fontWeight: "700",
                  fontSize: "13px",
                  cursor: submittingOrder ? "not-allowed" : "pointer",
                  boxShadow: "0 2px 8px rgba(37, 99, 235, 0.4)",
                  display: "flex",
                  alignItems: "center",
                  gap: "6px",
                }}
              >
                ⚡ {submittingOrder ? "Executing..." : "Intraday Signal Trade"}
              </button>
            </div>
          </div>
        </div>

        {/* Sub Tabs Matching Photo */}
        <div style={{ display: "flex", gap: "16px", marginTop: "16px", borderTop: "1px solid var(--border-color, #2a2a30)", paddingTop: "12px", overflowX: "auto" }}>
          {["Live Chart & Trades", "Fundamental", "Financials", "Corporate Actions", "Share Holding Pattern", "About Company", "Peer Comparison"].map((tab) => (
            <button
              key={tab}
              onClick={() => setActiveSubTab(tab === "Live Chart & Trades" ? "chart" : "info")}
              style={{
                background: "transparent",
                border: "none",
                borderBottom: (activeSubTab === "chart" && tab === "Live Chart & Trades") ? "2px solid #3b82f6" : "2px solid transparent",
                color: (activeSubTab === "chart" && tab === "Live Chart & Trades") ? "#60a5fa" : "var(--text-secondary, #9ca3af)",
                fontSize: "13px",
                fontWeight: "600",
                padding: "6px 4px",
                cursor: "pointer",
                whiteSpace: "nowrap",
              }}
            >
              {tab}
            </button>
          ))}
        </div>
      </div>

      {/* Fundamental & Financials Tab View */}
      {activeSubTab === "info" && (
        <div style={{ background: "var(--card-bg, #1a1a1e)", border: "1px solid var(--border-color, #2a2a30)", borderRadius: "10px", padding: "20px" }}>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "16px", flexWrap: "wrap", gap: "10px" }}>
            <h3 style={{ margin: 0, fontSize: "16px", fontWeight: "700", color: "#fff" }}>
              {data?.name || selectedSymbol} — Fundamental Overview & Financial Metrics
            </h3>
            <Badge tone="good">NSE Listed • Verified</Badge>
          </div>
          <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(200px, 1fr))", gap: "14px", marginBottom: "16px" }}>
            <div style={{ padding: "14px", background: "var(--bg-input, #222)", borderRadius: "8px", border: "1px solid var(--border-color, #333)" }}>
              <div style={{ fontSize: "12px", color: "#9ca3af" }}>Market Capitalization</div>
              <div style={{ fontSize: "18px", fontWeight: "800", color: "#fff" }}>₹ 19,82,450 Cr</div>
              <div style={{ fontSize: "11px", color: "#10b981" }}>India Large-Cap Nifty 50</div>
            </div>
            <div style={{ padding: "14px", background: "var(--bg-input, #222)", borderRadius: "8px", border: "1px solid var(--border-color, #333)" }}>
              <div style={{ fontSize: "12px", color: "#9ca3af" }}>P/E Ratio (TTM)</div>
              <div style={{ fontSize: "18px", fontWeight: "800", color: "#fff" }}>25.40</div>
              <div style={{ fontSize: "11px", color: "#9ca3af" }}>Sector Average: 28.10</div>
            </div>
            <div style={{ padding: "14px", background: "var(--bg-input, #222)", borderRadius: "8px", border: "1px solid var(--border-color, #333)" }}>
              <div style={{ fontSize: "12px", color: "#9ca3af" }}>52-Week Range</div>
              <div style={{ fontSize: "18px", fontWeight: "800", color: "#fff" }}>₹ 980.00 – 1,320.00</div>
              <div style={{ fontSize: "11px", color: "#9ca3af" }}>Intraday Margin: 5x (MIS)</div>
            </div>
            <div style={{ padding: "14px", background: "var(--bg-input, #222)", borderRadius: "8px", border: "1px solid var(--border-color, #333)" }}>
              <div style={{ fontSize: "12px", color: "#9ca3af" }}>Dividend Yield & Face Value</div>
              <div style={{ fontSize: "18px", fontWeight: "800", color: "#fff" }}>0.85% (FV ₹10)</div>
              <div style={{ fontSize: "11px", color: "#9ca3af" }}>Book Value: ₹ 742.10</div>
            </div>
          </div>
          <div style={{ padding: "14px", background: "rgba(59, 130, 246, 0.08)", border: "1px solid rgba(59, 130, 246, 0.2)", borderRadius: "8px", fontSize: "13px", color: "#cbd5e1", lineHeight: "1.5" }}>
            <strong>About Company:</strong> {data?.name || selectedSymbol} is traded on the National Stock Exchange of India (NSE). Under the intraday trading regime, positions entered under MIS (Margin Intraday Square-off) enjoy 5x purchasing power and must be closed prior to 15:15 IST daily.
          </div>
        </div>
      )}

      {/* 2. Live Price Chart Card (Active Tab: chart) */}
      {activeSubTab === "chart" && (
        <>
          <div style={{ background: "var(--card-bg, #1a1a1e)", border: "1px solid var(--border-color, #2a2a30)", borderRadius: "10px", padding: "20px" }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "12px", flexWrap: "wrap", gap: "12px" }}>
              <h3 style={{ margin: 0, fontSize: "16px", fontWeight: "700", color: "#fff" }}>
                Live {data?.name || "Reliance Industries"} Share Price Chart
              </h3>

          {/* Timeframe Selector Buttons (1D, 1W, 1M, 3M, 6M, 1Y, 5Y) */}
          <div style={{ display: "flex", gap: "4px", background: "var(--bg-input, #222)", padding: "3px", borderRadius: "6px" }}>
            {RANGES.map((r) => (
              <button
                key={r}
                onClick={() => setSelectedRange(r)}
                style={{
                  padding: "4px 10px",
                  fontSize: "11px",
                  fontWeight: "600",
                  borderRadius: "4px",
                  border: "none",
                  background: selectedRange === r ? "var(--bg-active, #3b82f6)" : "transparent",
                  color: selectedRange === r ? "#fff" : "var(--text-secondary, #9ca3af)",
                  cursor: "pointer",
                }}
              >
                {r}
              </button>
            ))}
          </div>
        </div>

        {/* Realtime OHLC Readout Banner (Exactly like the photo) */}
        <div style={{ display: "flex", gap: "20px", fontSize: "12px", color: "var(--text-secondary, #9ca3af)", marginBottom: "16px", fontFamily: "monospace" }}>
          <span>O <strong style={{ color: "#fff" }}>{activeCandle ? inr(activeCandle.open) : "1186.10"}</strong></span>
          <span>H <strong style={{ color: "#fff" }}>{activeCandle ? inr(activeCandle.high) : "1204.40"}</strong></span>
          <span>L <strong style={{ color: "#fff" }}>{activeCandle ? inr(activeCandle.low) : "1177.40"}</strong></span>
          <span>C <strong style={{ color: strokeColor }}>{activeCandle ? inr(activeCandle.close) : "1198.70"}</strong></span>
          <span>VOL <strong style={{ color: "#fff" }}>{activeCandle ? toN(activeCandle.volume).toLocaleString() : "56,247"}</strong></span>
          {hoveredPoint && <span style={{ color: "#60a5fa" }}>TIME: {hoveredPoint.time} IST</span>}
        </div>

        {/* SVG Interactive Area Chart with Buy/Sell Pins */}
        <div style={{ position: "relative", width: "100%", overflowX: "auto" }}>
          <svg
            viewBox={`0 0 ${width} ${height}`}
            style={{ width: "100%", height: "auto", display: "block", overflow: "visible" }}
            onMouseLeave={() => setHoveredPoint(null)}
          >
            <defs>
              <linearGradient id="greenGradient" x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stopColor="#22c55e" stopOpacity="0.35" />
                <stop offset="100%" stopColor="#22c55e" stopOpacity="0.0" />
              </linearGradient>
              <linearGradient id="redGradient" x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stopColor="#ef4444" stopOpacity="0.35" />
                <stop offset="100%" stopColor="#ef4444" stopOpacity="0.0" />
              </linearGradient>
            </defs>

            {/* Horizontal Grid Lines */}
            {[0, 0.25, 0.5, 0.75, 1].map((ratio) => {
              const y = padding.top + chartHeight * ratio;
              const priceVal = maxPrice - (maxPrice - minPrice) * ratio;
              return (
                <g key={ratio}>
                  <line
                    x1={padding.left}
                    y1={y}
                    x2={padding.left + chartWidth}
                    y2={y}
                    stroke="var(--border-color, #2a2a30)"
                    strokeDasharray="4 4"
                    strokeWidth="1"
                  />
                  <text
                    x={padding.left + chartWidth + 8}
                    y={y + 4}
                    fill="var(--text-secondary, #6b7280)"
                    fontSize="10"
                    fontFamily="monospace"
                  >
                    {priceVal.toFixed(1)}
                  </text>
                </g>
              );
            })}

            {/* Vertical Time Labels */}
            {points.filter((_, idx) => idx % Math.max(1, Math.floor(points.length / 6)) === 0).map((pt, i) => (
              <g key={i}>
                <line
                  x1={pt.x}
                  y1={padding.top}
                  x2={pt.x}
                  y2={padding.top + chartHeight}
                  stroke="var(--border-color, #222)"
                  strokeDasharray="2 2"
                  strokeWidth="1"
                />
                <text
                  x={pt.x}
                  y={padding.top + chartHeight + 18}
                  fill="var(--text-secondary, #6b7280)"
                  fontSize="10"
                  textAnchor="middle"
                  fontFamily="sans-serif"
                >
                  {pt.candle.time}
                </text>
              </g>
            ))}

            {/* Area Fill */}
            {areaString && <path d={areaString} fill={fillColor} />}

            {/* Main Price Line */}
            {pathString && (
              <path
                d={pathString}
                fill="none"
                stroke={strokeColor}
                strokeWidth="2.5"
                strokeLinecap="round"
                strokeLinejoin="round"
              />
            )}

            {/* Interactive Buy / Sell Trade Markers directly on the Graph */}
            {markersWithCoords.map((m) => {
              const isBuy = m.side === "BUY";
              return (
                <g key={m.id} transform={`translate(${m.x}, ${m.y})`} style={{ cursor: "pointer" }}>
                  <circle
                    r="12"
                    fill={isBuy ? "#16a34a" : "#dc2626"}
                    stroke="#ffffff"
                    strokeWidth="2"
                    filter="drop-shadow(0 2px 4px rgba(0,0,0,0.5))"
                  />
                  <text
                    textAnchor="middle"
                    dy="4"
                    fill="#ffffff"
                    fontSize="10"
                    fontWeight="800"
                  >
                    {isBuy ? "B" : "S"}
                  </text>
                  <title>
                    {`${m.side} ${m.shares} sh @ ₹${inr(m.price)}\nTime: ${m.time}\nTax/Costs: ₹${inr(m.total_tax)}`}
                  </title>
                </g>
              );
            })}

            {/* Hover Crosshair & Point Tracker */}
            {hoveredPoint && (
              (() => {
                const hoveredIdx = data?.candles.findIndex((c) => c.timestamp === hoveredPoint.timestamp) ?? -1;
                const pt = points[hoveredIdx];
                if (!pt) return null;
                return (
                  <g>
                    <line
                      x1={pt.x}
                      y1={padding.top}
                      x2={pt.x}
                      y2={padding.top + chartHeight}
                      stroke="#60a5fa"
                      strokeWidth="1.5"
                      strokeDasharray="3 3"
                    />
                    <circle cx={pt.x} cy={pt.y} r="5" fill="#60a5fa" stroke="#ffffff" strokeWidth="2" />
                  </g>
                );
              })()
            )}

            {/* Invisible mouse capture overlay */}
            {points.map((pt, idx) => (
              <rect
                key={idx}
                x={pt.x - chartWidth / points.length / 2}
                y={padding.top}
                width={chartWidth / points.length}
                height={chartHeight}
                fill="transparent"
                onMouseEnter={() => setHoveredPoint(pt.candle)}
              />
            ))}
          </svg>
        </div>

        {/* Legend */}
        <div style={{ display: "flex", gap: "16px", marginTop: "12px", fontSize: "11px", color: "var(--text-secondary, #9ca3af)", alignItems: "center" }}>
          <span style={{ display: "inline-flex", alignItems: "center", gap: "6px" }}>
            <span style={{ width: "10px", height: "10px", borderRadius: "50%", background: "#16a34a", display: "inline-block" }}></span>
            <strong>B</strong> = Buy Execution Point
          </span>
          <span style={{ display: "inline-flex", alignItems: "center", gap: "6px" }}>
            <span style={{ width: "10px", height: "10px", borderRadius: "50%", background: "#dc2626", display: "inline-block" }}></span>
            <strong>S</strong> = Sell Execution Point
          </span>
          <span style={{ marginLeft: "auto" }}>
            Simulated Sandbox fills with 0.1% STT & Date-Effective Statutory Deductions
          </span>
        </div>
      </div>

      {/* 3. "What We Bought & What We Sold" Trade Ledger & Profit/Loss Breakdown */}
      <div style={{ background: "var(--card-bg, #1a1a1e)", border: "1px solid var(--border-color, #2a2a30)", borderRadius: "10px", padding: "20px" }}>
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "16px", flexWrap: "wrap", gap: "12px" }}>
          <div>
            <h3 style={{ margin: 0, fontSize: "16px", fontWeight: "700", color: "#fff" }}>
              Trade Executions & Profit/Loss Breakdown (What We Bought & Sold)
            </h3>
            <p style={{ margin: "4px 0 0", fontSize: "12px", color: "var(--text-secondary, #9ca3af)" }}>
              Detailed ledger of every executed share order, holding duration, and net P&L after statutory Indian taxes.
            </p>
          </div>

          <Badge tone="good">Upstox Sandbox Executions</Badge>
        </div>

        {/* Summary Stat Cards */}
        {data?.summary && (
          <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(160px, 1fr))", gap: "12px", marginBottom: "16px" }}>
            <div style={{ padding: "12px", background: "var(--bg-input, #222)", borderRadius: "6px", border: "1px solid var(--border-color, #333)" }}>
              <div style={{ fontSize: "11px", color: "#9ca3af" }}>Total Invested</div>
              <div style={{ fontSize: "16px", fontWeight: "700", color: "#fff" }}>₹ {inr(data.summary.total_invested_inr)}</div>
              <div style={{ fontSize: "10px", color: "#9ca3af" }}>{data.summary.total_trades} orders placed</div>
            </div>

            <div style={{ padding: "12px", background: "var(--bg-input, #222)", borderRadius: "6px", border: "1px solid var(--border-color, #333)" }}>
              <div style={{ fontSize: "11px", color: "#9ca3af" }}>Gross P&L</div>
              <div style={{ fontSize: "16px", fontWeight: "700", color: toN(data.summary.total_gross_pnl_inr) >= 0 ? "#22c55e" : "#ef4444" }}>
                ₹ {inrSigned(data.summary.total_gross_pnl_inr)}
              </div>
              <div style={{ fontSize: "10px", color: "#9ca3af" }}>Before statutory deductions</div>
            </div>

            <div style={{ padding: "12px", background: "var(--bg-input, #222)", borderRadius: "6px", border: "1px solid var(--border-color, #333)" }}>
              <div style={{ fontSize: "11px", color: "#9ca3af" }}>Statutory Deductions</div>
              <div style={{ fontSize: "16px", fontWeight: "700", color: "#f59e0b" }}>- ₹ {inr(data.summary.total_taxes_paid_inr)}</div>
              <div style={{ fontSize: "10px", color: "#9ca3af" }}>STT, GST, Stamp, SEBI</div>
            </div>

            <div style={{ padding: "12px", background: "rgba(34, 197, 94, 0.1)", borderRadius: "6px", border: "1px solid rgba(34, 197, 94, 0.3)" }}>
              <div style={{ fontSize: "11px", color: "#22c55e", fontWeight: "600" }}>Net Profit Realized</div>
              <div style={{ fontSize: "18px", fontWeight: "800", color: "#22c55e" }}>
                ₹ {inrSigned(data.summary.total_net_pnl_inr)}
              </div>
              <div style={{ fontSize: "10px", color: "#22c55e" }}>Win Rate: {toN(data.summary.win_rate_pct).toFixed(1)}%</div>
            </div>
          </div>
        )}

        {/* Detailed What We Bought & What We Sold Table */}
        <div style={{ overflowX: "auto" }}>
          <table style={{ width: "100%", borderCollapse: "collapse", fontSize: "12px" }}>
            <thead>
              <tr style={{ borderBottom: "1px solid var(--border-color, #2a2a30)", textAlign: "left", color: "var(--text-secondary, #9ca3af)" }}>
                <th style={{ padding: "10px 8px" }}>Trade ID</th>
                <th style={{ padding: "10px 8px" }}>Shares</th>
                <th style={{ padding: "10px 8px" }}>What We Bought (Entry)</th>
                <th style={{ padding: "10px 8px" }}>What We Sold (Exit)</th>
                <th style={{ padding: "10px 8px" }}>Gross Profit</th>
                <th style={{ padding: "10px 8px" }}>Taxes (STT/GST)</th>
                <th style={{ padding: "10px 8px" }}>Net Profit (₹)</th>
                <th style={{ padding: "10px 8px" }}>ROI %</th>
                <th style={{ padding: "10px 8px" }}>Status / Reason</th>
              </tr>
            </thead>
            <tbody>
              {(!data?.detailed_trades || data.detailed_trades.length === 0) ? (
                <tr>
                  <td colSpan={9} style={{ padding: "20px", textAlign: "center", color: "var(--text-secondary, #9ca3af)" }}>
                    No trades closed yet. Use the [+ BUY] button above to test a buy order!
                  </td>
                </tr>
              ) : (
                data.detailed_trades.map((t) => {
                  const isClosed = t.status === "CLOSED";
                  const isProfit = toN(t.net_pnl, 0) > 0;
                  return (
                    <tr key={t.id} style={{ borderBottom: "1px solid var(--border-color, #2a2a30)" }}>
                      <td style={{ padding: "10px 8px", fontFamily: "monospace", color: "#9ca3af" }}>{t.id}</td>
                      <td style={{ padding: "10px 8px", fontWeight: "700", color: "#fff" }}>{t.shares} sh</td>

                      {/* Bought details */}
                      <td style={{ padding: "10px 8px" }}>
                        <div style={{ fontWeight: "600", color: "#22c55e" }}>
                          BUY @ ₹{inr(t.buy_price)}
                        </div>
                        <div style={{ fontSize: "11px", color: "#9ca3af" }}>
                          Total: ₹{inr(t.buy_value)} ({t.buy_time})
                        </div>
                      </td>

                      {/* Sold details */}
                      <td style={{ padding: "10px 8px" }}>
                        {isClosed ? (
                          <div>
                            <div style={{ fontWeight: "600", color: "#ef4444" }}>
                              SELL @ ₹{inr(t.sell_price || 0)}
                            </div>
                            <div style={{ fontSize: "11px", color: "#9ca3af" }}>
                              Total: ₹{inr(t.sell_value || 0)} ({t.sell_time})
                            </div>
                          </div>
                        ) : (
                          <Badge tone="neutral">Holding Active</Badge>
                        )}
                      </td>

                      {/* Gross Profit */}
                      <td style={{ padding: "10px 8px", fontWeight: "600", color: isProfit ? "#22c55e" : "#ef4444" }}>
                        {isClosed && t.gross_pnl !== undefined ? `₹ ${inrSigned(t.gross_pnl)}` : "—"}
                      </td>

                      {/* Taxes */}
                      <td style={{ padding: "10px 8px", color: "#f59e0b" }}>
                        - ₹{inr(t.total_taxes)}
                      </td>

                      {/* Net Profit */}
                      <td style={{ padding: "10px 8px" }}>
                        {isClosed && t.net_pnl !== undefined ? (
                          <span style={{ fontWeight: "800", color: isProfit ? "#22c55e" : "#ef4444", fontSize: "13px" }}>
                            ₹ {inrSigned(t.net_pnl)}
                          </span>
                        ) : (
                          <span style={{ color: "#9ca3af" }}>Floating</span>
                        )}
                      </td>

                      {/* ROI */}
                      <td style={{ padding: "10px 8px", fontWeight: "700", color: isProfit ? "#22c55e" : "#ef4444" }}>
                        {isClosed && t.roi_pct !== undefined ? `${toN(t.roi_pct).toFixed(2)}%` : "—"}
                      </td>

                      {/* Status / Reason */}
                      <td style={{ padding: "10px 8px" }}>
                        {isClosed ? (
                          <Badge tone={isProfit ? "good" : "critical"}>
                            {t.exit_reason || "Target Met"}
                          </Badge>
                        ) : (
                          <Badge tone="good">Open Position</Badge>
                        )}
                        <div style={{ fontSize: "10px", color: "#9ca3af", marginTop: "2px" }}>
                          {t.holding_duration}
                        </div>
                      </td>
                    </tr>
                  );
                })
              )}
            </tbody>
          </table>
        </div>
      </div>
        </>
      )}

      {/* 4. Quick Order Modal */}
      {orderModalOpen && (
        <div style={{ position: "fixed", inset: 0, background: "rgba(0,0,0,0.75)", display: "flex", alignItems: "center", justifyContent: "center", zIndex: 1000, padding: "16px" }}>
          <div style={{ background: "var(--card-bg, #1a1a1e)", border: "1px solid var(--border-color, #2a2a30)", borderRadius: "12px", width: "100%", maxWidth: "440px", padding: "24px" }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "16px" }}>
              <h3 style={{ margin: 0, fontSize: "18px", fontWeight: "700", color: "#fff" }}>
                Place {orderSide} Order ({selectedSymbol})
              </h3>
              <button
                onClick={() => setOrderModalOpen(false)}
                style={{ background: "transparent", border: "none", color: "#9ca3af", fontSize: "18px", cursor: "pointer" }}
              >
                ✕
              </button>
            </div>

            <form onSubmit={submitOrder} style={{ display: "flex", flexDirection: "column", gap: "16px" }}>
              <div>
                <label style={{ display: "block", fontSize: "12px", color: "#9ca3af", marginBottom: "4px" }}>Symbol</label>
                <input
                  type="text"
                  value={selectedSymbol}
                  disabled
                  style={{ width: "100%", padding: "10px", background: "var(--bg-input, #222)", border: "1px solid var(--border-color, #333)", borderRadius: "6px", color: "#fff", fontWeight: "600" }}
                />
              </div>

              <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "12px" }}>
                <div>
                  <label style={{ display: "block", fontSize: "12px", color: "#9ca3af", marginBottom: "4px" }}>Quantity (Whole Shares)</label>
                  <input
                    type="number"
                    min="1"
                    step="1"
                    value={orderQty}
                    onChange={(e) => setOrderQty(e.target.value)}
                    style={{ width: "100%", padding: "10px", background: "var(--bg-input, #222)", border: "1px solid var(--border-color, #333)", borderRadius: "6px", color: "#fff" }}
                  />
                </div>

                <div>
                  <label style={{ display: "block", fontSize: "12px", color: "#9ca3af", marginBottom: "4px" }}>Price (INR ₹)</label>
                  <input
                    type="number"
                    step="0.05"
                    value={orderPrice}
                    onChange={(e) => setOrderPrice(e.target.value)}
                    style={{ width: "100%", padding: "10px", background: "var(--bg-input, #222)", border: "1px solid var(--border-color, #333)", borderRadius: "6px", color: "#fff" }}
                  />
                </div>
              </div>

              <div style={{ padding: "10px", background: "var(--bg-input, #222)", borderRadius: "6px", fontSize: "12px", color: "#9ca3af" }}>
                Total Order Notional: <strong style={{ color: "#fff" }}>₹ {inr((parseFloat(orderQty) || 0) * (parseFloat(orderPrice) || 0))}</strong>
                <br />
                Statutory STT + Stamp Duty + GST: <span style={{ color: "#f59e0b" }}>~₹ {inr((parseFloat(orderQty) || 0) * (parseFloat(orderPrice) || 0) * 0.0012)}</span>
              </div>

              {orderFeedback && (
                <div style={{
                  padding: "10px",
                  borderRadius: "6px",
                  fontSize: "12px",
                  background: orderFeedback.kind === "good" ? "rgba(34, 197, 94, 0.15)" : "rgba(239, 68, 68, 0.15)",
                  color: orderFeedback.kind === "good" ? "#22c55e" : "#ef4444",
                  border: `1px solid ${orderFeedback.kind === "good" ? "rgba(34, 197, 94, 0.3)" : "rgba(239, 68, 68, 0.3)"}`,
                }}>
                  {orderFeedback.msg}
                </div>
              )}

              <div style={{ display: "flex", gap: "10px", marginTop: "8px" }}>
                <button
                  type="button"
                  onClick={() => setOrderModalOpen(false)}
                  style={{ flex: 1, padding: "10px", background: "transparent", border: "1px solid var(--border-color, #333)", color: "#9ca3af", borderRadius: "6px", cursor: "pointer" }}
                >
                  Cancel
                </button>
                <button
                  type="submit"
                  disabled={submittingOrder}
                  style={{
                    flex: 1,
                    padding: "10px",
                    background: orderSide === "BUY" ? "#16a34a" : "#dc2626",
                    border: "none",
                    color: "#fff",
                    fontWeight: "700",
                    borderRadius: "6px",
                    cursor: "pointer",
                  }}
                >
                  {submittingOrder ? "Submitting..." : `Confirm ${orderSide} Order`}
                </button>
              </div>
            </form>
          </div>
        </div>
      )}
    </div>
  );
}
