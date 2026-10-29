import { useEffect, useState } from "react";
import { Badge } from "../ui/Badge";

interface IntradayStatusData {
  india?: {
    market: string;
    session_state: "OPEN" | "CLOSED";
    currency_symbol: string;
    capital_allocation_inr: string | number;
    local_time: string;
    entry_permitted: boolean;
    exit_due: boolean;
  };
  us?: {
    market: string;
    session_state: "OPEN" | "CLOSED";
    currency_symbol: string;
    capital_allocation_usd: string | number;
    local_time: string;
    entry_permitted: boolean;
    exit_due: boolean;
  };
  fx_rate?: {
    rate: string;
    base: string;
    quote: string;
  };
}

export function MarketSessionRibbon() {
  const [data, setData] = useState<IntradayStatusData | null>(null);
  const [unavailable, setUnavailable] = useState(false);

  useEffect(() => {
    let active = true;
    async function load() {
      try {
        const res = await fetch("/api/intraday/status");
        if (!res.ok) throw new Error(`Market status request failed (${res.status})`);
        const json: IntradayStatusData = await res.json();
        if (active) {
          setData(json);
          setUnavailable(false);
        }
      } catch {
        if (active) setUnavailable(true);
      }
    }
    load();
    const interval = setInterval(load, 15000);
    const onCapsUpdated = () => {
      load();
    };
    window.addEventListener("caps-updated", onCapsUpdated);
    return () => {
      active = false;
      clearInterval(interval);
      window.removeEventListener("caps-updated", onCapsUpdated);
    };
  }, []);

  if (!data) {
    return unavailable ? (
      <div className="connbar connbar--warning" role="status">
        Market session details are temporarily unavailable.
      </div>
    ) : null;
  }

  const indiaOpen = data.india?.session_state === "OPEN";
  const usOpen = data.us?.session_state === "OPEN";
  const fx = Number(data.fx_rate?.rate);

  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "space-between",
        flexWrap: "wrap",
        gap: "12px",
        padding: "6px 14px",
        background: "var(--surface-1)",
        border: "1px solid var(--border)",
        borderRadius: "8px",
        fontSize: "12px",
        marginBottom: "12px",
      }}
    >
      {unavailable && (
        <Badge tone="warning" title="Showing the last successful session response">
          Session status may be stale
        </Badge>
      )}
      <div style={{ display: "flex", alignItems: "center", gap: "16px", flexWrap: "wrap" }}>
        {/* NSE Market Pill */}
        <div style={{ display: "flex", alignItems: "center", gap: "6px" }}>
          <span style={{ fontWeight: 600, color: "var(--text-primary)" }}>NSE India:</span>
          <Badge tone={indiaOpen ? "good" : "neutral"}>
            {indiaOpen ? "🟢 09:15–15:30 IST OPEN" : "⚪ CLOSED"}
          </Badge>
          {data.india?.capital_allocation_inr && (
            <span style={{ color: "var(--text-secondary)", fontSize: "11px" }}>
              (Cap: ₹{Number(data.india.capital_allocation_inr).toLocaleString("en-IN")})
            </span>
          )}
          {data.india?.exit_due && (
            <Badge tone="critical">Auto Square-Off Due</Badge>
          )}
        </div>

        {/* US Market Pill */}
        <div style={{ display: "flex", alignItems: "center", gap: "6px" }}>
          <span style={{ fontWeight: 600, color: "var(--text-primary)" }}>US Equities:</span>
          <Badge tone={usOpen ? "good" : "neutral"}>
            {usOpen ? "🟢 09:30–16:00 ET OPEN" : "⚪ CLOSED"}
          </Badge>
          {data.us?.capital_allocation_usd && (
            <span style={{ color: "var(--text-secondary)", fontSize: "11px" }}>
              (Cap: ${data.us.capital_allocation_usd})
            </span>
          )}
        </div>

        {/* Polymarket 24/7 Pill */}
        <div style={{ display: "flex", alignItems: "center", gap: "6px" }}>
          <span style={{ fontWeight: 600, color: "var(--text-primary)" }}>Polymarket:</span>
          <Badge tone="neutral">24/7 market</Badge>
        </div>
      </div>

      {/* FX Reference Benchmark */}
      {Number.isFinite(fx) && fx > 0 && (
        <div style={{ display: "flex", alignItems: "center", gap: "8px", color: "var(--text-secondary)" }}>
          <span>USD/INR:</span>
          <strong style={{ color: "var(--text-primary)" }}>₹{fx.toFixed(2)}</strong>
        </div>
      )}
    </div>
  );
}
