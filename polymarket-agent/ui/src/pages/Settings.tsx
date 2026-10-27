import { useState, useEffect, useSyncExternalStore } from "react";
import { Card } from "../components/ui/Card";
import { Badge, agentStateTone } from "../components/ui/Badge";
import { auth } from "../api/auth";
import { get, postJson, failureText } from "../api/client";
import { asObject } from "../api/narrow";
import { DATASET_PATHS, resume, halt } from "../api/endpoints";
import { useDataset } from "../data/useDataset";
import { dataStore } from "../data/store";
import type { Health } from "../api/types";
import {
  INTERVAL_OPTIONS,
  applyTheme,
  settings,
  type IntervalSeconds,
  type ThemeChoice,
} from "../data/settings";
import { storageAvailable } from "../lib/storage";
import { money } from "../lib/format";

interface CapsConfig {
  india_capital_allocation_inr: number;
  us_capital_allocation_usd: number;
  default_india_capital_inr: number;
  default_us_capital_usd: number;
  currency_inr: string;
  currency_usd: string;
}

export function Settings() {
  const cfg = useSyncExternalStore(settings.subscribe, settings.getSnapshot, settings.getSnapshot);
  const authSnap = useSyncExternalStore(auth.subscribe, auth.getSnapshot, auth.getSnapshot);
  const health = useDataset<Health>("health");

  const [draft, setDraft] = useState("");
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<string | null>(null);
  const [budgetDraft, setBudgetDraft] = useState(
    cfg.localDailyBudget === null ? "" : String(cfg.localDailyBudget),
  );

  // Agent lifecycle state & start/pause control
  const stateRaw = health.data?.agent_state;
  const isHalted = health.data ? (health.data.halted === true || !!health.data.halt) : true;
  const agentState = isHalted ? "HALTED" : (stateRaw === "HALTED" || !stateRaw ? "ALIVE" : stateRaw);
  const [togglingAgent, setTogglingAgent] = useState(false);

  async function handleToggleAgent() {
    setTogglingAgent(true);
    try {
      const res = isHalted ? await resume() : await halt();
      if (!res.ok) {
        console.error("Failed to toggle agent state:", res.error);
      }
      await dataStore.refresh();
      setTimeout(() => void dataStore.refresh(), 250);
    } catch (e) {
      console.error("Failed to toggle agent state:", e);
    } finally {
      setTogglingAgent(false);
    }
  }

  // Strategy Configuration state
  const [strategyParams, setStrategyParams] = useState({
    pullbackDipPct: "0.80",
    fastEma: "20",
    slowEma: "50",
    rewardRiskRatio: "1.50",
    stopLossBufferPct: "0.30",
    maxHoldHours: "24",
  });
  const [strategySaved, setStrategySaved] = useState(false);

  useEffect(() => {
    try {
      const saved = localStorage.getItem("polymarket_pullback_config");
      if (saved) {
        setStrategyParams(JSON.parse(saved));
      }
    } catch {
      // ignore
    }
  }, []);

  function handleSaveStrategy() {
    try {
      localStorage.setItem("polymarket_pullback_config", JSON.stringify(strategyParams));
      setStrategySaved(true);
      setTimeout(() => setStrategySaved(false), 3000);
    } catch {
      // ignore
    }
  }

  function handleResetStrategy() {
    const defaults = {
      pullbackDipPct: "0.80",
      fastEma: "20",
      slowEma: "50",
      rewardRiskRatio: "1.50",
      stopLossBufferPct: "0.30",
      maxHoldHours: "24",
    };
    setStrategyParams(defaults);
    try {
      localStorage.setItem("polymarket_pullback_config", JSON.stringify(defaults));
      setStrategySaved(true);
      setTimeout(() => setStrategySaved(false), 3000);
    } catch {
      // ignore
    }
  }

  // Capital Allocation Caps state (exclusive to Settings)
  const [caps, setCaps] = useState<CapsConfig | null>(null);
  const [indiaCapDraft, setIndiaCapDraft] = useState("");
  const [usCapDraft, setUsCapDraft] = useState("");
  const [savingCaps, setSavingCaps] = useState(false);
  const [capsMessage, setCapsMessage] = useState<{ text: string; tone: "good" | "critical" } | null>(null);

  useEffect(() => {
    let active = true;
    async function fetchCaps() {
      try {
        const token = auth.getToken();
        const res = await fetch("/api/settings/caps", {
          headers: token ? { Authorization: `Bearer ${token}` } : undefined,
        });
        if (res.ok && active) {
          const json: CapsConfig = await res.json();
          setCaps(json);
          setIndiaCapDraft(String(json.india_capital_allocation_inr));
          setUsCapDraft(String(json.us_capital_allocation_usd));
        }
      } catch {
        // silent fallback
      }
    }
    fetchCaps();
    return () => {
      active = false;
    };
  }, []);

  async function handleSaveCaps() {
    const inr = parseFloat(indiaCapDraft);
    const usd = parseFloat(usCapDraft);
    if (isNaN(inr) || inr <= 0) {
      setCapsMessage({ text: "NSE India capital cap must be a positive number.", tone: "critical" });
      return;
    }
    if (isNaN(usd) || usd <= 0) {
      setCapsMessage({ text: "US Equities capital cap must be a positive number.", tone: "critical" });
      return;
    }
    setSavingCaps(true);
    setCapsMessage(null);
    const res = await postJson<{
      status: string;
      message: string;
      india_capital_allocation_inr: number;
      us_capital_allocation_usd: number;
    }>("/api/settings/caps", {
      india_capital_allocation_inr: inr,
      us_capital_allocation_usd: usd,
    });
    setSavingCaps(false);
    if (res.ok) {
      setCaps((prev) =>
        prev
          ? {
              ...prev,
              india_capital_allocation_inr: res.value.india_capital_allocation_inr,
              us_capital_allocation_usd: res.value.us_capital_allocation_usd,
            }
          : null,
      );
      setCapsMessage({
        text: `Capital allocation caps updated: NSE ₹${Number(res.value.india_capital_allocation_inr).toLocaleString("en-IN")} | US $${res.value.us_capital_allocation_usd}`,
        tone: "good",
      });
      window.dispatchEvent(new CustomEvent("caps-updated", { detail: res.value }));
    } else {
      setCapsMessage({ text: failureText(res.error), tone: "critical" });
    }
  }

  async function handleResetCaps() {
    setSavingCaps(true);
    setCapsMessage(null);
    const defaultInr = caps?.default_india_capital_inr ?? 1000;
    const defaultUsd = caps?.default_us_capital_usd ?? 5;
    const res = await postJson<{
      status: string;
      message: string;
      india_capital_allocation_inr: number;
      us_capital_allocation_usd: number;
    }>("/api/settings/caps", {
      india_capital_allocation_inr: defaultInr,
      us_capital_allocation_usd: defaultUsd,
    });
    setSavingCaps(false);
    if (res.ok) {
      setIndiaCapDraft(String(defaultInr));
      setUsCapDraft(String(defaultUsd));
      setCaps((prev) =>
        prev
          ? {
              ...prev,
              india_capital_allocation_inr: defaultInr,
              us_capital_allocation_usd: defaultUsd,
            }
          : null,
      );
      setCapsMessage({
        text: `Reset to default caps: NSE India ₹${Number(defaultInr).toLocaleString("en-IN")} | US Equities $${defaultUsd}`,
        tone: "good",
      });
      window.dispatchEvent(new CustomEvent("caps-updated", { detail: res.value }));
    } else {
      setCapsMessage({ text: failureText(res.error), tone: "critical" });
    }
  }

  const canPersist = storageAvailable();

  async function testToken() {
    setTesting(true);
    setTestResult(null);
    const res = await get({ path: DATASET_PATHS.metrics, narrow: asObject });
    setTesting(false);
    if (res.ok) {
      setTestResult(
        auth.getSnapshot().hasToken
          ? "Success — the token is accepted."
          : "Success — this agent does not require a token.",
      );
    } else {
      setTestResult(failureText(res.error));
    }
  }

  return (
    <div className="page">
      {!canPersist && (
        <div className="connbar connbar--warning">
          Browser storage is unavailable (private browsing, most likely), so
          nothing on this page will be remembered after a reload.
        </div>
      )}

      {/* 1. Agent Lifecycle & Master Start/Pause Control */}
      <Card title="Agent Operating Status & Control">
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", flexWrap: "wrap", gap: "16px" }}>
          <div>
            <div style={{ display: "flex", alignItems: "center", gap: "10px", marginBottom: "8px" }}>
              <span style={{ fontSize: "14px", fontWeight: 600, color: "var(--text-primary)" }}>Current Agent State:</span>
              <Badge tone={agentStateTone(agentState)}>{agentState}</Badge>
              <Badge tone="good">Paper Funds Only (Live APIs Disabled)</Badge>
            </div>
            <p className="muted" style={{ margin: 0, fontSize: "12px" }}>
              The agent operates strictly in paper-trading simulation mode. If in Standby or Halted state, press Start Agent to begin evaluation and market execution cycles.
            </p>
          </div>
          <div>
            <button
              className="btn"
              onClick={handleToggleAgent}
              disabled={togglingAgent}
              style={{
                backgroundColor: isHalted ? "#16a34a" : "#dc2626",
                color: "#ffffff",
                border: isHalted ? "1px solid #22c55e" : "1px solid #ef4444",
                fontWeight: 700,
                fontSize: "13px",
                padding: "8px 18px",
                borderRadius: "6px",
                display: "inline-flex",
                alignItems: "center",
                gap: "8px",
                boxShadow: isHalted
                  ? "0 0 12px rgba(34, 197, 94, 0.4)"
                  : "0 0 12px rgba(220, 38, 38, 0.4)",
                cursor: "pointer",
              }}
            >
              {togglingAgent ? "Updating…" : isHalted ? "▶ Start Agent" : "⏹ Pause Agent"}
            </button>
          </div>
        </div>
      </Card>

      {/* 2. Pullback & Confirmed Rebound Strategy Configuration */}
      <Card title="Pullback Strategy Configuration">
        <p className="muted" style={{ marginTop: 0 }}>
          Tune 5m candle pullback parameters, trend filters, and execution rules. Strategy initiates paper buys only upon confirmed rebound above VWAP and exits at the planned target ratio.
        </p>
        <div className="filters" style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))", gap: "14px", alignItems: "flex-end" }}>
          <label className="field">
            <span>Pullback Dip Depth (%)</span>
            <input
              className="input"
              type="number"
              step="0.1"
              min="0.1"
              value={strategyParams.pullbackDipPct}
              onChange={(e) => setStrategyParams({ ...strategyParams, pullbackDipPct: e.target.value })}
            />
          </label>
          <label className="field">
            <span>Fast EMA Period</span>
            <input
              className="input"
              type="number"
              min="5"
              max="50"
              value={strategyParams.fastEma}
              onChange={(e) => setStrategyParams({ ...strategyParams, fastEma: e.target.value })}
            />
          </label>
          <label className="field">
            <span>Slow EMA Period</span>
            <input
              className="input"
              type="number"
              min="20"
              max="200"
              value={strategyParams.slowEma}
              onChange={(e) => setStrategyParams({ ...strategyParams, slowEma: e.target.value })}
            />
          </label>
          <label className="field">
            <span>Reward-to-Risk Target Ratio</span>
            <input
              className="input"
              type="number"
              step="0.1"
              min="1.0"
              value={strategyParams.rewardRiskRatio}
              onChange={(e) => setStrategyParams({ ...strategyParams, rewardRiskRatio: e.target.value })}
            />
          </label>
          <label className="field">
            <span>Stop Buffer Below Low (%)</span>
            <input
              className="input"
              type="number"
              step="0.05"
              min="0.1"
              value={strategyParams.stopLossBufferPct}
              onChange={(e) => setStrategyParams({ ...strategyParams, stopLossBufferPct: e.target.value })}
            />
          </label>
          <label className="field">
            <span>Max Position Hold (Hours)</span>
            <input
              className="input"
              type="number"
              min="1"
              max="72"
              value={strategyParams.maxHoldHours}
              onChange={(e) => setStrategyParams({ ...strategyParams, maxHoldHours: e.target.value })}
            />
          </label>
        </div>
        <div style={{ display: "flex", gap: "10px", marginTop: "16px", alignItems: "center" }}>
          <button className="btn btn--primary" onClick={handleSaveStrategy}>
            Save Strategy Config
          </button>
          <button className="btn" onClick={handleResetStrategy}>
            Reset Strategy Defaults
          </button>
          {strategySaved && (
            <span style={{ color: "var(--status-good, #22c55e)", fontSize: "12px", fontWeight: 600 }}>
              ✓ Strategy configuration saved successfully.
            </span>
          )}
        </div>
      </Card>

      {/* 3. Market Feeds & Data Providers */}
      <Card title="Market Feeds & Session Gating Config">
        <p className="muted" style={{ marginTop: 0 }}>
          Real-time WebSocket market data providers for India and US equities. Execution decisions are strictly paused outside official exchange hours.
        </p>
        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "16px", flexWrap: "wrap" }}>
          <div style={{ background: "rgba(255,255,255,0.03)", padding: "12px 16px", borderRadius: "8px", border: "1px solid var(--border-color)" }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "6px" }}>
              <strong>🇮🇳 India Market Feed</strong>
              <Badge tone="neutral">Upstox V3 Feed</Badge>
            </div>
            <p className="muted" style={{ fontSize: "12px", margin: "0 0 6px" }}>
              Stream: Market Data Feed V3 Protobuf WebSocket. Instruments: NIFTY 50 cash equities.
            </p>
            <div style={{ fontSize: "11px", color: "var(--text-secondary)" }}>
              Session: Regular Trading Hours 09:15–15:30 IST. Outside hours: Strategy automatically paused.
            </div>
          </div>
          <div style={{ background: "rgba(255,255,255,0.03)", padding: "12px 16px", borderRadius: "8px", border: "1px solid var(--border-color)" }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "6px" }}>
              <strong>🇺🇸 US Market Feed</strong>
              <Badge tone="neutral">Alpaca IEX Feed</Badge>
            </div>
            <p className="muted" style={{ fontSize: "12px", margin: "0 0 6px" }}>
              Stream: Alpaca WebSocket V2. Disclaimer: IEX Top of Book (Not consolidated SIP).
            </p>
            <div style={{ fontSize: "11px", color: "var(--text-secondary)" }}>
              Session: Regular Trading Hours 09:30–16:00 ET. Daylight saving transitions tracked via <code>America/New_York</code>.
            </div>
          </div>
        </div>
      </Card>

      <Card title="Dashboard token">
        <p className="muted" style={{ marginTop: 0 }}>
          Needed only when the agent was started with <code>DASHBOARD_TOKEN</code>{" "}
          set. Stored in this browser and sent as a bearer header.
        </p>
        <div className="filters">
          <label className="field" style={{ flex: 1, minWidth: 220 }}>
            <span>
              Token{" "}
              {authSnap.hasToken && (
                <Badge tone="good">saved · …{authSnap.hint}</Badge>
              )}
            </span>
            <input
              className="input"
              type="password"
              autoComplete="off"
              placeholder={authSnap.hasToken ? "•••• (unchanged)" : "not set"}
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
            />
          </label>
          <button
            className="btn btn--primary"
            disabled={draft.trim() === ""}
            onClick={() => {
              auth.setToken(draft);
              setDraft("");
              setTestResult(null);
            }}
          >
            Save
          </button>
          <button
            className="btn"
            disabled={!authSnap.hasToken}
            onClick={() => {
              auth.clearToken();
              setTestResult(null);
            }}
          >
            Clear
          </button>
          <button className="btn" onClick={testToken} disabled={testing}>
            {testing ? "Testing…" : "Test"}
          </button>
        </div>
        {testResult && (
          <div className="state__msg" style={{ marginTop: 10 }}>{testResult}</div>
        )}
      </Card>

      <Card title="Refresh">
        <div className="filters">
          <label className="field">
            <span>Automatic refresh</span>
            <select
              className="select"
              value={cfg.intervalSeconds}
              onChange={(e) =>
                settings.update({
                  intervalSeconds: Number(e.target.value) as IntervalSeconds,
                })
              }
            >
              {INTERVAL_OPTIONS.map((s) => (
                <option key={s} value={s}>
                  {s === 0 ? "Off" : s < 60 ? `Every ${s}s` : `Every ${s / 60}m`}
                </option>
              ))}
            </select>
          </label>
        </div>
        <p className="muted" style={{ marginBottom: 0, fontSize: 12 }}>
          Polling pauses automatically while this tab is in the background, and
          resumes when you return. History endpoints are fetched less often than
          the live ones, since they are unbounded and change once per cycle.
        </p>
      </Card>

      <Card title="Display">
        <div className="filters">
          <label className="field">
            <span>Theme</span>
            <select
              className="select"
              value={cfg.theme}
              onChange={(e) => {
                const theme = e.target.value as ThemeChoice;
                settings.update({ theme });
                applyTheme(theme);
              }}
            >
              <option value="system">Match system</option>
              <option value="light">Light</option>
              <option value="dark">Dark</option>
            </select>
          </label>
          <label className="field">
            <span>Price precision</span>
            <select
              className="select"
              value={cfg.pricePrecision}
              onChange={(e) =>
                settings.update({ pricePrecision: Number(e.target.value) as 2 | 4 })
              }
            >
              <option value={4}>4 decimals</option>
              <option value={2}>2 decimals</option>
            </select>
          </label>
        </div>
        <p className="muted" style={{ marginBottom: 0, fontSize: 12 }}>
          Prediction-market contracts trade in the third and fourth decimal, so
          rounding prices to cents loses real differences between trades.
        </p>
      </Card>

      <Card title="Capital Allocation Caps">
        <p className="muted" style={{ marginTop: 0 }}>
          Configure maximum capital allocation caps for paper trading venues. Changes dynamically update sandbox buying power, exposure checks, and the venue tags in the top navigation ribbon.
        </p>
        <div className="filters" style={{ alignItems: "flex-end", gap: "16px" }}>
          <label className="field" style={{ minWidth: 220, flex: 1 }}>
            <span>
              NSE India Cap (INR){" "}
              {caps && (
                <Badge tone="neutral">
                  Current: ₹{Number(caps.india_capital_allocation_inr).toLocaleString("en-IN")}
                </Badge>
              )}
            </span>
            <div style={{ position: "relative", display: "flex", alignItems: "center" }}>
              <span
                style={{
                  position: "absolute",
                  left: 10,
                  color: "var(--text-secondary)",
                  fontWeight: 600,
                  pointerEvents: "none",
                }}
              >
                ₹
              </span>
              <input
                className="input"
                style={{ paddingLeft: 24, width: "100%" }}
                type="number"
                min="1"
                step="100"
                placeholder="1000"
                value={indiaCapDraft}
                onChange={(e) => setIndiaCapDraft(e.target.value)}
              />
            </div>
          </label>

          <label className="field" style={{ minWidth: 220, flex: 1 }}>
            <span>
              US Equities Cap (USD){" "}
              {caps && (
                <Badge tone="neutral">
                  Current: ${Number(caps.us_capital_allocation_usd).toFixed(2)}
                </Badge>
              )}
            </span>
            <div style={{ position: "relative", display: "flex", alignItems: "center" }}>
              <span
                style={{
                  position: "absolute",
                  left: 10,
                  color: "var(--text-secondary)",
                  fontWeight: 600,
                  pointerEvents: "none",
                }}
              >
                $
              </span>
              <input
                className="input"
                style={{ paddingLeft: 24, width: "100%" }}
                type="number"
                min="0.1"
                step="1"
                placeholder="5.00"
                value={usCapDraft}
                onChange={(e) => setUsCapDraft(e.target.value)}
              />
            </div>
          </label>

          <div style={{ display: "flex", gap: "8px" }}>
            <button
              className="btn btn--primary"
              disabled={savingCaps || !indiaCapDraft.trim() || !usCapDraft.trim()}
              onClick={handleSaveCaps}
            >
              {savingCaps ? "Saving…" : "Save Caps"}
            </button>

            <button
              className="btn"
              disabled={savingCaps}
              onClick={handleResetCaps}
              title="Reset caps to defaults (₹1,000 INR & $5.00 USD)"
            >
              Reset to Defaults
            </button>
          </div>
        </div>

        {capsMessage && (
          <div
            className="state__msg"
            style={{
              marginTop: 12,
              padding: "8px 12px",
              borderRadius: "var(--radius-sm)",
              fontSize: "13px",
              backgroundColor:
                capsMessage.tone === "good"
                  ? "rgba(34, 197, 94, 0.12)"
                  : "rgba(239, 68, 68, 0.12)",
              border: `1px solid ${
                capsMessage.tone === "good" ? "var(--status-good)" : "var(--status-critical)"
              }`,
              color:
                capsMessage.tone === "good" ? "var(--status-good)" : "var(--status-critical)",
              display: "flex",
              alignItems: "center",
              gap: 8,
            }}
          >
            <span>{capsMessage.tone === "good" ? "✓" : "⚠"}</span>
            <span>{capsMessage.text}</span>
          </div>
        )}
      </Card>

      <Card title="Local daily budget">
        <p className="muted" style={{ marginTop: 0 }}>
          The agent's own <code>daily_api_budget</code> is not exposed over the
          API. This is a display aid for the Costs page and changes nothing about
          how the agent behaves.
        </p>
        <div className="filters">
          <label className="field">
            <span>Budget (USD)</span>
            <input
              className="input"
              inputMode="decimal"
              placeholder="none"
              value={budgetDraft}
              onChange={(e) => setBudgetDraft(e.target.value)}
            />
          </label>
          <button
            className="btn"
            onClick={() => {
              const v = budgetDraft.trim();
              const n = v === "" ? null : Number(v);
              settings.update({
                localDailyBudget: n === null || !Number.isFinite(n) ? null : n,
              });
            }}
          >
            Save
          </button>
          {cfg.localDailyBudget !== null && (
            <span className="muted">Currently {money(cfg.localDailyBudget)}</span>
          )}
        </div>
      </Card>

      <Card title="Reset">
        <button
          className="btn"
          onClick={() => {
            settings.reset();
            applyTheme("system");
            setBudgetDraft("");
          }}
        >
          Clear all local settings
        </button>
        <p className="muted" style={{ marginBottom: 0, fontSize: 12, marginTop: 8 }}>
          Does not clear the saved token — use Clear above for that.
        </p>
      </Card>
    </div>
  );
}
