import { useMemo, useState } from "react";
import { useDataset } from "../data/useDataset";
import { DatasetCard } from "../components/ui/DatasetCard";
import { Card } from "../components/ui/Card";
import { StatTile } from "../components/ui/StatTile";
import { EmptyState } from "../components/ui/States";
import { Badge, type StatusTone } from "../components/ui/Badge";
import { DataTable } from "../components/ui/DataTable";
import { num } from "../lib/decimal";
import { executionStats } from "../data/execution";
import { absTime, durationMs, int, price, relTime, DASH } from "../lib/format";
import { parseTs } from "../lib/time";
import { useNow } from "../data/useNow";
import type { FillRecord, OrderRecord } from "../api/types";

/**
 * Execution quality.
 *
 * This is the screen the paper window is actually measured on: the promotion
 * criteria are a fill rate, a median and p95 slippage, and zero UNKNOWN
 * orders older than a cycle. None of that was visible anywhere before — the
 * Trades page shows positions, which is what happened *after* execution
 * worked.
 */
export function Orders() {
  const orders = useDataset<OrderRecord[]>("orders");
  const fills = useDataset<FillRecord[]>("fills");
  const now = useNow();

  const [state, setState] = useState<string>("ALL");
  const [query, setQuery] = useState("");

  const all = orders.data ?? [];

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return all.filter((o) => {
      if (state !== "ALL" && o.state.toUpperCase() !== state) return false;
      if (q) {
        const hay = `${o.symbol} ${o.venue_id} ${o.client_order_id}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    });
  }, [all, state, query]);

  const stats = useMemo(() => executionStats(all, fills.data ?? []), [all, fills.data]);

  return (
    <div className="page">
      <Card title="Execution quality">
        <p className="muted" style={{ marginTop: 0 }}>
          Computed in this browser from the most recent 500 orders and fills,
          not reported by the agent. Slippage is measured against the mid at
          submission, so it is the cost of crossing — not the cost of being
          wrong.
        </p>
        <div className="grid grid--tiles">
          <StatTile
            label="Fill rate"
            value={stats.terminal === 0 ? DASH : `${(stats.fillRate * 100).toFixed(0)}%`}
            sub={
              stats.terminal === 0
                ? "no orders have reached a terminal state"
                : `${int(stats.filled)} of ${int(stats.terminal)} resolved orders`
            }
          />
          <StatTile
            label="Median slippage"
            value={stats.medianSlippage === null ? DASH : `${stats.medianSlippage.toFixed(1)} bps`}
            sub={stats.withSlippage === 0 ? "no fills carry a mid" : `${int(stats.withSlippage)} fills`}
          />
          <StatTile
            label="p95 slippage"
            value={stats.p95Slippage === null ? DASH : `${stats.p95Slippage.toFixed(1)} bps`}
          />
          <StatTile
            label="Median time to fill"
            value={stats.medianFillMs === null ? DASH : durationMs(stats.medianFillMs)}
          />
          <StatTile
            label="Unresolved"
            value={int(stats.unresolved)}
            sub="still working or unknown"
            tone={stats.unresolved > 0 ? "neg" : null}
          />
          <StatTile
            label="Unknown"
            value={int(stats.unknown)}
            /* The dangerous state: a submit that timed out may still have been
               accepted, so this is never "did not happen". */
            sub={stats.unknown > 0 ? "fate not established — never retried blind" : "none"}
            tone={stats.unknown > 0 ? "neg" : null}
          />
        </div>
      </Card>

      <DatasetCard
        title="Orders"
        label="Orders"
        dataset={orders}
        datasetKey="orders"
        actions={
          <div className="filters">
            <label>
              <span className="muted">State</span>{" "}
              <select value={state} onChange={(e) => setState(e.target.value)}>
                <option value="ALL">All</option>
                {ORDER_STATES.map((s) => (
                  <option key={s} value={s}>
                    {s}
                  </option>
                ))}
              </select>
            </label>
            <label>
              <span className="muted">Find</span>{" "}
              <input
                value={query}
                placeholder="symbol, venue, id"
                onChange={(e) => setQuery(e.target.value)}
              />
            </label>
          </div>
        }
      >
        {() =>
          filtered.length === 0 ? (
            <EmptyState
              title={all.length === 0 ? "No orders yet" : "Nothing matches those filters"}
              hint={
                all.length === 0
                  ? "Orders appear once a venue is configured and the agent takes a view."
                  : undefined
              }
            />
          ) : (
            <DataTable
              rows={filtered}
              rowKey={(o) => o.client_order_id}
              columns={[
                {
                  key: "state",
                  header: "State",
                  render: (o) => <Badge tone={orderStateTone(o.state)}>{o.state}</Badge>,
                  sortValue: (o) => o.state,
                },
                { key: "venue", header: "Venue", render: (o) => o.venue_id },
                { key: "symbol", header: "Symbol", render: (o) => o.symbol },
                { key: "side", header: "Side", render: (o) => o.side },
                {
                  key: "intent",
                  header: "Intent",
                  render: (o) => (
                    <span className={o.intent === "EXIT" ? "muted" : undefined}>{o.intent}</span>
                  ),
                },
                {
                  key: "qty",
                  header: "Qty",
                  numeric: true,
                  render: (o) => `${o.filled_qty} / ${o.qty}`,
                },
                {
                  key: "limit",
                  header: "Limit",
                  numeric: true,
                  render: (o) => price(num(o.limit_price)),
                  sortValue: (o) => num(o.limit_price),
                },
                {
                  key: "avg",
                  header: "Avg fill",
                  numeric: true,
                  render: (o) => price(num(o.avg_fill_price)),
                  sortValue: (o) => num(o.avg_fill_price),
                },
                {
                  key: "submitted",
                  header: "Submitted",
                  render: (o) => {
                    const t = parseTs(o.submitted_at);
                    return t ? relTime(t, now) : DASH;
                  },
                  sortValue: (o) => parseTs(o.submitted_at)?.getTime() ?? null,
                  title: (o) => absTime(parseTs(o.submitted_at)) || undefined,
                },
                {
                  key: "why",
                  header: "Reason",
                  truncate: true,
                  render: (o) => o.reject_reason ?? DASH,
                  title: (o) => o.reject_reason ?? undefined,
                },
              ]}
            />
          )
        }
      </DatasetCard>

      <DatasetCard title="Fills" label="Fills" dataset={fills} datasetKey="fills">
        {(rows) =>
          rows.length === 0 ? (
            <EmptyState
              title="No fills yet"
              hint="A fill is recorded when the venue confirms one, not when an order is accepted."
            />
          ) : (
            <DataTable
              rows={rows}
              rowKey={(f) => String(f.id)}
              columns={[
                { key: "venue", header: "Venue", render: (f) => f.venue_id },
                { key: "symbol", header: "Symbol", render: (f) => f.symbol },
                { key: "side", header: "Side", render: (f) => f.side },
                { key: "qty", header: "Qty", numeric: true, render: (f) => f.qty },
                {
                  key: "price",
                  header: "Price",
                  numeric: true,
                  render: (f) => price(num(f.price)),
                  sortValue: (f) => num(f.price),
                },
                {
                  key: "mid",
                  header: "Mid at submit",
                  numeric: true,
                  render: (f) => price(num(f.mid_at_submit)),
                  sortValue: (f) => num(f.mid_at_submit),
                },
                {
                  key: "slip",
                  header: "Slippage",
                  numeric: true,
                  render: (f) => {
                    const v = num(f.slippage_bps);
                    return v === null ? DASH : `${v.toFixed(1)} bps`;
                  },
                  sortValue: (f) => num(f.slippage_bps),
                },
                {
                  key: "ttf",
                  header: "Time to fill",
                  numeric: true,
                  render: (f) => durationMs(f.time_to_fill_ms),
                  sortValue: (f) => f.time_to_fill_ms,
                },
                {
                  key: "fee",
                  header: "Fee",
                  numeric: true,
                  render: (f) => price(num(f.fee)),
                  sortValue: (f) => num(f.fee),
                },
                {
                  key: "at",
                  header: "Filled",
                  render: (f) => {
                    const t = parseTs(f.filled_at);
                    return t ? relTime(t, now) : DASH;
                  },
                  sortValue: (f) => parseTs(f.filled_at)?.getTime() ?? null,
                  title: (f) => absTime(parseTs(f.filled_at)) || undefined,
                },
              ]}
            />
          )
        }
      </DatasetCard>
    </div>
  );
}

const ORDER_STATES = [
  "PENDING",
  "ACCEPTED",
  "PARTIALLY_FILLED",
  "FILLED",
  "CANCELLED",
  "EXPIRED",
  "REJECTED",
  "UNKNOWN",
] as const;

function orderStateTone(state: string): StatusTone {
  switch (state.toUpperCase()) {
    case "FILLED":
      return "good";
    case "PARTIALLY_FILLED":
      return "warning";
    case "REJECTED":
      return "critical";
    // Not an error, and not safe either: a submit that timed out may still
    // have been accepted. It blocks its symbol until reconciliation resolves
    // it, which is the right direction but not a free one.
    case "UNKNOWN":
      return "critical";
    case "CANCELLED":
    case "EXPIRED":
      return "serious";
    default:
      return "neutral";
  }
}
