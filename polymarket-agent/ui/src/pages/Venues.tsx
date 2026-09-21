import { useMemo } from "react";
import { useDataset } from "../data/useDataset";
import { DatasetCard } from "../components/ui/DatasetCard";
import { Card } from "../components/ui/Card";
import { StatTile } from "../components/ui/StatTile";
import { EmptyState } from "../components/ui/States";
import { Badge, type StatusTone } from "../components/ui/Badge";
import { DataTable, type Column } from "../components/ui/DataTable";
import { num } from "../lib/decimal";
import { int, pctFromFraction, DASH } from "../lib/format";
import type { VenueStatus } from "../api/types";

/**
 * Which platforms this agent trades, and whether each one is actually working.
 *
 * No other screen could answer that. Orders and fills carry a venue column, so
 * a working venue is visible *once it has traded* — but a venue that was
 * configured and then skipped appeared nowhere at all, and the only record of
 * why was a single line in the startup log. An operator who enabled Coinbase
 * in paper mode saw it silently do nothing.
 */
export function Venues() {
  const venues = useDataset<VenueStatus[]>("venues");
  const rows = venues.data ?? [];

  const counts = useMemo(() => {
    const active = rows.filter((v) => v.active).length;
    const wanted = rows.filter((v) => v.enabled).length;
    // The interesting number: asked for and not running.
    return { active, wanted, blocked: wanted - active, total: rows.length };
  }, [rows]);

  const columns: Column<VenueStatus>[] = [
    {
      key: "status",
      header: "Status",
      render: (v) => <Badge tone={tone(v)}>{label(v)}</Badge>,
      sortValue: (v) => (v.active ? 0 : v.enabled ? 1 : 2),
    },
    {
      key: "id",
      header: "Venue",
      render: (v) => v.id,
      sortValue: (v) => v.id,
    },
    {
      key: "kind",
      header: "Platform",
      render: (v) => PLATFORMS[v.kind.toLowerCase()]?.name ?? v.kind,
      sortValue: (v) => v.kind,
    },
    {
      key: "assets",
      header: "Assets",
      render: (v) =>
        v.asset_classes.length > 0
          ? v.asset_classes.map(assetLabel).join(", ")
          : PLATFORMS[v.kind.toLowerCase()]?.assets ?? DASH,
    },
    {
      key: "symbols",
      header: "Symbols",
      truncate: true,
      render: (v) => (v.symbols.length > 0 ? v.symbols.join(", ") : DASH),
      title: (v) => v.symbols.join(", "),
      sortValue: (v) => v.symbols.length,
    },
    {
      key: "fee",
      header: "Fee",
      numeric: true,
      render: (v) => pctFromFraction(num(v.fee_pct)),
      sortValue: (v) => num(v.fee_pct) ?? null,
    },
    {
      key: "paper",
      header: "Paper",
      render: (v) =>
        v.paper_trading ? (
          <Badge tone="good">yes</Badge>
        ) : (
          // The most load-bearing fact on this page: no paper endpoint means
          // enabling it is real money, whatever `agent.mode` says.
          <Badge tone="warning" title="No paper endpoint — this platform trades real money">
            live only
          </Badge>
        ),
      sortValue: (v) => (v.paper_trading ? 0 : 1),
    },
    {
      key: "equity",
      header: "Equity",
      render: (v) =>
        v.reports_equity ? (
          <Badge tone="good">yes</Badge>
        ) : v.active ? (
          <Badge
            tone="serious"
            title="Cannot value its own book — every loss limit is measured against equity summed across all venues, so the agent halts rather than trade without it"
          >
            no
          </Badge>
        ) : (
          <span className="muted">{DASH}</span>
        ),
      sortValue: (v) => (v.reports_equity ? 0 : 1),
    },
    {
      key: "session",
      header: "Session",
      render: (v) => (v.session === "unknown" ? DASH : sessionLabel(v.session)),
    },
  ];

  return (
    <div className="stack">
      <Card title="Platforms">
        <div className="grid grid--tiles">
          <StatTile label="Trading now" value={int(counts.active)} sub="in the registry" />
          <StatTile
            label="Configured"
            value={int(counts.total)}
            sub={`${int(counts.wanted)} enabled`}
          />
          <StatTile
            label="Enabled but not running"
            value={int(counts.blocked)}
            sub={counts.blocked > 0 ? "see the reason below" : "none"}
          />
        </div>
      </Card>

      <DatasetCard
        title="Configured venues"
        label="Venues"
        dataset={venues}
        datasetKey="venues"
      >
        {(data) => (
        <DataTable
          rows={data}
          columns={columns}
          rowKey={(v) => v.id}
          initialSort="status"
          initialDesc={false}
          empty={
            <EmptyState
              title="No venues configured"
              hint="With no [[venues]] section the agent falls back to the legacy Polymarket loop and trades none of the venue platforms."
            />
          }
        />
        )}
      </DatasetCard>

      {rows.some((v) => v.reason) && (
        <Card title="Why a venue is not trading">
          <ul className="reasons">
            {rows
              .filter((v) => v.reason)
              .map((v) => (
                <li key={v.id}>
                  <Badge tone={tone(v)}>{v.id}</Badge>{" "}
                  <span className="muted">{v.reason}</span>
                </li>
              ))}
          </ul>
        </Card>
      )}

      <Card title="Platforms this agent can trade">
        <p className="muted">
          Support is compiled in; a platform only trades when it also has a{" "}
          <code>[[venues]]</code> entry and credentials. Fees below are the
          config defaults, not live tier rates.
        </p>
        <DataTable
          rows={CATALOGUE}
          columns={catalogueColumns}
          rowKey={(p) => p.kind}
        />
      </Card>
    </div>
  );
}

