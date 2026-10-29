import { useEffect, useRef, useState, useMemo } from "react";
import { postJson } from "../../api/client";

export interface CandlePoint {
  time: string;
  open: number | string;
  high: number | string;
  low: number | string;
  close: number | string;
  volume: number;
}

export function CanvasCandleChart({
  symbol = "RELIANCE",
  candles = [],
  currentPrice,
  market = "NSE",
  currency = "INR",
  onOrderPlaced,
}: {
  symbol?: string;
  candles: CandlePoint[];
  currentPrice?: number | string;
  market?: "NSE" | "US";
  currency?: "INR" | "USD";
  onOrderPlaced?: () => void;
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);

  const currSymbol = currency === "USD" ? "$" : "₹";

  const [chartStyle, setChartStyle] = useState<"candles" | "line" | "area">("candles");
  const [showEma20, setShowEma20] = useState(true);
  const [showEma50, setShowEma50] = useState(true);
  const [selectedInterval, setSelectedInterval] = useState("5m");

  // Quick ticket order state
  const [orderSide, setOrderSide] = useState<"BUY" | "SELL">("BUY");
  const [orderQty, setOrderQty] = useState("1");
  const [submitting, setSubmitting] = useState(false);
  const [orderFeedback, setOrderFeedback] = useState<{ kind: "good" | "bad"; msg: string } | null>(null);

  const candleWidth = 10;
  const candleSpacing = 4;

  const toN = (v: unknown, fallback = 0): number => {
    const n = typeof v === "number" ? v : parseFloat(String(v));
    return isNaN(n) ? fallback : n;
  };

  const latestPrice = toN(currentPrice || candles[candles.length - 1]?.close || 0);

  // EMA Calculation
  const { ema20, ema50 } = useMemo(() => {
    function calcEma(period: number) {
      const k = 2 / (period + 1);
      const res: (number | null)[] = new Array(candles.length).fill(null);
      if (candles.length < period) return res;

      let sum = 0;
      for (let i = 0; i < period; i++) sum += toN(candles[i].close);
      let prev = sum / period;
      res[period - 1] = prev;

      for (let i = period; i < candles.length; i++) {
        const val = toN(candles[i].close) * k + prev * (1 - k);
        res[i] = val;
        prev = val;
      }
      return res;
    }
    return { ema20: calcEma(20), ema50: calcEma(50) };
  }, [candles]);

  // Canvas 60fps render
  useEffect(() => {
    const canvas = canvasRef.current;
    const container = containerRef.current;
    if (!canvas || !container) return;

    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    let animId: number;

    function render() {
      if (!canvas || !container) return;
      const c = ctx as CanvasRenderingContext2D;
      const dpr = window.devicePixelRatio || 1;
      const rect = container.getBoundingClientRect();
      const width = rect.width;
      const height = rect.height || 300;

      if (canvas.width !== width * dpr || canvas.height !== height * dpr) {
        canvas.width = width * dpr;
        canvas.height = height * dpr;
      }

      c.resetTransform();
      c.scale(dpr, dpr);
      c.clearRect(0, 0, width, height);

      if (candles.length === 0) {
        c.fillStyle = "#7a869e";
        c.font = "14px Inter, sans-serif";
        c.textAlign = "center";
        c.fillText("⏳ Loading real market data...", width / 2, height / 2 - 12);
        c.font = "11px Inter, sans-serif";
        c.fillStyle = "#4a5568";
        c.fillText("Fetching live candles from API", width / 2, height / 2 + 12);
        return;
      }

      const priceScaleWidth = 75;
      const timeScaleHeight = 24;
      const volumeHeight = Math.min(height * 0.18, 55);

      const chartAreaWidth = width - priceScaleWidth;
      const chartAreaHeight = height - timeScaleHeight;
      const candleAreaHeight = chartAreaHeight - volumeHeight;

      const step = candleWidth + candleSpacing;
      const maxFit = Math.floor(chartAreaWidth / step);
      const startIndex = Math.max(0, candles.length - maxFit);
      const visible = candles.slice(startIndex);

      if (visible.length === 0) return;

      let minP = Infinity;
      let maxP = -Infinity;
      let maxVol = 0;

      for (let i = 0; i < visible.length; i++) {
        const pt = visible[i];
        const low = toN(pt.low);
        const high = toN(pt.high);
        const vol = toN(pt.volume);
        if (low < minP) minP = low;
        if (high > maxP) maxP = high;
        if (vol > maxVol) maxVol = vol;
      }

      const pad = (maxP - minP) * 0.08 || 1;
      maxP += pad;
      minP -= pad;
      const priceRange = maxP - minP;

      const getY = (val: number) => candleAreaHeight - ((val - minP) / priceRange) * candleAreaHeight;
      const getVolY = (vol: number) => chartAreaHeight - (vol / (maxVol || 1)) * (volumeHeight - 8);
      const getX = (idx: number) => chartAreaWidth - ((visible.length - 1 - idx) * step) - candleWidth;

      // 1. Grid Lines & Axis
      c.strokeStyle = "rgba(31, 39, 55, 0.45)";
      c.lineWidth = 1;
      c.setLineDash([4, 4]);

      const gridSteps = 5;
      for (let i = 0; i <= gridSteps; i++) {
        const y = (candleAreaHeight / gridSteps) * i;
        c.beginPath();
        c.moveTo(0, y);
        c.lineTo(chartAreaWidth, y);
        c.stroke();

        const pVal = maxP - (i / gridSteps) * priceRange;
        c.setLineDash([]);
        c.fillStyle = "#7a869e";
        c.font = "10px JetBrains Mono, monospace";
        c.textAlign = "left";
        c.fillText(`${currSymbol}${pVal.toFixed(2)}`, chartAreaWidth + 8, y + 3.5);
        c.setLineDash([4, 4]);
      }

      // 2. EMA Indicators
      function drawEma(data: (number | null)[], color: string) {
        c.beginPath();
        c.strokeStyle = color;
        c.lineWidth = 1.6;
        c.setLineDash([]);
        let first = true;
        for (let i = 0; i < visible.length; i++) {
          const gIdx = startIndex + i;
          const val = data[gIdx];
          if (val !== null && val !== undefined) {
            const x = getX(i) + candleWidth / 2;
            const y = getY(val);
            if (first) {
              c.moveTo(x, y);
              first = false;
            } else {
              c.lineTo(x, y);
            }
          }
        }
        c.stroke();
      }

      if (showEma20) drawEma(ema20, "#f0b90b");
      if (showEma50) drawEma(ema50, "#3861fb");

      // 3. Volume Histogram
      c.setLineDash([]);
      for (let i = 0; i < visible.length; i++) {
        const pt = visible[i];
        const x = getX(i);
        const yTop = getVolY(toN(pt.volume));
        const isUp = toN(pt.close) >= toN(pt.open);
        c.fillStyle = isUp ? "rgba(14, 203, 129, 0.35)" : "rgba(246, 70, 93, 0.35)";
        c.fillRect(x, yTop, candleWidth, chartAreaHeight - yTop);
      }

      // 4. Candlesticks / Line / Area
      if (chartStyle === "candles") {
        for (let i = 0; i < visible.length; i++) {
          const pt = visible[i];
          const x = getX(i);
          const o = toN(pt.open);
          const cl = toN(pt.close);
          const h = toN(pt.high);
          const l = toN(pt.low);
          const isUp = cl >= o;

          const openY = getY(o);
          const closeY = getY(cl);
          const highY = getY(h);
          const lowY = getY(l);

          const bodyTop = Math.min(openY, closeY);
          const bodyHeight = Math.max(Math.abs(closeY - openY), 1.2);
          const color = isUp ? "#0ecb81" : "#f6465d";

          c.strokeStyle = color;
          c.lineWidth = 1.3;
          c.beginPath();
          c.moveTo(x + candleWidth / 2, highY);
          c.lineTo(x + candleWidth / 2, lowY);
          c.stroke();

          c.fillStyle = color;
          c.fillRect(x, bodyTop, candleWidth, bodyHeight);
        }
      } else {
        c.beginPath();
        for (let i = 0; i < visible.length; i++) {
          const x = getX(i) + candleWidth / 2;
          const y = getY(toN(visible[i].close));
          if (i === 0) c.moveTo(x, y);
          else c.lineTo(x, y);
        }

        if (chartStyle === "area") {
          const grad = c.createLinearGradient(0, 0, 0, candleAreaHeight);
          grad.addColorStop(0, "rgba(56, 97, 251, 0.35)");
          grad.addColorStop(1, "rgba(56, 97, 251, 0.0)");
          c.lineTo(getX(visible.length - 1) + candleWidth / 2, candleAreaHeight);
          c.lineTo(getX(0) + candleWidth / 2, candleAreaHeight);
          c.closePath();
          c.fillStyle = grad;
          c.fill();

          c.beginPath();
          for (let i = 0; i < visible.length; i++) {
            const x = getX(i) + candleWidth / 2;
            const y = getY(toN(visible[i].close));
            if (i === 0) c.moveTo(x, y);
            else c.lineTo(x, y);
          }
        }
        c.strokeStyle = "#3861fb";
        c.lineWidth = 2;
        c.stroke();
      }

      // 5. Current Price Pulse Line
      const latest = candles[candles.length - 1];
      const curY = getY(toN(latest.close));
      const isUp = toN(latest.close) >= toN(latest.open);

      c.setLineDash([3, 3]);
      c.strokeStyle = isUp ? "#0ecb81" : "#f6465d";
      c.beginPath();
      c.moveTo(0, curY);
      c.lineTo(chartAreaWidth, curY);
      c.stroke();
      c.setLineDash([]);

      c.fillStyle = isUp ? "#0ecb81" : "#f6465d";
      c.fillRect(chartAreaWidth, curY - 9, priceScaleWidth, 18);
      c.fillStyle = "#ffffff";
      c.font = "bold 10px JetBrains Mono, monospace";
      c.fillText(`${currSymbol}${toN(latest.close).toFixed(2)}`, chartAreaWidth + 6, curY + 3.5);

      animId = requestAnimationFrame(render);
    }

    render();
    return () => cancelAnimationFrame(animId);
  }, [candles, chartStyle, showEma20, showEma50, ema20, ema50]);

  // Execute Order handler
  async function placeOrder() {
    setSubmitting(true);
    setOrderFeedback(null);
    try {
      const endpoint = market === "US" ? "/api/us/paper/order" : "/api/india/sandbox/order";
      const res = await postJson(endpoint, {
        symbol,
        side: orderSide,
        quantity: parseInt(orderQty, 10) || 1,
        order_type: "MARKET",
        price: null,
      });

      if (res.ok) {
        setOrderFeedback({
          kind: "good",
          msg: `Executed ${orderSide} ${orderQty} ${symbol} at ${currSymbol}${latestPrice.toFixed(2)} on ${market === "US" ? "Alpaca Paper" : "NSE Sandbox"}!`,
        });
        onOrderPlaced?.();
      } else {
        const errorMsg =
          "message" in res.error && res.error.message
            ? res.error.message
            : res.error.kind || "Execution rejected";
        setOrderFeedback({ kind: "bad", msg: `Failed: ${errorMsg}` });
      }
    } catch {
      setOrderFeedback({ kind: "bad", msg: "Network error executing order" });
    } finally {
      setSubmitting(false);
      setTimeout(() => setOrderFeedback(null), 4000);
    }
  }

  const active = candles[candles.length - 1];

  return (
    <div
      style={{
        display: "grid",
        gridTemplateColumns: "1fr 280px",
        gap: "1px",
        background: "var(--border)",
        border: "1px solid var(--border)",
        borderRadius: "var(--radius)",
        overflow: "hidden",
      }}
    >
      {/* LEFT: 60FPS CANVAS CHART ENGINE */}
      <div style={{ display: "flex", flexDirection: "column", background: "var(--surface-0)", minWidth: 0 }}>
        {/* Chart Header Bar */}
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            padding: "8px 12px",
            background: "var(--surface-1)",
            borderBottom: "1px solid var(--border)",
            flexWrap: "wrap",
            gap: "8px",
          }}
        >
          <div style={{ display: "flex", alignItems: "center", gap: "10px" }}>
            <span style={{ fontSize: "14px", fontWeight: "700", color: "var(--text-primary)" }}>{symbol}</span>
            <span style={{ fontSize: "16px", fontWeight: "700", color: "var(--status-good)", fontVariantNumeric: "tabular-nums" }}>
              {currSymbol}{latestPrice.toFixed(2)}
            </span>
            <div style={{ display: "flex", gap: "2px" }}>
              {["1m", "5m", "15m", "1H", "1D"].map((tf) => (
                <button
                  key={tf}
                  type="button"
                  style={{
                    padding: "2px 8px",
                    fontSize: "11px",
                    fontWeight: 500,
                    borderRadius: "3px",
                    border: "none",
                    background: selectedInterval === tf ? "var(--surface-2)" : "transparent",
                    color: selectedInterval === tf ? "var(--text-primary)" : "var(--text-muted)",
                    cursor: "pointer",
                  }}
                  onClick={() => setSelectedInterval(tf)}
                >
                  {tf}
                </button>
              ))}
            </div>
          </div>

          {/* Chart Controls */}
          <div style={{ display: "flex", alignItems: "center", gap: "6px" }}>
            <div style={{ display: "flex", background: "var(--surface-2)", padding: "2px", borderRadius: "4px" }}>
              {(["candles", "line", "area"] as const).map((style) => (
                <button
                  key={style}
                  type="button"
                  style={{
                    padding: "2px 8px",
                    fontSize: "11px",
                    fontWeight: 600,
                    borderRadius: "3px",
                    border: "none",
                    background: chartStyle === style ? "var(--surface-1)" : "transparent",
                    color: chartStyle === style ? "var(--text-primary)" : "var(--text-muted)",
                    cursor: "pointer",
                    textTransform: "capitalize",
                  }}
                  onClick={() => setChartStyle(style)}
                >
                  {style}
                </button>
              ))}
            </div>

            <button
              type="button"
              style={{
                padding: "2px 6px",
                fontSize: "11px",
                borderRadius: "3px",
                border: "1px solid rgba(240, 185, 11, 0.4)",
                background: showEma20 ? "rgba(240, 185, 11, 0.12)" : "transparent",
                color: "#f0b90b",
                cursor: "pointer",
              }}
              onClick={() => setShowEma20(!showEma20)}
            >
              EMA 20
            </button>
            <button
              type="button"
              style={{
                padding: "2px 6px",
                fontSize: "11px",
                borderRadius: "3px",
                border: "1px solid rgba(56, 97, 251, 0.4)",
                background: showEma50 ? "rgba(56, 97, 251, 0.12)" : "transparent",
                color: "#3861fb",
                cursor: "pointer",
              }}
              onClick={() => setShowEma50(!showEma50)}
            >
              EMA 50
            </button>
          </div>
        </div>

        {/* OHLCV Dynamic Status Bar */}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: "12px",
            padding: "4px 12px",
            fontSize: "11px",
            fontFamily: "var(--mono)",
            color: "var(--text-muted)",
            background: "var(--surface-0)",
            borderBottom: "1px solid var(--border)",
          }}
        >
          <span>O: <strong style={{ color: "var(--text-primary)" }}>{toN(active?.open).toFixed(2)}</strong></span>
          <span>H: <strong style={{ color: "var(--text-primary)" }}>{toN(active?.high).toFixed(2)}</strong></span>
          <span>L: <strong style={{ color: "var(--text-primary)" }}>{toN(active?.low).toFixed(2)}</strong></span>
          <span>C: <strong style={{ color: "var(--status-good)" }}>{toN(active?.close).toFixed(2)}</strong></span>
          <span>Vol: <strong style={{ color: "var(--text-primary)" }}>{toN(active?.volume).toFixed(0)}</strong></span>
        </div>

        {/* Canvas Plot */}
        <div ref={containerRef} style={{ position: "relative", width: "100%", height: "280px" }}>
          <canvas ref={canvasRef} style={{ width: "100%", height: "100%", display: "block" }} />
        </div>
      </div>

      {/* RIGHT: INSTITUTIONAL QUICK TRADE TICKET & DEPTH */}
      <div style={{ display: "flex", flexDirection: "column", background: "var(--surface-1)", padding: "12px", minWidth: 0 }}>
        <div style={{ fontSize: "12px", fontWeight: "700", color: "var(--text-primary)", marginBottom: "8px" }}>
          Institutional Trade Ticket
        </div>

        {/* BUY / SELL Switch Tabs */}
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "1fr 1fr",
            gap: "4px",
            padding: "2px",
            background: "var(--surface-2)",
            borderRadius: "var(--radius-sm)",
            marginBottom: "10px",
          }}
        >
          <button
            type="button"
            style={{
              padding: "6px",
              fontSize: "12px",
              fontWeight: "700",
              border: "none",
              borderRadius: "3px",
              background: orderSide === "BUY" ? "var(--status-good)" : "transparent",
              color: orderSide === "BUY" ? "#ffffff" : "var(--text-muted)",
              cursor: "pointer",
            }}
            onClick={() => setOrderSide("BUY")}
          >
            BUY / LONG
          </button>
          <button
            type="button"
            style={{
              padding: "6px",
              fontSize: "12px",
              fontWeight: "700",
              border: "none",
              borderRadius: "3px",
              background: orderSide === "SELL" ? "var(--status-critical)" : "transparent",
              color: orderSide === "SELL" ? "#ffffff" : "var(--text-muted)",
              cursor: "pointer",
            }}
            onClick={() => setOrderSide("SELL")}
          >
            SELL / SHORT
          </button>
        </div>

        {/* Quantity Input */}
        <div style={{ marginBottom: "8px" }}>
          <div style={{ display: "flex", justifyContent: "space-between", fontSize: "11px", color: "var(--text-muted)", marginBottom: "2px" }}>
            <span>Order Quantity</span>
            <span>Shares</span>
          </div>
          <input
            type="number"
            min="1"
            max="100"
            className="input"
            style={{ width: "100%", padding: "5px 8px", fontSize: "12px", fontVariantNumeric: "tabular-nums" }}
            value={orderQty}
            onChange={(e) => setOrderQty(e.target.value)}
          />
        </div>

        {/* Quick Quantity Chips */}
        <div style={{ display: "grid", gridTemplateColumns: "repeat(4, 1fr)", gap: "4px", marginBottom: "10px" }}>
          {["1", "5", "10", "25"].map((q) => (
            <button
              key={q}
              type="button"
              className="btn btn--sm"
              style={{ padding: "3px 0", fontSize: "11px", textAlign: "center" }}
              onClick={() => setOrderQty(q)}
            >
              {q} Qty
            </button>
          ))}
        </div>

        {/* Cost & Tax Summary */}
        <div
          style={{
            padding: "8px",
            background: "var(--surface-2)",
            borderRadius: "var(--radius-sm)",
            fontSize: "11px",
            fontFamily: "var(--mono)",
            marginBottom: "12px",
            display: "flex",
            flexDirection: "column",
            gap: "4px",
          }}
        >
          <div style={{ display: "flex", justifyContent: "space-between" }}>
            <span style={{ color: "var(--text-muted)" }}>Est. Value:</span>
            <strong style={{ color: "var(--text-primary)" }}>
              {currSymbol}{(latestPrice * (parseInt(orderQty, 10) || 1)).toFixed(2)}
            </strong>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between" }}>
            <span style={{ color: "var(--text-muted)" }}>
              {market === "US" ? "SEC + FINRA Fees:" : "STT + GST (0.025%):"}
            </span>
            <span style={{ color: "var(--text-secondary)" }}>
              {market === "US"
                ? "$0.01"
                : `₹${(latestPrice * (parseInt(orderQty, 10) || 1) * 0.0003).toFixed(2)}`}
            </span>
          </div>
        </div>

        {orderFeedback && (
          <div
            style={{
              padding: "6px 8px",
              borderRadius: "4px",
              fontSize: "11px",
              marginBottom: "8px",
              background: orderFeedback.kind === "good" ? "var(--status-good-bg)" : "var(--status-critical-bg)",
              color: orderFeedback.kind === "good" ? "var(--status-good)" : "var(--status-critical)",
              fontWeight: 600,
            }}
          >
            {orderFeedback.msg}
          </div>
        )}

        {/* Submit Execution Button */}
        <button
          type="button"
          disabled={submitting}
          style={{
            width: "100%",
            padding: "8px",
            fontSize: "12px",
            fontWeight: "700",
            border: "none",
            borderRadius: "var(--radius-sm)",
            background: orderSide === "BUY" ? "var(--status-good)" : "var(--status-critical)",
            color: "#ffffff",
            cursor: submitting ? "not-allowed" : "pointer",
            boxShadow: "0 2px 8px rgba(0,0,0,0.2)",
          }}
          onClick={placeOrder}
        >
          {submitting ? "Placing…" : `${orderSide} ${orderQty} ${symbol}`}
        </button>
      </div>
    </div>
  );
}
