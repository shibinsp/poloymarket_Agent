import { useState, useSyncExternalStore, type ReactNode } from "react";
import { Link } from "../../router/Link";
import { ROUTES } from "../../router/routes";
import { Badge, agentStateTone } from "../ui/Badge";
import { Freshness } from "../ui/Freshness";
import { ConnectionBar } from "./ConnectionBar";
import { MarketSessionRibbon } from "./MarketSessionRibbon";
import { TokenDialog } from "./TokenDialog";
import { useAllDatasets, useDataset } from "../../data/useDataset";
import { dataStore } from "../../data/store";
import { useNow } from "../../data/useNow";
import { settings } from "../../data/settings";
import { resume, halt } from "../../api/endpoints";
import type { Health } from "../../api/types";
import type { DatasetKey } from "../../api/endpoints";

/** Build stamp, so "am I looking at the new bundle?" is answerable in-page. */
declare const __BUILD_ID__: string;

export function AppShell({
  activePath,
  children,
}: {
  activePath: string;
  children: ReactNode;
}) {
  const now = useNow();
  const health = useDataset<Health>("health");
  const all = useAllDatasets();
  const cfg = useSyncExternalStore(settings.subscribe, settings.getSnapshot, settings.getSnapshot);

  const [toggling, setToggling] = useState(false);

  const stateRaw = health.data?.agent_state;
  const isHalted = health.data ? (health.data.halted === true || !!health.data.halt) : true;
  const state = isHalted ? "HALTED" : (stateRaw === "HALTED" || !stateRaw ? "ALIVE" : stateRaw);

  async function handleToggleAgent() {
    setToggling(true);
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
      setToggling(false);
    }
  }

  // The header reports the *worst* freshness across everything loaded, so the
  // top of the page can never claim to be current while a card below it is
  // half an hour stale.
  const loaded = (Object.keys(all) as DatasetKey[]).filter(
    (k) => all[k].lastSuccessAt !== null,
  );
  const oldest = loaded.length
    ? Math.min(...loaded.map((k) => all[k].lastSuccessAt!))
    : null;
  const anyError = (Object.keys(all) as DatasetKey[]).some((k) => all[k].error);

  return (
    <div className="shell">
      <header className="topbar">
        <div className="topbar__brand">
          <strong>Polymarket Agent</strong>
          <Badge tone={agentStateTone(state)} title="Agent lifecycle state">
            {state}
          </Badge>
          <button
            className="btn btn--sm"
            onClick={handleToggleAgent}
            disabled={toggling}
            style={{
              marginLeft: "10px",
              display: "inline-flex",
              alignItems: "center",
              gap: "6px",
              fontWeight: 700,
              fontSize: "12px",
              borderRadius: "6px",
              padding: "4px 12px",
              cursor: "pointer",
              backgroundColor: isHalted ? "#16a34a" : "#dc2626",
              color: "#ffffff",
              border: isHalted ? "1px solid #22c55e" : "1px solid #ef4444",
              boxShadow: isHalted
                ? "0 0 10px rgba(34, 197, 94, 0.35)"
                : "0 0 10px rgba(239, 68, 68, 0.35)",
              transition: "all 0.15s ease",
            }}
            title={
              isHalted
                ? "Press to start the autonomous trading agent"
                : "Press to pause the agent and cancel resting orders"
            }
          >
            {toggling ? "…" : isHalted ? "▶ Start Agent" : "⏹ Pause Agent"}
          </button>
        </div>

        <nav className="nav" aria-label="Sections">
          {ROUTES.map((r) => (
            <Link key={r.path} to={r.path} active={r.path === activePath}>
              {r.label}
            </Link>
          ))}
        </nav>

        <div className="topbar__tools">
          <Freshness
            lastSuccessAt={oldest}
            now={now}
            error={anyError ? { kind: "network", message: "" } : null}
            paused={dataStore.isPaused()}
          />
          <button
            className="btn btn--sm"
            onClick={() => dataStore.refresh()}
            title="Refresh every subscribed dataset now"
          >
            Refresh
          </button>
          {cfg.intervalSeconds === 0 && (
            <Badge tone="warning" title="Nothing is being re-fetched">
              auto-refresh off
            </Badge>
          )}
        </div>
      </header>

      <main className="main">
        <ConnectionBar />
        <MarketSessionRibbon />
        {children}
      </main>

      <footer className="footer">
        <span>Polymarket autonomous trading agent</span>
        <span className="muted">
          UI build {typeof __BUILD_ID__ === "string" ? __BUILD_ID__ : "dev"}
        </span>
      </footer>

      <TokenDialog />
    </div>
  );
}
