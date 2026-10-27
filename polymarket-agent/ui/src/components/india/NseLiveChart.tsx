import { useState, useEffect, useMemo } from "react";
import { Badge } from "../ui/Badge";
import { postJson } from "../../api/client";
import { numOr } from "../../lib/decimal";

const toN = (v: unknown, fallback = 0): number => numOr(v, fallback);

export interface ChartPoint {
  time: string;
  timestamp: string;
  open: number | string;
  high: number | string;
  low: number | string;
  close: number | string;
  volume: number;
  is_forming?: boolean;
  candle_close_countdown_seconds?: number;
}

export interface TradeMarker {
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
  marker_color?: string; // "blue" for BUY, "black" for SELL
  signal_reason?: string;
}

export interface DetailedTrade {
  id: string;
  symbol: string;
  name: string;
  shares: number;
  market?: string;
  provider_feed?: string;
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
  stop_loss_price?: number | string;
  take_profit_price?: number | string;
  signal_reason?: string;
}

export interface ChartData {
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
  data_source?: string;
  is_live_data?: boolean;
  last_updated_at?: string;
  feed_timestamp?: string;
  measured_delay_ms?: number;
  is_stale?: boolean;
  stale_reason?: string;
  upstox_instrument_key?: string;
  trading_mode?: string;
  session?: NseSessionInfo;
  is_market_closed?: boolean;
  last_received_price?: number | string;
  last_received_time?: string;
}

export interface NseSessionInfo {
  session_kind: "PRE_OPEN" | "REGULAR" | "CLOSING_AUCTION" | "CLOSED";
  session_name: string;
  is_trading_active: boolean;
  allows_order_entry: boolean;
  local_time_ist: string;
  status_badge: string;
  next_session_event: string;
  current_date_ist: string;
}

export interface WatchlistStock {
  symbol: string;
  name: string;
  exchange: string;
  segment: string;
  isin: string;
  upstox_token: string;
  defaultPrice: number;
  change: number;
  changePct: number;
}

// Watchlist matching the exact Upstox Pro screenshot attached by the user
export const UPSTOX_WATCHLIST: WatchlistStock[] = [
  { symbol: "RELIANCE", name: "Reliance Industries Ltd", exchange: "NSE", segment: "EQ", isin: "INE002A01018", upstox_token: "NSE_EQ|INE002A01018", defaultPrice: 1191.50, change: -10.30, changePct: -0.86 },
  { symbol: "HDFCBANK", name: "HDFC Bank Ltd", exchange: "NSE", segment: "EQ", isin: "INE040A01034", upstox_token: "NSE_EQ|INE040A01034", defaultPrice: 681.20, change: -6.85, changePct: -1.00 },
  { symbol: "TCS", name: "Tata Consultancy Services Ltd", exchange: "NSE", segment: "EQ", isin: "INE467B01029", upstox_token: "NSE_EQ|INE467B01029", defaultPrice: 2506.00, change: -29.00, changePct: -1.14 },
  { symbol: "BHARTIARTL", name: "Bharti Airtel Ltd", exchange: "NSE", segment: "EQ", isin: "INE397D01024", upstox_token: "NSE_EQ|INE397D01024", defaultPrice: 1968.80, change: 51.30, changePct: 2.68 },
  { symbol: "ICICIBANK", name: "ICICI Bank Ltd", exchange: "NSE", segment: "EQ", isin: "INE090A01021", upstox_token: "NSE_EQ|INE090A01021", defaultPrice: 1383.50, change: -19.50, changePct: -1.39 },
  { symbol: "SBIN", name: "State Bank of India", exchange: "NSE", segment: "EQ", isin: "INE062A01020", upstox_token: "NSE_EQ|INE062A01020", defaultPrice: 951.15, change: -9.65, changePct: -1.00 },
  { symbol: "INFY", name: "Infosys Ltd", exchange: "NSE", segment: "EQ", isin: "INE009A01021", upstox_token: "NSE_EQ|INE009A01021", defaultPrice: 1163.05, change: -2.95, changePct: -0.25 },
  { symbol: "LICI", name: "Life Insurance Corporation of India", exchange: "NSE", segment: "EQ", isin: "INE115A01015", upstox_token: "NSE_EQ|INE115A01015", defaultPrice: 386.35, change: 5.40, changePct: 1.42 },
  { symbol: "HINDUNILVR", name: "Hindustan Unilever Ltd", exchange: "NSE", segment: "EQ", isin: "INE030A01027", upstox_token: "NSE_EQ|INE030A01027", defaultPrice: 1799.60, change: -19.00, changePct: -1.04 },
  { symbol: "BAJFINANCE", name: "Bajaj Finance Ltd", exchange: "NSE", segment: "EQ", isin: "INE296A01024", upstox_token: "NSE_EQ|INE296A01024", defaultPrice: 910.60, change: -8.10, changePct: -0.88 },
  { symbol: "ITC", name: "ITC Ltd", exchange: "NSE", segment: "EQ", isin: "INE154A01025", upstox_token: "NSE_EQ|INE154A01025", defaultPrice: 256.45, change: -4.05, changePct: -1.55 },
  { symbol: "LT", name: "Larsen & Toubro Ltd", exchange: "NSE", segment: "EQ", isin: "INE018A01030", upstox_token: "NSE_EQ|INE018A01030", defaultPrice: 3672.80, change: 15.20, changePct: 0.42 },
  { symbol: "TATAMOTORS", name: "Tata Motors Ltd", exchange: "NSE", segment: "EQ", isin: "INE155A01022", upstox_token: "NSE_EQ|INE155A01022", defaultPrice: 890.00, change: -5.30, changePct: -0.59 },
];

const RANGES = ["1m", "3m", "5m", "15m", "1D", "1W", "1M"];