function tone(v: VenueStatus): StatusTone {
  if (v.active) return "good";
  if (v.enabled) return "serious";
  return "neutral";
}

function label(v: VenueStatus): string {
  if (v.active) return "trading";
  if (v.enabled) return "blocked";
  return "disabled";
}

function assetLabel(raw: string): string {
  if (raw === "CryptoSpot") return "crypto spot";
  if (raw === "Equity") return "equities";
  if (raw === "PredictionBinary") return "prediction";
  return raw;
}

function sessionLabel(raw: string): string {
  return raw === "Always" ? "24/7" : raw;
}

interface Platform {
  kind: string;
  name: string;
  assets: string;
  paper: string;
  note: string;
}

/**
 * Every venue kind the binary knows how to build.
 *
 * Static on purpose: it answers "what *could* this agent trade", which is a
 * question about the build and not about the config. The table above answers
 * "what is it trading", which is the config.
 */
const CATALOGUE: Platform[] = [
  {
    kind: "alpaca",
    name: "Alpaca",
    assets: "US equities, crypto spot",
    paper: "yes",
    note: "Separate paper host and paper keys. Equities trade their session, including the 8pm–4am ET overnight window, limit-only; crypto is 24/7.",
  },
  {
    kind: "coinbase",
    name: "Coinbase Advanced Trade",
    assets: "crypto spot",
    paper: "no",
    note: "No paper endpoint — the sandbox has no matching engine, so the agent refuses to build this venue unless agent.mode = \"live\".",
  },
  {
    kind: "binance_us",
    name: "Binance.US",
    assets: "crypto spot",
    paper: "no",
    note: "No testnet. testnet.binance.vision belongs to global Binance, a different exchange that blocks US persons. Live mode only.",
  },
  {
    kind: "polymarket",
    name: "Polymarket",
    assets: "prediction markets",
    paper: "in-process",
    note: "Cannot report account equity, so it is refused in the venue registry; the legacy loop runs it when no [[venues]] are configured. Its CLOB prohibits US persons.",
  },
];

/** Kind → catalogue entry, for the per-venue rows above. */
const PLATFORMS: Record<string, Platform> = Object.fromEntries(
  CATALOGUE.map((p) => [p.kind, p]),
);

const catalogueColumns: Column<Platform>[] = [
  { key: "name", header: "Platform", render: (p) => p.name },
  { key: "assets", header: "Assets", render: (p) => p.assets },
  {
    key: "paper",
    header: "Paper",
    render: (p) =>
      p.paper === "yes" ? (
        <Badge tone="good">yes</Badge>
      ) : p.paper === "no" ? (
        <Badge tone="warning">live only</Badge>
      ) : (
        <Badge tone="neutral">{p.paper}</Badge>
      ),
  },
  {
    key: "note",
    header: "Notes",
    // Wrapped, not truncated: these are the facts that decide whether
    // enabling a platform costs real money, and an ellipsis hides the half
    // that matters.
    render: (p) => <span className="muted cell-wrap">{p.note}</span>,
  },
];
