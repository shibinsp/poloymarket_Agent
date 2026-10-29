import { useEffect, useState } from "react";
import { halt, resume } from "../../api/endpoints";
import { postJson } from "../../api/client";
import { dataStore } from "../../data/store";
import { useDataset } from "../../data/useDataset";
import { Badge, agentStateTone } from "../ui/Badge";
import type { Health } from "../../api/types";

interface VenueStatusData {
  master_halted: boolean;
  india: {
    name: string;
    enabled: boolean;
    capital_inr: number;
    currency: string;
    status: string;
  };
  us: {
    name: string;
    enabled: boolean;
    capital_usd: number;
    currency: string;
    status: string;
  };
  polymarket: {
    name: string;
    enabled: boolean;
    capital_usd: number;
    currency: string;
    status: string;
  };
}

export function AgentActionButtons({ compact = false }: { compact?: boolean }) {
  const health = useDataset<Health>("health");
  const [busyVenue, setBusyVenue] = useState<string | null>(null);
  const [msg, setMsg] = useState<string | null>(null);

  const [venueStatus, setVenueStatus] = useState<VenueStatusData | null>(null);

  const isMasterHalted =
    (health.data?.halted ?? false) ||
    health.data?.agent_state === "HALTED" ||
    !!health.data?.halt;
  const masterState = isMasterHalted ? "HALTED" : (health.data?.agent_state ?? "ALIVE");

  const loadVenueStatus = async () => {
    try {
      const res = await fetch("/api/venue/status");
      if (res.ok) {
        const json = await res.json();
        setVenueStatus(json);
      }
    } catch {
      // fallback
    }
  };

  useEffect(() => {
    loadVenueStatus();
    const interval = setInterval(loadVenueStatus, 4000);
    return () => clearInterval(interval);
  }, []);

  async function toggleMaster() {
    setBusyVenue("master");
    setMsg(null);
    try {
      if (isMasterHalted) {
        const res = await resume();
        if (res.ok) {
          setMsg("Master Agent Started");
          void dataStore.refresh();
          void loadVenueStatus();
        } else {
          setMsg("Failed: " + (res.error?.kind || "error"));
        }
      } else {
        const res = await halt();
        if (res.ok) {
          setMsg("Master Agent Halted");
          void dataStore.refresh();
          void loadVenueStatus();
        } else {
          setMsg("Failed: " + (res.error?.kind || "error"));
        }
      }
    } catch {
      setMsg("Network error");
    } finally {
      setBusyVenue(null);
      setTimeout(() => setMsg(null), 3500);
    }
  }

  async function toggleVenue(venue: "india" | "us") {
    setBusyVenue(venue);
    setMsg(null);
    try {
      const isCurrentlyEnabled =
        venue === "india"
          ? (venueStatus?.india?.enabled ?? true)
          : (venueStatus?.us?.enabled ?? true);

      const nextEnabled = !isCurrentlyEnabled;

      const res = await postJson("/api/venue/toggle", {
        venue,
        enabled: nextEnabled,
      });

      if (res.ok) {
        setMsg(`${venue.toUpperCase()} ${nextEnabled ? "Started" : "Paused"}`);
        void dataStore.refresh();
        void loadVenueStatus();
      } else {
        setMsg("Failed: " + (res.error?.kind || "error"));
      }
    } catch {
      setMsg("Network error");
    } finally {
      setBusyVenue(null);
      setTimeout(() => setMsg(null), 3500);
    }
  }

  const indiaEnabled = venueStatus?.india?.enabled ?? true;
  const usEnabled = venueStatus?.us?.enabled ?? true;
  const indiaRunning = !isMasterHalted && indiaEnabled;
  const usRunning = !isMasterHalted && usEnabled;

  return (
    <div
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: compact ? "6px" : "10px",
        flexWrap: "wrap",
      }}
    >
      {/* 1. Indian NSE Start/Stop Button */}
      <div style={{ display: "inline-flex", alignItems: "center", gap: "5px" }}>
        <button
          type="button"
          disabled={busyVenue === "india"}
          onClick={() => toggleVenue("india")}
          className={`btn btn--sm ${indiaRunning ? "btn--danger" : "btn--primary"}`}
          title={indiaRunning ? "Pause Indian NSE automated trading" : "Start Indian NSE automated trading"}
          style={{
            fontWeight: 600,
            fontSize: "12px",
            padding: "4px 8px",
            display: "inline-flex",
            alignItems: "center",
            gap: "4px",
          }}
        >
          {busyVenue === "india" ? "…" : indiaRunning ? "⏹ Stop India" : "▶ Start India"}
        </button>
        <Badge tone={indiaRunning ? "good" : "warning"} title="Indian NSE Cash Equities Trading Status">
          {indiaRunning ? "🇮🇳 LIVE" : "🇮🇳 PAUSED"}
        </Badge>
      </div>

      {/* 2. US Alpaca Start/Stop Button */}
      <div style={{ display: "inline-flex", alignItems: "center", gap: "5px" }}>
        <button
          type="button"
          disabled={busyVenue === "us"}
          onClick={() => toggleVenue("us")}
          className={`btn btn--sm ${usRunning ? "btn--danger" : "btn--primary"}`}
          title={usRunning ? "Pause US Alpaca automated trading" : "Start US Alpaca automated trading"}
          style={{
            fontWeight: 600,
            fontSize: "12px",
            padding: "4px 8px",
            display: "inline-flex",
            alignItems: "center",
            gap: "4px",
          }}
        >
          {busyVenue === "us" ? "…" : usRunning ? "⏹ Stop US" : "▶ Start US"}
        </button>
        <Badge tone={usRunning ? "good" : "warning"} title="US Equities Alpaca Trading Status">
          {usRunning ? "🇺🇸 LIVE" : "🇺🇸 PAUSED"}
        </Badge>
      </div>

      {/* 3. Master / Polymarket Loop Control */}
      <div style={{ display: "inline-flex", alignItems: "center", gap: "5px" }}>
        <button
          type="button"
          disabled={busyVenue === "master"}
          onClick={toggleMaster}
          className={`btn btn--sm ${isMasterHalted ? "btn--primary" : "btn--quiet"}`}
          title={isMasterHalted ? "Start the master agent loop" : "Pause all automated trading"}
          style={{
            fontSize: "12px",
            padding: "4px 8px",
            display: "inline-flex",
            alignItems: "center",
            gap: "4px",
          }}
        >
          {busyVenue === "master" ? "…" : isMasterHalted ? "▶ Start All" : "⏹ Stop All"}
        </button>
        <Badge tone={isMasterHalted ? "critical" : agentStateTone(masterState)}>
          {isMasterHalted ? "🔴 HALTED" : "🟢 ACTIVE"}
        </Badge>
      </div>

      {msg && (
        <span
          style={{
            fontSize: "11px",
            fontWeight: 600,
            color: "var(--series-1)",
            marginLeft: "4px",
          }}
        >
          {msg}
        </span>
      )}
    </div>
  );
}