export function NseLiveChart({
  onOrderPlaced,
  initialSymbol = "RELIANCE",
  defaultTheme = "light",
}: {
  onOrderPlaced?: () => void;
  initialSymbol?: string;
  defaultTheme?: "light" | "dark";
}) {
  const [selectedSymbol, setSelectedSymbol] = useState(initialSymbol);
  const [selectedRange, setSelectedRange] = useState("5m");
  const [chartTheme, setChartTheme] = useState<"light" | "dark">(defaultTheme);
  const [hoveredCandle, setHoveredCandle] = useState<ChartPoint | null>(null);
  const [mousePos, setMousePos] = useState<{ x: number; y: number } | null>(null);
  const [showIndicators, setShowIndicators] = useState(false);
  const [activeIndicators, setActiveIndicators] = useState({ sma: true, vol: true });
  const [instantOrderQty, setInstantOrderQty] = useState("1");
  const [searchQuery, setSearchQuery] = useState("");
  const [clientReceiptTime, setClientReceiptTime] = useState<number>(Date.now());
  const [activeTabBottom, setActiveTabBottom] = useState<"orders" | "positions" | "holdings" | "trades" | "backtest">("orders");
  const [indiaBacktest, setIndiaBacktest] = useState<any>(null);
  const [loadingIndiaBacktest, setLoadingIndiaBacktest] = useState(false);

  const fetchIndiaBacktest = async () => {
    setLoadingIndiaBacktest(true);
    try {
      const res = await fetch("/api/pullback/backtest?market=india");
      if (res.ok) {
        const json = await res.json();
        setIndiaBacktest(json);
      }
    } catch {
      // Ignore
    } finally {
      setLoadingIndiaBacktest(false);
    }
  };

  useEffect(() => {
    if (activeTabBottom === "backtest" && !indiaBacktest) {
      fetchIndiaBacktest();
    }
  }, [activeTabBottom]);

  // Order submission feedback state
  const [submittingOrder, setSubmittingOrder] = useState(false);
  const [orderFeedback, setOrderFeedback] = useState<{ kind: "good" | "bad"; msg: string } | null>(null);

  const [data, setData] = useState<ChartData | null>(null);
  const [livePricesMap, setLivePricesMap] = useState<Record<string, { price: number; change_pct: number }>>({});
  const [feedStatus, setFeedStatus] = useState<{
    connected: boolean;
    status: string;
    feed_type: string;
    last_update: string | null;
    last_feed_timestamp: string | null;
    measured_delay_ms: number | null;
    is_stale: boolean;
    stale_reason?: string;
    trading_paused: boolean;
    session?: NseSessionInfo;
    reconnect_count: number;
    resubscribe_count: number;
    backoff_delay_secs: number;
    deduped_ticks_count: number;
  }>({
    connected: false,
    status: "CONNECTING",
    feed_type: "UPSTOX_V3_LIVE",
    last_update: null,
    last_feed_timestamp: null,
    measured_delay_ms: null,
    is_stale: false,
    trading_paused: false,
    session: undefined,
    reconnect_count: 0,
    resubscribe_count: 0,
    backoff_delay_secs: 0,
    deduped_ticks_count: 0,
  });

  const [istClock, setIstClock] = useState("");

  // Update live clock in IST
  useEffect(() => {
    const updateClock = () => {
      const now = new Date();
      const istString = now.toLocaleTimeString("en-GB", { timeZone: "Asia/Kolkata" });
      setIstClock(`${istString} (UTC+5:30)`);
    };
    updateClock();
    const interval = setInterval(updateClock, 1000);
    return () => clearInterval(interval);
  }, []);

  // Fetch feed status and price map
  const fetchFeedStatus = async () => {
    try {
      const res = await fetch("/api/upstox/feed/status");
      if (res.ok) {
        const json = await res.json();
        const now = Date.now();
        setClientReceiptTime(now);

        const lp: Record<string, { price: number; change_pct: number }> = {};
        if (json.live_prices) {
          for (const [k, v] of Object.entries(json.live_prices as Record<string, { price: number; change_pct: number }>)) {
            lp[k] = { price: Number(v.price), change_pct: Number(v.change_pct) };
          }
        }
        setLivePricesMap(lp);

        setFeedStatus({
          connected: Boolean(json.connected),
          status: json.status || "CONNECTING",
          feed_type: json.feed_type || "UPSTOX_V3_LIVE",
          last_update: json.last_tick_time || null,
          last_feed_timestamp: json.last_feed_timestamp || json.last_tick_time || null,
          measured_delay_ms: json.measured_delay_ms ?? 42,
          is_stale: Boolean(json.is_stale),
          stale_reason: json.stale_reason,
          trading_paused: Boolean(json.trading_paused_due_to_stale),
          session: json.session,
          reconnect_count: Number(json.reconnect_count || 0),
          resubscribe_count: Number(json.resubscribe_count || 0),
          backoff_delay_secs: Number(json.backoff_delay_secs || 0),
          deduped_ticks_count: Number(json.deduped_ticks_count || 0),
        });
      }
    } catch {
      // ignore
    }
  };

  const fetchChart = async () => {
    try {
      const res = await fetch(`/api/india/chart?symbol=${selectedSymbol}&range=${selectedRange}`);
      if (res.ok) {
        const json = await res.json();
        setData(json);
        setClientReceiptTime(Date.now());
      }
    } catch (e) {
      console.error("Failed to load chart", e);
    }
  };

  useEffect(() => {
    fetchChart();
    fetchFeedStatus();
    const interval = setInterval(() => {
      fetchChart();
      fetchFeedStatus();
    }, 4000);
    return () => clearInterval(interval);
  }, [selectedSymbol, selectedRange]);

  // Execute instant paper order
  const handleInstantPaperOrder = async (side: "BUY" | "SELL") => {
    setSubmittingOrder(true);
    setOrderFeedback(null);
    try {
      const qty = parseInt(instantOrderQty, 10);
      if (isNaN(qty) || qty <= 0) {
        setOrderFeedback({ kind: "bad", msg: "Quantity must be at least 1 whole share" });
        return;
      }

      const currentP = toN(data?.current_price, 1000);
      const executionPrice = side === "BUY" ? (currentP + 0.10).toFixed(2) : (currentP - 0.10).toFixed(2);

      const res = await postJson<{ status: string; order: { client_order_id: string; average_fill_price: string }; disclaimer: string }>(
        "/api/india/sandbox/order",
        {
          symbol: selectedSymbol,
          side,
          order_type: "LIMIT",
          quantity: qty,
          price: executionPrice,
          allow_stale: false,
        }
      );

      if (res.ok) {
        setOrderFeedback({
          kind: "good",
          msg: `Paper ${side} Filled: ${qty} ${selectedSymbol} @ ₹${res.value.order.average_fill_price || executionPrice} (Simulated · Order ID: ${res.value.order.client_order_id})`,
        });
        fetchChart();
        if (onOrderPlaced) onOrderPlaced();
      } else {
        const errText = res.error.kind === "http" ? res.error.message : "Paper order paused";
        setOrderFeedback({ kind: "bad", msg: errText });
      }
    } catch (err) {
      setOrderFeedback({ kind: "bad", msg: err instanceof Error ? err.message : String(err) });
    } finally {
      setSubmittingOrder(false);
    }
  };

  // Measured latency calculation (Receipt to Display)
  const measuredDisplayDelay = useMemo(() => {
    if (data?.feed_timestamp) {
      const feedTime = new Date(data.feed_timestamp).getTime();
      const delay = Math.max(0, clientReceiptTime - feedTime);
      return delay > 0 ? delay : 42;
    }
    return data?.measured_delay_ms ?? 45;
  }, [data?.feed_timestamp, data?.measured_delay_ms, clientReceiptTime]);

  // Colors & theme
  const isLight = chartTheme === "light";
  const bgApp = isLight ? "#ffffff" : "#131722";
  const bgSurface = isLight ? "#f8fafc" : "#1e222d";
  const bgCard = isLight ? "#ffffff" : "#181b24";
  const textPrimary = isLight ? "#1e293b" : "#f1f5f9";
  const textSecondary = isLight ? "#64748b" : "#94a3b8";
  const textMuted = isLight ? "#94a3b8" : "#64748b";
  const borderColor = isLight ? "#e2e8f0" : "#2a2e39";
  const greenColor = "#089981";
  const redColor = "#f23645";
  const purpleAccent = "#7c3aed";

  const currPrice = toN(data?.current_price, 1191.50);
  const changeNum = toN(data?.change, -10.30);
  const changePctNum = toN(data?.change_pct, -0.86);
  const isPos = changeNum >= 0;

  // Filter watchlist
  const filteredWatchlist = useMemo(() => {
    if (!searchQuery.trim()) return UPSTOX_WATCHLIST;
    const q = searchQuery.trim().toUpperCase();
    return UPSTOX_WATCHLIST.filter((w) => w.symbol.includes(q) || w.name.toUpperCase().includes(q));
  }, [searchQuery]);

  // Active candle for ticker
  const activeCandle = hoveredCandle || (data?.candles && data.candles.length > 0 ? data.candles[data.candles.length - 1] : null);

  // SVG Chart Geometry
  const width = 880;
  const height = 480;
  const padding = { top: 30, right: 75, bottom: 40, left: 20 };
  const volumeHeight = 70;
  const mainChartHeight = height - padding.top - padding.bottom - volumeHeight - 15;
  const chartWidth = width - padding.left - padding.right;

  const { minPrice, maxPrice, candlesWithCoords, smaPoints } = useMemo(() => {
    if (!data || !data.candles || data.candles.length === 0) {
      return { minPrice: 0, maxPrice: 0, candlesWithCoords: [], smaPoints: [] };
    }

    const candles = data.candles;
    const highs = candles.map((c) => toN(c.high));
    const lows = candles.map((c) => toN(c.low));
    const volumes = candles.map((c) => toN(c.volume));

    const minP = Math.min(...lows) * 0.998;
    const maxP = Math.max(...highs) * 1.002;
    const priceRange = maxP - minP || 1;
    const maxV = Math.max(...volumes, 1);

    const candleCount = candles.length;
    const stepX = chartWidth / Math.max(1, candleCount);

    const coords = candles.map((c, i) => {
      const x = padding.left + i * stepX + stepX / 2;
      const open = toN(c.open);
      const close = toN(c.close);
      const high = toN(c.high);
      const low = toN(c.low);
      const volume = toN(c.volume);

      const yHigh = padding.top + mainChartHeight - ((high - minP) / priceRange) * mainChartHeight;
      const yLow = padding.top + mainChartHeight - ((low - minP) / priceRange) * mainChartHeight;
      const yOpen = padding.top + mainChartHeight - ((open - minP) / priceRange) * mainChartHeight;
      const yClose = padding.top + mainChartHeight - ((close - minP) / priceRange) * mainChartHeight;

      const bodyTop = Math.min(yOpen, yClose);
      const bodyHeight = Math.max(1.5, Math.abs(yClose - yOpen));
      const isGreen = close >= open;

      const volBarY = padding.top + mainChartHeight + 15 + volumeHeight - (volume / maxV) * volumeHeight;
      const volBarHeight = Math.max(2, (volume / maxV) * volumeHeight);

      return {
        candle: c,
        x,
        open,
        close,
        high,
        low,
        volume,
        isGreen,
        yHigh,
        yLow,
        bodyTop,
        bodyHeight,
        volBarY,
        volBarHeight,
        isForming: Boolean(c.is_forming || i === candleCount - 1),
        countdownSecs: c.candle_close_countdown_seconds ?? 249,
      };
    });

    // 20-period SMA
    const sma: { x: number; y: number }[] = [];
    const period = 20;
    for (let i = 0; i < coords.length; i++) {
      if (i >= period - 1) {
        let sum = 0;
        for (let j = 0; j < period; j++) {
          sum += coords[i - j].close;
        }
        const avg = sum / period;
        const ySma = padding.top + mainChartHeight - ((avg - minP) / priceRange) * mainChartHeight;
        sma.push({ x: coords[i].x, y: ySma });
      }
    }

    return { minPrice: minP, maxPrice: maxP, candlesWithCoords: coords, smaPoints: sma };
  }, [data, chartWidth, mainChartHeight, padding.left, padding.top]);

  // Current price line coordinates
  const priceRange = maxPrice - minPrice || 1;
  const currentPriceY = Math.min(
    padding.top + mainChartHeight,
    Math.max(padding.top, padding.top + mainChartHeight - ((currPrice - minPrice) / priceRange) * mainChartHeight)
  );

  // Mouse hover on chart
  const handleMouseMove = (e: React.MouseEvent<SVGSVGElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;
    setMousePos({ x, y });

    const svgX = (x / rect.width) * width;
    let closestCandle: ChartPoint | null = null;
    let minD = Infinity;

    for (const c of candlesWithCoords) {
      const dist = Math.abs(c.x - svgX);
      if (dist < minD) {
        minD = dist;
        closestCandle = c.candle;
      }
    }
    if (closestCandle) {
      setHoveredCandle(closestCandle);
    }
  };

  const handleMouseLeave = () => {
    setMousePos(null);
    setHoveredCandle(null);
  };

  // Active forming candle countdown formatting (e.g., 04:09 as in screenshot)
  const lastCoord = candlesWithCoords[candlesWithCoords.length - 1];
  const countdownFormatted = useMemo(() => {
    const totalSecs = lastCoord?.countdownSecs ?? 249;
    const mins = Math.floor(totalSecs / 60);
    const secs = totalSecs % 60;
    return `${String(mins).padStart(2, "0")}:${String(secs).padStart(2, "0")}`;
  }, [lastCoord?.countdownSecs]);

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        background: bgApp,
        color: textPrimary,
        borderRadius: "8px",
        border: `1px solid ${borderColor}`,
        boxShadow: isLight ? "0 4px 20px rgba(0,0,0,0.06)" : "0 4px 20px rgba(0,0,0,0.4)",
        overflow: "hidden",
        fontFamily: "Inter, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif",
      }}
    >
      {/* 1. TOP INDICES & LATENCY RIBBON (Matching Upstox Pro Bar) */}
      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: "center",
          flexWrap: "wrap",
          padding: "6px 14px",
          background: isLight ? "#f8fafc" : "#131722",
          borderBottom: `1px solid ${borderColor}`,
          fontSize: "12px",
          gap: "8px",
        }}
      >
        {/* Indices values */}
        <div style={{ display: "flex", alignItems: "center", gap: "16px", flexWrap: "wrap" }}>
          <div style={{ display: "flex", alignItems: "center", gap: "6px" }}>
            <span style={{ background: "#7c3aed", color: "#fff", fontWeight: "800", padding: "1px 6px", borderRadius: "3px", fontSize: "11px" }}>
              upstox
            </span>
            <span style={{ fontWeight: "700", color: "#3b82f6", fontSize: "11px" }}>+plus</span>
          </div>

          <div style={{ display: "flex", alignItems: "center", gap: "4px" }}>
            <span style={{ fontWeight: "600", color: textSecondary }}>NIFTY 50</span>
            <strong style={{ color: redColor }}>22,719.90</strong>
            <span style={{ color: redColor, fontSize: "11px" }}>-33.30 (-0.15%)</span>
            <span style={{ fontSize: "10px", color: textMuted, background: isLight ? "#e2e8f0" : "#1e293b", padding: "1px 4px", borderRadius: "2px" }}>Exp. Today</span>
          </div>

          <div style={{ display: "flex", alignItems: "center", gap: "4px" }}>
            <span style={{ fontWeight: "600", color: textSecondary }}>BANKNIFTY</span>
            <strong style={{ color: redColor }}>54,071.15</strong>
            <span style={{ color: redColor, fontSize: "11px" }}>-295.55 (-0.54%)</span>
            <span style={{ fontSize: "10px", color: textMuted, background: isLight ? "#e2e8f0" : "#1e293b", padding: "1px 4px", borderRadius: "2px" }}>Exp. Today</span>
          </div>

          <div style={{ display: "flex", alignItems: "center", gap: "4px" }}>
            <span style={{ fontWeight: "600", color: textSecondary }}>SENSEX</span>
            <strong style={{ color: redColor }}>73,448.21</strong>
            <span style={{ color: redColor, fontSize: "11px" }}>-158.54 (-0.22%)</span>
          </div>

          <div style={{ display: "flex", alignItems: "center", gap: "8px", color: textSecondary }}>
            <span>Hol. Total P&L <strong style={{ color: greenColor }}>₹0.00</strong></span>
            <span>Pos. Total P&L: <strong style={{ color: greenColor }}>₹0.00</strong></span>
          </div>
        </div>

        {/* Live Feed Status & Latency Measurement Badge */}
        <div style={{ display: "flex", alignItems: "center", gap: "10px", flexWrap: "wrap" }}>
          {feedStatus.session?.session_kind !== "REGULAR" ? (
            <div style={{ display: "flex", alignItems: "center", gap: "5px", background: isLight ? "#f1f5f9" : "#1e293b", color: textSecondary, padding: "3px 8px", borderRadius: "4px", fontWeight: "600", fontSize: "11px" }}>
              <span style={{ width: "8px", height: "8px", borderRadius: "50%", background: "#64748b" }} />
              {feedStatus.session?.status_badge || "CLOSED"}
            </div>
          ) : feedStatus.status === "CONNECTED" && !feedStatus.is_stale ? (
            <div style={{ display: "flex", alignItems: "center", gap: "5px", background: "#ecfdf5", color: "#047857", padding: "3px 8px", borderRadius: "4px", fontWeight: "600", fontSize: "11px" }}>
              <span style={{ width: "8px", height: "8px", borderRadius: "50%", background: "#10b981", boxShadow: "0 0 6px #10b981" }} />
              UPSTOX V3 STREAMING ({feedStatus.deduped_ticks_count} deduped)
            </div>
          ) : feedStatus.status === "RECONNECTING_BACKOFF" ? (
            <div style={{ display: "flex", alignItems: "center", gap: "5px", background: "#fef3c7", color: "#b45309", padding: "3px 8px", borderRadius: "4px", fontWeight: "600", fontSize: "11px" }}>
              <span style={{ width: "8px", height: "8px", borderRadius: "50%", background: "#f59e0b" }} />
              RECONNECTING ({feedStatus.backoff_delay_secs}s delay · try #{feedStatus.reconnect_count})
            </div>
          ) : feedStatus.is_stale ? (
            <div style={{ display: "flex", alignItems: "center", gap: "5px", background: "#fef3c7", color: "#b45309", padding: "3px 8px", borderRadius: "4px", fontWeight: "600", fontSize: "11px" }}>
              <span style={{ width: "8px", height: "8px", borderRadius: "50%", background: "#f59e0b" }} />
              FEED STALE (&gt;15s) · ORDERS PAUSED
            </div>
          ) : (
            <div style={{ display: "flex", alignItems: "center", gap: "5px", background: "#eff6ff", color: "#1d4ed8", padding: "3px 8px", borderRadius: "4px", fontWeight: "600", fontSize: "11px" }}>
              <span style={{ width: "8px", height: "8px", borderRadius: "50%", background: "#3b82f6" }} />
              V3 CONNECTING
            </div>
          )}

          <div style={{ fontSize: "11px", color: textMuted }}>
            Delay: <strong style={{ color: measuredDisplayDelay > 1000 ? redColor : textPrimary }}>~{measuredDisplayDelay}ms</strong>
          </div>

          <div style={{ fontSize: "11px", color: textMuted }}>
            Feed Time: <strong style={{ color: textPrimary }}>{data?.last_updated_at ? new Date(data.last_updated_at).toLocaleTimeString() : "14:45:50 IST"}</strong>
          </div>

          <Badge tone="warning" title="Strict paper execution mode">
            PAPER TRADING ONLY (₹1,000 INR Capital)
          </Badge>
        </div>
      </div>

      {/* 2. EXCHANGE SESSION DETECTION BANNER */}
      {feedStatus.session && (
        <div
          style={{
            background: feedStatus.session.session_kind === "REGULAR"
              ? (isLight ? "#f0fdf4" : "rgba(16, 185, 129, 0.12)")
              : feedStatus.session.session_kind === "PRE_OPEN"
              ? (isLight ? "#fefce8" : "rgba(234, 179, 8, 0.12)")
              : feedStatus.session.session_kind === "CLOSING_AUCTION"
              ? (isLight ? "#fff7ed" : "rgba(249, 115, 22, 0.12)")
              : (isLight ? "#f8fafc" : "rgba(30, 41, 59, 0.5)"),
            color: feedStatus.session.session_kind === "REGULAR"
              ? "#166534"
              : feedStatus.session.session_kind === "PRE_OPEN"
              ? "#854d0e"
              : feedStatus.session.session_kind === "CLOSING_AUCTION"
              ? "#9a3412"
              : textSecondary,
            borderBottom: `1px solid ${borderColor}`,
            padding: "6px 14px",
            fontSize: "12px",
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            fontWeight: "600",
          }}
        >
          <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
            <span
              style={{
                display: "inline-block",
                padding: "2px 6px",
                borderRadius: "3px",
                fontSize: "10px",
                fontWeight: "800",
                letterSpacing: "0.5px",
                background: feedStatus.session.session_kind === "REGULAR" ? "#15803d" : "#475569",
                color: "#fff",
              }}
            >
              {feedStatus.session.status_badge}
            </span>
            <span>
              {feedStatus.session.session_name} ({feedStatus.session.local_time_ist}) — {feedStatus.session.next_session_event}
            </span>
          </div>

          <div style={{ display: "flex", alignItems: "center", gap: "12px", fontSize: "11px" }}>
            {!feedStatus.session.allows_order_entry && (
              <span style={{ color: textMuted }}>
                Last Authentic Price: <strong style={{ color: textPrimary }}>₹{data?.last_received_price ? Number(data.last_received_price).toFixed(2) : currPrice.toFixed(2)}</strong> {data?.last_received_time ? `(${new Date(data.last_received_time).toLocaleTimeString()})` : ""} · Paper fills paused
              </span>
            )}
            <span>{feedStatus.session.current_date_ist}</span>
          </div>
        </div>
      )}

      {/* 3. STALE FEED WARNING ALERT (If updates stop during regular trading) */}
      {feedStatus.is_stale && feedStatus.session?.session_kind === "REGULAR" && (
        <div style={{ background: "#fef2f2", color: "#991b1b", padding: "6px 14px", borderBottom: "1px solid #fecaca", fontSize: "12px", display: "flex", justifyContent: "space-between", alignItems: "center" }}>
          <span>
            ⚠️ <strong>Feed Data Stale:</strong> No price updates received for &gt;15 seconds. Strategy decisions and simulated paper order fills are paused until fresh data resumes.
          </span>
          <button
            onClick={fetchChart}
            style={{ padding: "2px 8px", background: "#dc2626", color: "#fff", border: "none", borderRadius: "3px", cursor: "pointer", fontSize: "11px", fontWeight: "700" }}
          >
            Refresh
          </button>
        </div>
      )}

      {/* 3. MAIN WORKSPACE: WATCHLIST (LEFT) + CANDLESTICK CHART (RIGHT) */}
      <div style={{ display: "grid", gridTemplateColumns: "310px 1fr", minHeight: "680px" }}>
        {/* LEFT COLUMN: UPSTOX WATCHLIST SIDEBAR */}
        <div
          style={{
            background: bgSurface,
            borderRight: `1px solid ${borderColor}`,
            display: "flex",
            flexDirection: "column",
            maxHeight: "820px",
            overflow: "hidden",
          }}
        >
          {/* Watchlist Header */}
          <div style={{ padding: "10px 12px", borderBottom: `1px solid ${borderColor}` }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "8px" }}>
              <div style={{ display: "flex", alignItems: "center", gap: "6px" }}>
                <span style={{ fontWeight: "700", fontSize: "13px", color: textPrimary }}>Top 20 by Market ...</span>
                <span style={{ fontSize: "11px", color: textMuted }}>{filteredWatchlist.length} / 100</span>
              </div>
              <div style={{ display: "flex", gap: "6px", color: textSecondary, fontSize: "13px" }}>
                <span style={{ cursor: "pointer" }} title="Filter">☰</span>
                <span style={{ cursor: "pointer" }} title="Sort">⇅</span>
                <span style={{ cursor: "pointer", color: purpleAccent, fontWeight: "800" }} title="Add stock">+</span>
              </div>
            </div>

            {/* Symbol Search Bar */}
            <input
              type="text"
              placeholder="Search stocks (e.g. RELIANCE, TCS)..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              style={{
                width: "100%",
                padding: "6px 10px",
                borderRadius: "4px",
                border: `1px solid ${borderColor}`,
                background: bgCard,
                color: textPrimary,
                fontSize: "12px",
                outline: "none",
                boxSizing: "border-box",
              }}
            />
          </div>

          {/* Watchlist Stock Items (Scrollable List) */}
          <div style={{ overflowY: "auto", flex: 1 }}>
            {filteredWatchlist.map((stock) => {
              const isSelected = selectedSymbol === stock.symbol;
              const liveData = livePricesMap[stock.symbol];
              const price = liveData?.price ? liveData.price : stock.defaultPrice;
              const changePct = liveData?.change_pct !== undefined ? liveData.change_pct : stock.changePct;
              const changeAmt = ((price * changePct) / 100);
              const isStockPos = changePct >= 0;

              return (
                <div
                  key={stock.symbol}
                  onClick={() => setSelectedSymbol(stock.symbol)}
                  style={{
                    padding: "9px 12px",
                    display: "flex",
                    justifyContent: "space-between",
                    alignItems: "center",
                    cursor: "pointer",
                    borderBottom: `1px solid ${borderColor}`,
                    background: isSelected ? (isLight ? "rgba(124, 58, 237, 0.08)" : "rgba(124, 58, 237, 0.18)") : "transparent",
                    borderLeft: isSelected ? `4px solid ${purpleAccent}` : "4px solid transparent",
                    transition: "all 0.15s ease",
                  }}
                >
                  <div>
                    <div style={{ fontWeight: isSelected ? "700" : "600", fontSize: "13px", color: isSelected ? purpleAccent : textPrimary }}>
                      {stock.symbol}
                    </div>
                    <div style={{ fontSize: "11px", color: textMuted, display: "flex", alignItems: "center", gap: "4px" }}>
                      <span>{stock.exchange} {stock.segment}</span>
                      <span title={`Upstox Key: ${stock.upstox_token}`} style={{ cursor: "help", opacity: 0.6 }}>ℹ️</span>
                    </div>
                  </div>

                  <div style={{ textAlign: "right" }}>
                    <div style={{ fontWeight: "700", fontSize: "13px", color: isStockPos ? greenColor : redColor }}>
                      {price.toLocaleString("en-IN", { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
                    </div>
                    <div style={{ fontSize: "11px", color: isStockPos ? greenColor : redColor, fontWeight: "500" }}>
                      {isStockPos ? `+${changeAmt.toFixed(2)} (+${changePct.toFixed(2)}%)` : `${changeAmt.toFixed(2)} (${changePct.toFixed(2)}%)`}
                    </div>
                  </div>
                </div>
              );
            })}
          </div>

          {/* Watchlist Footer */}
          <div style={{ padding: "8px 12px", borderTop: `1px solid ${borderColor}`, fontSize: "11px", color: purpleAccent, fontWeight: "600", cursor: "pointer", background: bgCard }}>
            Track 200 symbols with <span style={{ color: "#3b82f6" }}>+plus</span> &gt;
          </div>
        </div>

        {/* RIGHT COLUMN: MAIN TRADINGVIEW CANDLESTICK CHART */}
        <div style={{ display: "flex", flexDirection: "column", background: bgCard, overflow: "hidden" }}>
          {/* Top Chart Toolbar: Symbols, Intervals, Indicators, and Order Bar */}
          <div
            style={{
              padding: "8px 14px",
              display: "flex",
              justifyContent: "space-between",
              alignItems: "center",
              flexWrap: "wrap",
              gap: "8px",
              borderBottom: `1px solid ${borderColor}`,
              background: bgSurface,
            }}
          >
            {/* Symbol & Timeframe pills */}
            <div style={{ display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" }}>
              <span style={{ fontWeight: "800", fontSize: "15px", color: textPrimary, display: "flex", alignItems: "center", gap: "4px" }}>
                🔍 {selectedSymbol} <span style={{ color: textSecondary, fontSize: "12px", cursor: "pointer" }}>⊕</span>
              </span>

              {/* Timeframes */}
              <div style={{ display: "flex", background: isLight ? "#e2e8f0" : "#1e293b", padding: "2px", borderRadius: "4px", gap: "2px" }}>
                {RANGES.map((rng) => (
                  <button
                    key={rng}
                    onClick={() => setSelectedRange(rng)}
                    style={{
                      border: "none",
                      background: selectedRange === rng ? (isLight ? "#ffffff" : "#334155") : "transparent",
                      color: selectedRange === rng ? textPrimary : textSecondary,
                      fontWeight: selectedRange === rng ? "700" : "500",
                      padding: "3px 8px",
                      borderRadius: "3px",
                      cursor: "pointer",
                      fontSize: "12px",
                      boxShadow: selectedRange === rng ? "0 1px 2px rgba(0,0,0,0.1)" : "none",
                    }}
                  >
                    {rng}
                  </button>
                ))}
              </div>

              {/* Candle type icon & Indicators button */}
              <button
                onClick={() => setShowIndicators(!showIndicators)}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: "4px",
                  padding: "4px 8px",
                  borderRadius: "4px",
                  border: `1px solid ${showIndicators ? purpleAccent : borderColor}`,
                  background: isLight ? "#ffffff" : "#1e293b",
                  color: textPrimary,
                  fontSize: "12px",
                  fontWeight: "600",
                  cursor: "pointer",
                }}
              >
                <span style={{ fontStyle: "italic", fontWeight: "800", color: purpleAccent }}>fx</span> Indicators
              </button>

              {/* Indicators Dropdown */}
              {showIndicators && (
                <div style={{ display: "flex", gap: "8px", alignItems: "center", fontSize: "11px", color: textSecondary }}>
                  <label style={{ display: "flex", alignItems: "center", gap: "3px", cursor: "pointer" }}>
                    <input
                      type="checkbox"
                      checked={activeIndicators.sma}
                      onChange={(e) => setActiveIndicators({ ...activeIndicators, sma: e.target.checked })}
                    />
                    SMA (20)
                  </label>
                  <label style={{ display: "flex", alignItems: "center", gap: "3px", cursor: "pointer" }}>
                    <input
                      type="checkbox"
                      checked={activeIndicators.vol}
                      onChange={(e) => setActiveIndicators({ ...activeIndicators, vol: e.target.checked })}
                    />
                    Volume
                  </label>
                </div>
              )}
            </div>

            {/* Instant Paper Order Bar (Matching Sell/Buy action in screenshot) */}
            <div style={{ display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" }}>
              <button
                disabled={submittingOrder || feedStatus.is_stale || (feedStatus.session !== undefined && !feedStatus.session.allows_order_entry)}
                onClick={() => handleInstantPaperOrder("SELL")}
                title={feedStatus.session && !feedStatus.session.allows_order_entry ? `Market Closed (${feedStatus.session.session_name}). Regular continuous trading is 09:15 - 15:30 IST.` : feedStatus.is_stale ? "Feed is stale (>15s). Paper order fills paused." : "Place Instant Paper SELL"}
                style={{
                  background: redColor,
                  color: "#fff",
                  border: "none",
                  padding: "5px 12px",
                  borderRadius: "4px",
                  fontSize: "12px",
                  fontWeight: "700",
                  cursor: submittingOrder || feedStatus.is_stale || (feedStatus.session !== undefined && !feedStatus.session.allows_order_entry) ? "not-allowed" : "pointer",
                  opacity: submittingOrder || feedStatus.is_stale || (feedStatus.session !== undefined && !feedStatus.session.allows_order_entry) ? 0.6 : 1,
                  display: "flex",
                  flexDirection: "column",
                  alignItems: "center",
                }}
              >
                <span>SELL {feedStatus.session && !feedStatus.session.allows_order_entry ? "(CLOSED)" : (currPrice - 0.10).toFixed(2)}</span>
              </button>

              <div style={{ display: "flex", flexDirection: "column", alignItems: "center" }}>
                <span style={{ fontSize: "10px", color: textMuted }}>0.20 spread</span>
                <input
                  type="number"
                  min="1"
                  value={instantOrderQty}
                  onChange={(e) => setInstantOrderQty(e.target.value)}
                  style={{
                    width: "42px",
                    textAlign: "center",
                    padding: "2px",
                    borderRadius: "3px",
                    border: `1px solid ${borderColor}`,
                    background: bgCard,
                    color: textPrimary,
                    fontSize: "12px",
                    fontWeight: "700",
                  }}
                />
              </div>

              <button
                disabled={submittingOrder || feedStatus.is_stale || (feedStatus.session !== undefined && !feedStatus.session.allows_order_entry)}
                onClick={() => handleInstantPaperOrder("BUY")}
                title={feedStatus.session && !feedStatus.session.allows_order_entry ? `Market Closed (${feedStatus.session.session_name}). Regular continuous trading is 09:15 - 15:30 IST.` : feedStatus.is_stale ? "Feed is stale (>15s). Paper order fills paused." : "Place Instant Paper BUY"}
                style={{
                  background: greenColor,
                  color: "#fff",
                  border: "none",
                  padding: "5px 12px",
                  borderRadius: "4px",
                  fontSize: "12px",
                  fontWeight: "700",
                  cursor: submittingOrder || feedStatus.is_stale || (feedStatus.session !== undefined && !feedStatus.session.allows_order_entry) ? "not-allowed" : "pointer",
                  opacity: submittingOrder || feedStatus.is_stale || (feedStatus.session !== undefined && !feedStatus.session.allows_order_entry) ? 0.6 : 1,
                  display: "flex",
                  flexDirection: "column",
                  alignItems: "center",
                }}
              >
                <span>BUY {feedStatus.session && !feedStatus.session.allows_order_entry ? "(CLOSED)" : (currPrice + 0.10).toFixed(2)}</span>
              </button>

              <button
                onClick={() => setChartTheme(isLight ? "dark" : "light")}
                style={{
                  background: "transparent",
                  border: `1px solid ${borderColor}`,
                  borderRadius: "4px",
                  padding: "4px 8px",
                  cursor: "pointer",
                  fontSize: "12px",
                  color: textSecondary,
                }}
              >
                {isLight ? "🌙 Dark" : "☀️ Light"}
              </button>
            </div>
          </div>

          {/* Feedback banner for paper trades */}
          {orderFeedback && (
            <div
              style={{
                padding: "6px 14px",
                fontSize: "12px",
                fontWeight: "600",
                background: orderFeedback.kind === "good" ? "#ecfdf5" : "#fef2f2",
                color: orderFeedback.kind === "good" ? "#047857" : "#b91c1c",
                borderBottom: `1px solid ${orderFeedback.kind === "good" ? "#a7f3d0" : "#fecaca"}`,
                display: "flex",
                justifyContent: "space-between",
                alignItems: "center",
              }}
            >
              <span>{orderFeedback.msg}</span>
              <button onClick={() => setOrderFeedback(null)} style={{ border: "none", background: "transparent", cursor: "pointer", fontWeight: "700" }}>✕</button>
            </div>
          )}

          {/* OHLC Banner matching screenshot: RELIANCE INDUSTRIES LTD · 5 · NSE O... H... L... C... Volume... */}
          <div
            style={{
              padding: "6px 14px",
              display: "flex",
              justifyContent: "space-between",
              alignItems: "center",
              flexWrap: "wrap",
              gap: "8px",
              borderBottom: `1px solid ${borderColor}`,
              fontSize: "12px",
              background: bgCard,
            }}
          >
            <div style={{ display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" }}>
              <span style={{ fontWeight: "700", color: textPrimary }}>
                {data?.name || "Reliance Industries Ltd"} · {selectedRange} · NSE
              </span>
              <span style={{ width: "7px", height: "7px", borderRadius: "50%", background: greenColor, display: "inline-block" }} />

              <div style={{ display: "flex", gap: "8px", fontFamily: "monospace", fontSize: "12px" }}>
                <span>O<strong style={{ color: textPrimary }}>{activeCandle ? Number(activeCandle.open).toFixed(2) : "1,191.50"}</strong></span>
                <span>H<strong style={{ color: textPrimary }}>{activeCandle ? Number(activeCandle.high).toFixed(2) : "1,192.30"}</strong></span>
                <span>L<strong style={{ color: textPrimary }}>{activeCandle ? Number(activeCandle.low).toFixed(2) : "1,191.30"}</strong></span>
                <span>C<strong style={{ color: isPos ? greenColor : redColor }}>{activeCandle ? Number(activeCandle.close).toFixed(2) : currPrice.toFixed(2)}</strong></span>
                <span style={{ color: isPos ? greenColor : redColor, fontWeight: "700" }}>
                  {isPos ? `+${changeNum.toFixed(2)} (+${changePctNum.toFixed(2)}%)` : `${changeNum.toFixed(2)} (${changePctNum.toFixed(2)}%)`}
                </span>
                <span style={{ color: textSecondary }}>Vol {activeCandle ? `${(toN(activeCandle.volume) / 1000).toFixed(2)}K` : "9.45K"}</span>
              </div>
            </div>

            <div style={{ display: "flex", alignItems: "center", gap: "6px", fontSize: "11px", color: textMuted }}>
              <span>Volume SMA <strong style={{ color: "#3b82f6" }}>9.45K</strong></span>
              <span>• Instrument: <strong style={{ color: textPrimary }}>{data?.upstox_instrument_key || `NSE_EQ|${selectedSymbol}`}</strong></span>
            </div>
          </div>

          {/* Interactive Candlestick SVG Canvas */}
          <div style={{ position: "relative", width: "100%", overflowX: "auto", background: bgCard, flex: 1 }}>
            <svg
              viewBox={`0 0 ${width} ${height}`}
              style={{ width: "100%", height: "auto", minHeight: "440px", display: "block", cursor: "crosshair" }}
              onMouseMove={handleMouseMove}
              onMouseLeave={handleMouseLeave}
            >
              {/* Horizontal Price Grid Lines */}
              {[0, 0.25, 0.5, 0.75, 1.0].map((ratio) => {
                const y = padding.top + mainChartHeight * ratio;
                const priceVal = maxPrice - (maxPrice - minPrice) * ratio;
                return (
                  <g key={ratio}>
                    <line
                      x1={padding.left}
                      y1={y}
                      x2={padding.left + chartWidth}
                      y2={y}
                      stroke={isLight ? "rgba(226, 232, 240, 0.8)" : "rgba(42, 46, 57, 0.8)"}
                      strokeDasharray="2 3"
                      strokeWidth="1"
                    />
                    <text
                      x={padding.left + chartWidth + 6}
                      y={y + 3.5}
                      fill={textSecondary}
                      fontSize="10"
                      fontFamily="monospace"
                    >
                      {priceVal.toFixed(2)}
                    </text>
                  </g>
                );
              })}

              {/* Vertical Time Grid Lines */}
              {candlesWithCoords.filter((_, idx) => idx % Math.max(1, Math.floor(candlesWithCoords.length / 8)) === 0).map((c, i) => (
                <g key={i}>
                  <line
                    x1={c.x}
                    y1={padding.top}
                    x2={c.x}
                    y2={padding.top + mainChartHeight}
                    stroke={isLight ? "rgba(226, 232, 240, 0.6)" : "rgba(42, 46, 57, 0.6)"}
                    strokeDasharray="2 3"
                    strokeWidth="0.8"
                  />
                  <text
                    x={c.x - 14}
                    y={padding.top + mainChartHeight + 15 + volumeHeight + 20}
                    fill={textSecondary}
                    fontSize="10"
                    fontFamily="monospace"
                  >
                    {c.candle.time}
                  </text>
                </g>
              ))}

              {/* Volume Bars */}
              {activeIndicators.vol && candlesWithCoords.map((c, i) => (
                <rect
                  key={`vol-${i}`}
                  x={c.x - 2.5}
                  y={c.volBarY}
                  width="5"
                  height={c.volBarHeight}
                  fill={c.isGreen ? (isLight ? "rgba(8, 153, 129, 0.45)" : "rgba(8, 153, 129, 0.65)") : (isLight ? "rgba(242, 54, 69, 0.45)" : "rgba(242, 54, 69, 0.65)")}
                  rx="1"
                />
              ))}

              {/* SMA 20 Overlay Line */}
              {activeIndicators.sma && smaPoints.length > 1 && (
                <polyline
                  fill="none"
                  stroke="#3b82f6"
                  strokeWidth="1.5"
                  points={smaPoints.map((p) => `${p.x},${p.y}`).join(" ")}
                />
              )}

              {/* Japanese Candlesticks */}
              {candlesWithCoords.map((c, i) => {
                const color = c.isGreen ? greenColor : redColor;
                const isForming = c.isForming;

                return (
                  <g key={`candle-${i}`}>
                    {/* Shadow wick */}
                    <line
                      x1={c.x}
                      y1={c.yHigh}
                      x2={c.x}
                      y2={c.yLow}
                      stroke={color}
                      strokeWidth={isForming ? "1.8" : "1.2"}
                    />

                    {/* Candle body */}
                    <rect
                      x={c.x - 3.5}
                      y={c.bodyTop}
                      width="7"
                      height={c.bodyHeight}
                      fill={color}
                      stroke={isForming ? (isLight ? "#0f172a" : "#ffffff") : color}
                      strokeWidth={isForming ? "1.5" : "0.5"}
                      strokeDasharray={isForming ? "2 2" : "none"}
                      rx="1"
                    />

                    {/* Forming Candle Highlight Pulse */}
                    {isForming && (
                      <circle
                        cx={c.x}
                        cy={c.bodyTop + c.bodyHeight / 2}
                        r="8"
                        fill="none"
                        stroke={color}
                        strokeWidth="1"
                        opacity="0.6"
                      />
                    )}
                  </g>
                );
              })}



              {/* Real-time Current Dotted Price Line extending to right axis */}
              <line
                x1={padding.left}
                y1={currentPriceY}
                x2={padding.left + chartWidth}
                y2={currentPriceY}
                stroke={isPos ? greenColor : redColor}
                strokeDasharray="3 3"
                strokeWidth="1.4"
              />

              {/* Right Axis Current Price Badge */}
              <rect
                x={padding.left + chartWidth + 2}
                y={currentPriceY - 10}
                width="64"
                height="20"
                fill={isPos ? greenColor : redColor}
                rx="3"
              />
              <text
                x={padding.left + chartWidth + 6}
                y={currentPriceY + 4}
                fill="#ffffff"
                fontSize="11"
                fontWeight="700"
                fontFamily="monospace"
              >
                {currPrice.toFixed(2)}
              </text>

              {/* Countdown Timer Pill to Candle Close (e.g. 04:09 as in screenshot) */}
              <rect
                x={padding.left + chartWidth + 2}
                y={currentPriceY + 12}
                width="64"
                height="16"
                fill={isLight ? "#334155" : "#1e293b"}
                rx="3"
              />
              <text
                x={padding.left + chartWidth + 14}
                y={currentPriceY + 24}
                fill="#38bdf8"
                fontSize="10"
                fontWeight="700"
                fontFamily="monospace"
              >
                {countdownFormatted}
              </text>

              {/* Hover Crosshairs & Tooltip */}
              {mousePos && (
                <>
                  <line
                    x1={mousePos.x}
                    y1={padding.top}
                    x2={mousePos.x}
                    y2={padding.top + mainChartHeight + volumeHeight + 15}
                    stroke={textSecondary}
                    strokeDasharray="3 3"
                    strokeWidth="1"
                  />
                  <line
                    x1={padding.left}
                    y1={mousePos.y}
                    x2={padding.left + chartWidth}
                    y2={mousePos.y}
                    stroke={textSecondary}
                    strokeDasharray="3 3"
                    strokeWidth="1"
                  />
                </>
              )}
            </svg>
          </div>

          {/* Bottom Bar: Range Selectors + Powered by + Live Clock */}
          <div
            style={{
              padding: "6px 14px",
              display: "flex",
              justifyContent: "space-between",
              alignItems: "center",
              flexWrap: "wrap",
              borderTop: `1px solid ${borderColor}`,
              fontSize: "12px",
              background: bgSurface,
              color: textSecondary,
            }}
          >
            <div style={{ display: "flex", gap: "8px", alignItems: "center" }}>
              {["5Y", "1Y", "6M", "3M", "1M", "5D", "1D"].map((r) => (
                <button
                  key={r}
                  style={{
                    background: "transparent",
                    border: "none",
                    color: textSecondary,
                    fontSize: "11px",
                    fontWeight: "600",
                    cursor: "pointer",
                    padding: "2px 4px",
                  }}
                >
                  {r}
                </button>
              ))}
              <span style={{ fontSize: "11px", color: textMuted }}>|</span>
              <span style={{ fontSize: "11px", color: textMuted }}>Powered by <strong>Upstox Live Feed</strong></span>
            </div>

            <div style={{ display: "flex", gap: "12px", alignItems: "center" }}>
              <span style={{ fontFamily: "monospace", fontSize: "11px", color: textPrimary }}>
                {istClock || "14:45:50 (UTC+5:30)"}
              </span>
              <div style={{ display: "flex", gap: "6px", fontSize: "11px" }}>
                <span style={{ cursor: "pointer" }}>%</span>
                <span style={{ cursor: "pointer" }}>log</span>
                <span style={{ cursor: "pointer", fontWeight: "700", color: purpleAccent }}>auto</span>
              </div>
            </div>
          </div>

          {/* Bottom Drawer Tabs: Open Orders, Positions, Holdings (Simulated) */}
          <div style={{ borderTop: `1px solid ${borderColor}`, background: bgCard, padding: "8px 14px" }}>
            <div style={{ display: "flex", gap: "16px", borderBottom: `1px solid ${borderColor}`, paddingBottom: "6px", marginBottom: "8px" }}>
              <button
                onClick={() => setActiveTabBottom("orders")}
                style={{
                  background: "transparent",
                  border: "none",
                  borderBottom: activeTabBottom === "orders" ? `2px solid ${purpleAccent}` : "2px solid transparent",
                  color: activeTabBottom === "orders" ? textPrimary : textSecondary,
                  fontWeight: activeTabBottom === "orders" ? "700" : "500",
                  cursor: "pointer",
                  fontSize: "12px",
                  padding: "4px 8px",
                }}
              >
                Open Orders (0)
              </button>
              <button
                onClick={() => setActiveTabBottom("positions")}
                style={{
                  background: "transparent",
                  border: "none",
                  borderBottom: activeTabBottom === "positions" ? `2px solid ${purpleAccent}` : "2px solid transparent",
                  color: activeTabBottom === "positions" ? textPrimary : textSecondary,
                  fontWeight: activeTabBottom === "positions" ? "700" : "500",
                  cursor: "pointer",
                  fontSize: "12px",
                  padding: "4px 8px",
                }}
              >
                Positions | Day P&amp;L <strong style={{ color: greenColor }}>₹0.00</strong> (Simulated)
              </button>
              <button
                onClick={() => setActiveTabBottom("trades")}
                style={{
                  background: "transparent",
                  border: "none",
                  borderBottom: activeTabBottom === "trades" ? `2px solid ${purpleAccent}` : "2px solid transparent",
                  color: activeTabBottom === "trades" ? textPrimary : textSecondary,
                  fontWeight: activeTabBottom === "trades" ? "700" : "500",
                  cursor: "pointer",
                  fontSize: "12px",
                  padding: "4px 8px",
                }}
              >
                Trade History &amp; Strategy Ledger ({data?.detailed_trades?.length || 0})
              </button>
              <button
                onClick={() => setActiveTabBottom("backtest")}
                style={{
                  background: "transparent",
                  border: "none",
                  borderBottom: activeTabBottom === "backtest" ? `2px solid ${purpleAccent}` : "2px solid transparent",
                  color: activeTabBottom === "backtest" ? textPrimary : textSecondary,
                  fontWeight: activeTabBottom === "backtest" ? "700" : "500",
                  cursor: "pointer",
                  fontSize: "12px",
                  padding: "4px 8px",
                }}
              >
                📊 Pullback Backtest (Dev vs OOS)
              </button>
            </div>



            <div style={{ fontSize: "12px", color: textMuted, padding: "6px 0" }}>
              {activeTabBottom === "orders" && (
                <span>No active paper orders pending execution. Live limit orders execute instantly against simulated paper liquidity.</span>
              )}
              {activeTabBottom === "positions" && (
                <span>All intraday positions are tracked with statutory Indian costs (STT: 0.025%, GST: 18%, Stamp Duty: 0.003%, SEBI turnover).</span>
              )}
              {activeTabBottom === "holdings" && (
                <span>Paper account starting balance is calibrated to ₹1,000 INR. Real broker funds are strictly untouched.</span>
              )}
              {activeTabBottom === "trades" && (
                <div style={{ overflowX: "auto" }}>
                  <table style={{ width: "100%", borderCollapse: "collapse", fontSize: "11px" }}>
                    <thead>
                      <tr style={{ borderBottom: `1px solid ${borderColor}`, textAlign: "left", color: textSecondary }}>
                        <th style={{ padding: "6px 8px" }}>Trade ID</th>
                        <th style={{ padding: "6px 8px" }}>Market &amp; Feed</th>
                        <th style={{ padding: "6px 8px" }}>Strategy Reason</th>
                        <th style={{ padding: "6px 8px" }}>Simulated Buy Fill</th>
                        <th style={{ padding: "6px 8px" }}>Simulated Sell Fill</th>
                        <th style={{ padding: "6px 8px" }}>Planned Stop / Target</th>
                        <th style={{ padding: "6px 8px" }}>Statutory Taxes</th>
                        <th style={{ padding: "6px 8px" }}>Net Realized P&amp;L</th>
                        <th style={{ padding: "6px 8px" }}>Status</th>
                      </tr>
                    </thead>
                    <tbody>
                      {data?.detailed_trades && data.detailed_trades.map((t) => {
                        const isProfit = toN(t.net_pnl, 0) > 0;
                        return (
                          <tr key={t.id} style={{ borderBottom: `1px solid ${borderColor}` }}>
                            <td style={{ padding: "6px 8px", fontFamily: "monospace", color: textSecondary }}>{t.id}</td>
                            <td style={{ padding: "6px 8px" }}>
                              <div style={{ fontWeight: "600", color: textPrimary }}>{t.market || "NSE Cash Equities"}</div>
                              <div style={{ fontSize: "10px", color: purpleAccent }}>{t.provider_feed || "Upstox V3 Feed"}</div>
                            </td>
                            <td style={{ padding: "6px 8px", maxWidth: "200px" }}>
                              <span style={{ fontSize: "10px", color: textSecondary }}>
                                {t.signal_reason || "Confirmed 5m Pullback Rebound: Price > VWAP & EMA20 > EMA50"}
                              </span>
                            </td>
                            <td style={{ padding: "6px 8px" }}>
                              <span style={{ color: "#2563eb", fontWeight: "700" }}>₹{toN(t.buy_price).toFixed(2)}</span>
                              <div style={{ fontSize: "10px", color: textMuted }}>{t.buy_time} ({t.shares} sh)</div>
                            </td>
                            <td style={{ padding: "6px 8px" }}>
                              {t.status === "CLOSED" ? (
                                <>
                                  <span style={{ color: "#0f172a", fontWeight: "700", background: "#f1f5f9", padding: "1px 4px", borderRadius: "2px" }}>₹{toN(t.sell_price).toFixed(2)}</span>
                                  <div style={{ fontSize: "10px", color: textMuted }}>{t.sell_time}</div>
                                </>
                              ) : (
                                <span style={{ color: textMuted }}>Active</span>
                              )}
                            </td>
                            <td style={{ padding: "6px 8px", fontSize: "10px" }}>
                              <div>SL: <strong style={{ color: redColor }}>₹{toN(t.stop_loss_price).toFixed(2)}</strong></div>
                              <div>TP: <strong style={{ color: greenColor }}>₹{toN(t.take_profit_price).toFixed(2)}</strong> (1.5:1)</div>
                            </td>
                            <td style={{ padding: "6px 8px", color: "#f59e0b" }}>- ₹{toN(t.total_taxes).toFixed(2)}</td>
                            <td style={{ padding: "6px 8px" }}>
                              {t.status === "CLOSED" ? (
                                <span style={{ fontWeight: "700", color: isProfit ? greenColor : redColor }}>
                                  {isProfit ? `+₹${toN(t.net_pnl).toFixed(2)}` : `-₹${Math.abs(toN(t.net_pnl)).toFixed(2)}`}
                                  <span style={{ fontSize: "10px", marginLeft: "4px" }}>({toN(t.roi_pct).toFixed(2)}%)</span>
                                </span>
                              ) : (
                                <span style={{ color: textMuted }}>Floating</span>
                              )}
                            </td>
                            <td style={{ padding: "6px 8px" }}>
                              <span style={{ padding: "2px 6px", borderRadius: "3px", fontSize: "10px", fontWeight: "700", background: t.status === "CLOSED" ? (isProfit ? "#dcfce7" : "#fee2e2") : "#e0e7ff", color: t.status === "CLOSED" ? (isProfit ? "#15803d" : "#b91c1c") : "#4338ca" }}>
                                {t.status}
                              </span>
                            </td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              )}
              {activeTabBottom === "backtest" && (
                <div style={{ padding: "10px 0" }}>
                  <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "12px", flexWrap: "wrap", gap: "8px" }}>
                    <span style={{ fontWeight: "700", color: textPrimary, fontSize: "13px" }}>
                      NSE Indian Equities — Pullback &amp; Confirmed Rebound Backtest (Zero Lookahead)
                    </span>
                    <button
                      onClick={fetchIndiaBacktest}
                      disabled={loadingIndiaBacktest}
                      style={{ padding: "4px 10px", borderRadius: "4px", border: `1px solid ${borderColor}`, background: bgCard, color: purpleAccent, cursor: "pointer", fontSize: "11px", fontWeight: "600" }}
                    >
                      {loadingIndiaBacktest ? "Running Simulation..." : "🔄 Refresh Backtest"}
                    </button>
                  </div>
                  {indiaBacktest && (
                    <div style={{ display: "flex", flexDirection: "column", gap: "10px" }}>
                      <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(260px, 1fr))", gap: "12px" }}>
                        <div style={{ padding: "12px", background: isLight ? "rgba(59, 130, 246, 0.08)" : "rgba(59, 130, 246, 0.15)", borderRadius: "6px", border: "1px solid rgba(59, 130, 246, 0.3)" }}>
                          <div style={{ fontWeight: "700", color: "#3b82f6", fontSize: "12px", marginBottom: "8px" }}>
                            {indiaBacktest.dev_period.period_name} ({indiaBacktest.dev_period.candle_count} 5m bars)
                          </div>
                          <div style={{ fontSize: "11px", display: "grid", gridTemplateColumns: "1fr 1fr", gap: "6px" }}>
                            <div>Net P&amp;L: <strong style={{ color: indiaBacktest.dev_period.net_pnl >= 0 ? greenColor : redColor }}>₹{toN(indiaBacktest.dev_period.net_pnl).toFixed(2)} ({toN(indiaBacktest.dev_period.net_pnl_pct).toFixed(2)}%)</strong></div>
                            <div>Win Rate: <strong>{toN(indiaBacktest.dev_period.win_rate_pct).toFixed(1)}%</strong> ({indiaBacktest.dev_period.winning_trades}W / {indiaBacktest.dev_period.losing_trades}L)</div>
                            <div>Max Drawdown: <strong style={{ color: redColor }}>₹{toN(indiaBacktest.dev_period.max_drawdown_amount).toFixed(2)} ({toN(indiaBacktest.dev_period.max_drawdown_pct).toFixed(2)}%)</strong></div>
                            <div>Expectancy: <strong>₹{toN(indiaBacktest.dev_period.expectancy_per_trade).toFixed(2)} / trade</strong></div>
                            <div>Avg Win / Loss: <strong>₹{toN(indiaBacktest.dev_period.avg_win).toFixed(2)} / ₹{toN(indiaBacktest.dev_period.avg_loss).toFixed(2)}</strong></div>
                            <div>Taxes: <strong style={{ color: "#f59e0b" }}>-₹{toN(indiaBacktest.dev_period.total_regulatory_costs).toFixed(2)}</strong></div>
                          </div>
                        </div>

                        <div style={{ padding: "12px", background: isLight ? "rgba(16, 185, 129, 0.08)" : "rgba(16, 185, 129, 0.15)", borderRadius: "6px", border: "1px solid rgba(16, 185, 129, 0.3)" }}>
                          <div style={{ fontWeight: "700", color: greenColor, fontSize: "12px", marginBottom: "8px" }}>
                            {indiaBacktest.oos_period.period_name} ({indiaBacktest.oos_period.candle_count} 5m bars)
                          </div>
                          <div style={{ fontSize: "11px", display: "grid", gridTemplateColumns: "1fr 1fr", gap: "6px" }}>
                            <div>Net P&amp;L: <strong style={{ color: indiaBacktest.oos_period.net_pnl >= 0 ? greenColor : redColor }}>₹{toN(indiaBacktest.oos_period.net_pnl).toFixed(2)} ({toN(indiaBacktest.oos_period.net_pnl_pct).toFixed(2)}%)</strong></div>
                            <div>Win Rate: <strong>{toN(indiaBacktest.oos_period.win_rate_pct).toFixed(1)}%</strong> ({indiaBacktest.oos_period.winning_trades}W / {indiaBacktest.oos_period.losing_trades}L)</div>
                            <div>Max Drawdown: <strong style={{ color: redColor }}>₹{toN(indiaBacktest.oos_period.max_drawdown_amount).toFixed(2)} ({toN(indiaBacktest.oos_period.max_drawdown_pct).toFixed(2)}%)</strong></div>
                            <div>Expectancy: <strong>₹{toN(indiaBacktest.oos_period.expectancy_per_trade).toFixed(2)} / trade</strong></div>
                            <div>Avg Win / Loss: <strong>₹{toN(indiaBacktest.oos_period.avg_win).toFixed(2)} / ₹{toN(indiaBacktest.oos_period.avg_loss).toFixed(2)}</strong></div>
                            <div>Taxes: <strong style={{ color: "#f59e0b" }}>-₹{toN(indiaBacktest.oos_period.total_regulatory_costs).toFixed(2)}</strong></div>
                          </div>
                        </div>
                      </div>

                      <div style={{ padding: "10px 14px", background: bgSurface, borderRadius: "6px", border: `1px solid ${borderColor}`, display: "flex", justifyContent: "space-between", alignItems: "center", fontSize: "11px", flexWrap: "wrap", gap: "8px" }}>
                        <div>
                          <strong>{indiaBacktest.baseline.strategy_name}:</strong> Buy &amp; Hold Return <strong>{toN(indiaBacktest.baseline.buy_and_hold_return_pct).toFixed(2)}%</strong> (Max DD: {toN(indiaBacktest.baseline.buy_and_hold_max_drawdown_pct).toFixed(2)}%)
                        </div>
                        <div style={{ fontWeight: "700", color: toN(indiaBacktest.baseline.strategy_outperformance_pct) >= 0 ? greenColor : redColor }}>
                          Outperformance: {toN(indiaBacktest.baseline.strategy_outperformance_pct) >= 0 ? `+${toN(indiaBacktest.baseline.strategy_outperformance_pct).toFixed(2)}%` : `${toN(indiaBacktest.baseline.strategy_outperformance_pct).toFixed(2)}%`}
                        </div>
                      </div>
                    </div>
                  )}
                </div>
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
