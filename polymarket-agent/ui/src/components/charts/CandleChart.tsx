import { useState, useMemo } from "react";
import { inr } from "../../lib/format";
import { postJson } from "../../api/client";
import { numOr } from "../../lib/decimal";

const toN = (v: unknown, fallback = 0): number => numOr(v, fallback);

export interface CandlePoint {
  time: string;
  open: number | string;
  high: number | string;
  low: number | string;
  close: number | string;
  volume: number;
}

export function CandleChart({
  candles,
  symbol = "RELIANCE",
  currentPrice,
  onOrderPlaced,
}: {
  candles: CandlePoint[];
  symbol?: string;
  currentPrice?: number | string;
  onOrderPlaced?: () => void;
}) {
  const [hoveredCandle, setHoveredCandle] = useState<CandlePoint | null>(null);
  const [modalOpen, setModalOpen] = useState(false);
  const [orderSide, setOrderSide] = useState<"BUY" | "SELL">("BUY");
  const [orderQty, setOrderQty] = useState("1");
  const [submitting, setSubmitting] = useState(false);
  const [feedback, setFeedback] = useState<string | null>(null);

  const activeCandle = hoveredCandle || candles[candles.length - 1];

  // SVG Dimensions
  const width = 800;
  const height = 280;
  const pad = { top: 20, right: 65, bottom: 30, left: 15 };
  const plotWidth = width - pad.left - pad.right;
  const plotHeight = height - pad.top - pad.bottom;

  const { candleElements, gridLines } = useMemo(() => {
    if (!candles || candles.length === 0) {
      return { candleElements: [], gridLines: [] };
    }

    const highs = candles.map((c) => toN(c.high));
    const lows = candles.map((c) => toN(c.low));
    const minVal = Math.min(...lows) * 0.998;
    const maxVal = Math.max(...highs) * 1.002;
    const range = maxVal - minVal || 1;

    const candleWidth = Math.max(3, Math.min(14, (plotWidth / candles.length) * 0.65));

    const elements = candles.map((c, i) => {
      const o = toN(c.open);
      const h = toN(c.high);
      const l = toN(c.low);
      const cl = toN(c.close);

      const isGreen = cl >= o;
      const color = isGreen ? "var(--status-good, #10b981)" : "var(--status-critical, #ef4444)";

      const cx = pad.left + (i / (candles.length - 1 || 1)) * plotWidth;
      const yHigh = pad.top + plotHeight - ((h - minVal) / range) * plotHeight;
      const yLow = pad.top + plotHeight - ((l - minVal) / range) * plotHeight;
      const yOpen = pad.top + plotHeight - ((o - minVal) / range) * plotHeight;
      const yClose = pad.top + plotHeight - ((cl - minVal) / range) * plotHeight;

      const bodyTop = Math.min(yOpen, yClose);
      const bodyHeight = Math.max(2, Math.abs(yClose - yOpen));

      return (
        <g
          key={c.time + i}
          onMouseEnter={() => setHoveredCandle(c)}
          onMouseLeave={() => setHoveredCandle(null)}
          style={{ cursor: "pointer" }}
        >
          {/* Wick */}
          <line
            x1={cx}
            y1={yHigh}
            x2={cx}
            y2={yLow}
            stroke={color}
            strokeWidth={1.5}
            opacity={0.85}
          />
          {/* Body */}
          <rect
            x={cx - candleWidth / 2}
            y={bodyTop}
            width={candleWidth}
            height={bodyHeight}
            fill={color}
            rx={1}
            opacity={0.95}
          />
        </g>
      );
    });

    // 4 Horizontal Grid Lines
    const grids = [0, 0.33, 0.66, 1].map((pct) => {
      const p = minVal + pct * range;
      const y = pad.top + plotHeight - pct * plotHeight;
      return { y, price: p };
    });

    return { candleElements: elements, gridLines: grids };
  }, [candles, plotWidth, plotHeight]);

  async function executeOrder() {
    setSubmitting(true);
    setFeedback(null);
    try {
      const res = await postJson("/api/india/sandbox/order", {
        symbol,
        side: orderSide,
        quantity: parseInt(orderQty, 10) || 1,
        order_type: "MARKET",
        price: null,
      });

      if (res.ok) {
        setFeedback(`Order Placed: ${orderSide} ${orderQty} ${symbol}!`);
        onOrderPlaced?.();
        setTimeout(() => {
          setModalOpen(false);
          setFeedback(null);
        }, 1500);
      } else {
        setFeedback("Failed: " + (res.error?.kind || "rejected"));
      }
    } catch {
      setFeedback("Network error");
    } finally {
      setSubmitting(false);
    }
  }

  const latestPrice = toN(currentPrice || activeCandle?.close || 1205.50);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "10px", width: "100%" }}>
      {/* Header bar: Symbol, Price, and Quick Buy/Sell Buttons */}
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", flexWrap: "wrap", gap: "8px" }}>
        <div style={{ display: "flex", alignItems: "baseline", gap: "10px" }}>
          <strong style={{ fontSize: "16px", color: "var(--text-primary)" }}>{symbol}</strong>
          <span style={{ fontSize: "20px", fontWeight: "700", color: "var(--status-good)", fontVariantNumeric: "tabular-nums" }}>
            {inr(latestPrice)}
          </span>
          {activeCandle && (
            <span style={{ fontSize: "12px", color: "var(--text-muted)", fontVariantNumeric: "tabular-nums" }}>
              O: {toN(activeCandle.open).toFixed(1)} · H: {toN(activeCandle.high).toFixed(1)} · L: {toN(activeCandle.low).toFixed(1)} · C: {toN(activeCandle.close).toFixed(1)}
            </span>
          )}
        </div>

        {/* Pro Buy / Sell Quick Buttons */}
        <div style={{ display: "flex", gap: "8px" }}>
          <button
            type="button"
            className="btn btn--sm"
            style={{ backgroundColor: "var(--status-good)", color: "#ffffff", borderColor: "var(--status-good)", fontWeight: "600" }}
            onClick={() => {
              setOrderSide("BUY");
              setModalOpen(true);
            }}
          >
            + Buy {symbol}
          </button>
          <button
            type="button"
            className="btn btn--sm btn--danger"
            style={{ fontWeight: "600" }}
            onClick={() => {
              setOrderSide("SELL");
              setModalOpen(true);
            }}
          >
            - Sell {symbol}
          </button>
        </div>
      </div>

      {/* SVG Candlestick Plot */}
      <div style={{ position: "relative", width: "100%", overflowX: "auto", background: "var(--surface-0)", borderRadius: "var(--radius-sm)", border: "1px solid var(--border)" }}>
        <svg
          viewBox={`0 0 ${width} ${height}`}
          style={{ display: "block", width: "100%", height: "auto", minWidth: "500px" }}
        >
          {/* Horizontal Grid lines & Price Labels */}
          {gridLines.map((g, idx) => (
            <g key={idx}>
              <line
                x1={pad.left}
                y1={g.y}
                x2={pad.left + plotWidth}
                y2={g.y}
                stroke="var(--grid)"
                strokeDasharray="3 3"
              />
              <text
                x={pad.left + plotWidth + 8}
                y={g.y + 4}
                fill="var(--text-muted)"
                fontSize="10"
                fontFamily="var(--mono)"
              >
                ₹{g.price.toFixed(1)}
              </text>
            </g>
          ))}

          {/* Render all candles */}
          {candleElements}
        </svg>
      </div>

      {/* Quick Trade Modal */}
      {modalOpen && (
        <div className="backdrop">
          <div className="dialog" style={{ width: "360px" }}>
            <h3 style={{ margin: "0 0 10px 0", color: orderSide === "BUY" ? "var(--status-good)" : "var(--status-critical)" }}>
              {orderSide} {symbol} · NSE Cash Sandbox
            </h3>
            <p style={{ fontSize: "12px", color: "var(--text-secondary)", marginBottom: "14px" }}>
              Simulated market order executing on Upstox NSE sandbox with automatic statutory cost calculation.
            </p>

            <label className="field" style={{ marginBottom: "12px", display: "block" }}>
              <span style={{ fontSize: "12px", color: "var(--text-secondary)" }}>Quantity (Shares)</span>
              <input
                type="number"
                min="1"
                max="50"
                className="input"
                style={{ width: "100%", marginTop: "4px" }}
                value={orderQty}
                onChange={(e) => setOrderQty(e.target.value)}
              />
            </label>

            <div style={{ fontSize: "12px", padding: "10px", background: "var(--surface-2)", borderRadius: "var(--radius-sm)", marginBottom: "14px" }}>
              <div style={{ display: "flex", justifyContent: "space-between", marginBottom: "4px" }}>
                <span>Estimated Value:</span>
                <strong>{inr(latestPrice * (parseInt(orderQty, 10) || 1))}</strong>
              </div>
              <div style={{ display: "flex", justifyContent: "space-between", color: "var(--text-muted)" }}>
                <span>Brokerage + STT (0.025%):</span>
                <span>₹{(latestPrice * (parseInt(orderQty, 10) || 1) * 0.0003).toFixed(2)}</span>
              </div>
            </div>

            {feedback && (
              <p style={{ margin: "0 0 10px 0", fontSize: "12px", color: "var(--status-good)", fontWeight: "600" }}>
                {feedback}
              </p>
            )}

            <div style={{ display: "flex", justifyContent: "flex-end", gap: "8px" }}>
              <button
                type="button"
                className="btn btn--quiet"
                disabled={submitting}
                onClick={() => setModalOpen(false)}
              >
                Cancel
              </button>
              <button
                type="button"
                disabled={submitting}
                className={`btn ${orderSide === "BUY" ? "btn--primary" : "btn--danger"}`}
                onClick={executeOrder}
              >
                {submitting ? "Placing…" : `Confirm ${orderSide}`}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
