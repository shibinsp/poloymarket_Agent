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
  const normalizedState = stateRaw?.toUpperCase();
  const isHalted =
    health.data?.halted === true ||
    health.data?.halt != null ||
    normalizedState === "HALTED";
  const stateKnown =
    isHalted ||
    normalizedState === "ALIVE" ||
    normalizedState === "LOWFUEL" ||
    normalizedState === "CRITICALSURVIVAL";
  const canToggle =
    health.data !== undefined &&
    !health.loading &&
    !health.error &&
    stateKnown;
  const state = !health.data || health.loading || health.error
    ? "CONNECTING"
    : isHalted
      ? "HALTED"
      : stateRaw || "UNKNOWN";

  async function handleToggleAgent() {
    if (!canToggle) return;
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
          <div className="topbar__brand-mark" aria-hidden="true">P</div>
          <div className="topbar__brand-copy">
            <strong>Polymarket Agent</strong>
            <span>trading operations dashboard</span>
          </div>
          <Badge tone={agentStateTone(state)} title="Agent lifecycle state">
            {state}
          </Badge>
          <button
            className={`btn btn--sm ${!canToggle ? "" : isHalted ? "btn--success" : "btn--danger"}`}
            onClick={handleToggleAgent}
            disabled={toggling || !canToggle}
            title={
              !canToggle
                ? "Agent state is unavailable; wait for a successful health check before changing it"
                : isHalted
                ? "Press to start the autonomous trading agent"
                : "Press to pause the agent and cancel resting orders"
            }
          >
            {toggling
              ? "Working…"
              : !canToggle
                ? "Status unavailable"
                : isHalted
                  ? "Start agent"
                  : "Pause agent"}
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
        <span>Trading operations dashboard</span>
        <span className="muted">
          UI build {typeof __BUILD_ID__ === "string" ? __BUILD_ID__ : "dev"}
        </span>
      </footer>

      <TokenDialog />
    </div>
  );
}
