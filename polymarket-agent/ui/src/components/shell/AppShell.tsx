import { useSyncExternalStore, type ReactNode } from "react";
import { Link } from "../../router/Link";
import { ROUTES } from "../../router/routes";
import { Freshness } from "../ui/Freshness";
import { ConnectionBar } from "./ConnectionBar";
import { MarketSessionRibbon } from "./MarketSessionRibbon";
import { AgentActionButtons } from "./AgentActionButtons";
import { useAllDatasets } from "../../data/useDataset";
import { dataStore } from "../../data/store";
import { useNow } from "../../data/useNow";
import { settings } from "../../data/settings";
import { Badge } from "../ui/Badge";
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
  const all = useAllDatasets();
  const cfg = useSyncExternalStore(settings.subscribe, settings.getSnapshot, settings.getSnapshot);

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
            <strong>MoneyAI</strong>
            <span>autonomous multi-venue trading operations</span>
          </div>
          <AgentActionButtons />
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
        <span>MoneyAI Trading Operations Dashboard · 60Hz Low-Latency Visualization Engine</span>
        <span className="muted">
          UI build {typeof __BUILD_ID__ === "string" ? __BUILD_ID__ : "dev"}
        </span>
      </footer>
    </div>
  );
}
