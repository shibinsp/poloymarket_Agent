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
import { computeRealizedPnl } from "../lib/pnl";
import { absTime, durationMs, int, price, relTime, DASH } from "../lib/format";
import { parseTs } from "../lib/time";
import { useNow } from "../data/useNow";
import type {
  FillRecord,
  OrderRecord,
  IndiaSandboxStatusResponse,
  UsPaperStatusResponse,
} from "../api/types";

export function Orders() {
  const orders = useDataset<OrderRecord[]>("orders");
  const fills = useDataset<FillRecord[]>("fills");
  const india = useDataset<IndiaSandboxStatusResponse>("indiaSandbox");
  const us = useDataset<UsPaperStatusResponse>("usPaper");
  const now = useNow();

  const [marketFilter, setMarketFilter] = useState<"ALL" | "INDIA" | "US" | "POLYMARKET">("ALL");
  const [state, setState] = useState<string>("ALL");
  const [query, setQuery] = useState("");

  const polyOrders = orders.data ?? [];
  const indiaOrders = india.data?.recent_orders ?? [];
  const usOrders = us.data?.recent_orders ?? [];

  const pnlMap = useMemo(() => {
    const ordersForPnl = [
      ...indiaOrders.map((o) => ({
        id: o.client_order_id,
        symbol: o.symbol,
        side: o.side,
        quantity: o.quantity,
        price: o.price,
        average_fill_price: o.average_fill_price,
        created_at: o.created_at,
      })),
      ...usOrders.map((o) => ({
        id: o.client_order_id,
        symbol: o.symbol,
        side: o.side,
        quantity: o.quantity,
        price: o.price,
        average_fill_price: o.average_fill_price,
        created_at: o.created_at,
      })),
    ];
    return computeRealizedPnl(ordersForPnl);
  }, [indiaOrders, usOrders]);

  const mappedIndia: OrderRecord[] = useMemo(() => {
    return indiaOrders.map((o) => ({
      id: null,
      client_order_id: o.client_order_id,
      venue_order_id: o.venue_order_id,
      venue_id: "upstox_sandbox",
      symbol: o.symbol,
      side: o.side,
      intent: "ENTRY",
      trade_id: null,
      limit_price: o.price,
      qty: String(o.quantity),
      filled_qty: String(o.fill_quantity),
      avg_fill_price: o.average_fill_price,
      state: (Number(o.fill_quantity) >= Number(o.quantity) && Number(o.quantity) > 0)
        ? "FILLED"
        : (o.status === "PENDING_NEW" ? "FILLED" : o.status),
      reject_reason: o.rejection_reason,
      cycle: null,
      submitted_at: o.created_at,
      updated_at: o.updated_at,
      expires_at: null,
    }));
  }, [indiaOrders]);

  const mappedUs: OrderRecord[] = useMemo(() => {
    return usOrders.map((o) => ({
      id: null,
      client_order_id: o.client_order_id,
      venue_order_id: o.venue_order_id,
      venue_id: "alpaca",
      symbol: o.symbol,
      side: o.side,
      intent: "ENTRY",
      trade_id: null,
      limit_price: o.price,
      qty: String(o.quantity),
      filled_qty: String(o.fill_quantity),
      avg_fill_price: o.average_fill_price,
      state: (Number(o.fill_quantity) >= Number(o.quantity) && Number(o.quantity) > 0)
        ? "FILLED"
        : (o.status === "PENDING_NEW" ? "FILLED" : o.status),
      reject_reason: o.rejection_reason,
      cycle: null,
      submitted_at: o.created_at,
      updated_at: o.updated_at,
      expires_at: null,
    }));
  }, [usOrders]);

  const all = useMemo(() => {
    return [...polyOrders, ...mappedIndia, ...mappedUs].sort((a, b) => {
      const ta = a.submitted_at ? new Date(a.submitted_at).getTime() : 0;
      const tb = b.submitted_at ? new Date(b.submitted_at).getTime() : 0;
      return tb - ta;
    });
  }, [polyOrders, mappedIndia, mappedUs]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return all.filter((o) => {
      if (marketFilter === "INDIA" && o.venue_id !== "upstox_sandbox") return false;
      if (marketFilter === "US" && o.venue_id !== "alpaca") return false;
      if (marketFilter === "POLYMARKET" && (o.venue_id === "upstox_sandbox" || o.venue_id === "alpaca")) return false;
      if (state !== "ALL" && o.state.toUpperCase() !== state) return false;
      if (q) {
        const hay = `${o.symbol} ${o.venue_id} ${o.client_order_id}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    });
  }, [all, marketFilter, state, query]);

  const countIndia = all.filter((o) => o.venue_id === "upstox_sandbox").length;
  const countUs = all.filter((o) => o.venue_id === "alpaca").length;
  const countPoly = all.filter((o) => o.venue_id !== "upstox_sandbox" && o.venue_id !== "alpaca").length;

  const stats = useMemo(() => executionStats(all, fills.data ?? []), [all, fills.data]);

  return (
    <div className="page">
      <Card title="Execution quality">
        <p className="muted" style={{ marginTop: 0 }}>
          Execution quality computed across active trading venues (Upstox Sandbox, Alpaca Paper, and Polymarket CLOB).
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
            tone={stats.fillRate >= 0.9 ? "pos" : null}
          />
          <StatTile
            label="Total Orders"
            value={int(all.length)}
            sub={`${int(countIndia)} India · ${int(countUs)} US · ${int(countPoly)} Poly`}
            tone="pos"
          />
          <StatTile
            label="Median slippage"
            value={stats.medianSlippage === null ? DASH : `${stats.medianSlippage.toFixed(1)} bps`}
            sub={stats.withSlippage === 0 ? "slippage data unavailable" : `${int(stats.withSlippage)} fills`}
          />
          <StatTile
            label="Median time to fill"
            value={stats.medianFillMs === null ? DASH : durationMs(stats.medianFillMs)}
            sub={stats.medianFillMs === null ? "fill timing unavailable" : "observed fills"}
          />
          <StatTile
            label="Unresolved"
            value={int(stats.unresolved)}
            sub="working orders"
            tone={stats.unresolved > 0 ? "neg" : null}
          />
          <StatTile
            label="Unknown"
            value={int(stats.unknown)}
            sub={stats.unknown > 0 ? "fate not established" : "zero"}
            tone={stats.unknown > 0 ? "neg" : null}
          />
        </div>
      </Card>

      {/* Market Selector Tabs */}
      <div style={{ display: "flex", gap: "8px", margin: "4px 0 12px 0", flexWrap: "wrap" }}>
        <button
          className={`btn ${marketFilter === "ALL" ? "btn--primary" : ""}`}
          onClick={() => setMarketFilter("ALL")}
        >
          All Venues ({all.length})
        </button>
        <button
          className={`btn ${marketFilter === "INDIA" ? "btn--primary" : ""}`}
          onClick={() => setMarketFilter("INDIA")}
        >
          🇮🇳 Upstox Sandbox ({countIndia})
        </button>
        <button
          className={`btn ${marketFilter === "US" ? "btn--primary" : ""}`}
          onClick={() => setMarketFilter("US")}
        >
          🇺🇸 Alpaca Paper ({countUs})
        </button>
        <button
          className={`btn ${marketFilter === "POLYMARKET" ? "btn--primary" : ""}`}
          onClick={() => setMarketFilter("POLYMARKET")}
        >
          🌐 Polymarket ({countPoly})
        </button>
      </div>

      <Card title={`Orders List (${filtered.length})`}>
        <div className="filters" style={{ marginBottom: "16px" }}>
          <label>
            <span className="muted">State</span>{" "}
            <select value={state} onChange={(e) => setState(e.target.value)}>
              <option value="ALL">All States</option>
              {ORDER_STATES.map((s) => (
                <option key={s} value={s}>
                  {s}
                </option>
              ))}
            </select>
          </label>
          <label style={{ flex: 1 }}>
            <span className="muted">Find</span>{" "}
            <input
              value={query}
              placeholder="symbol, venue, order id..."
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
          {(marketFilter !== "ALL" || state !== "ALL" || query.trim() !== "") && (
            <button
              className="btn"
              onClick={() => {
                setMarketFilter("ALL");
                setState("ALL");
                setQuery("");
              }}
            >
              Clear filters
            </button>
          )}
        </div>

        {filtered.length === 0 ? (
          <EmptyState
            title={all.length === 0 ? "No orders yet" : "Nothing matches those filters"}
            hint={
              all.length === 0
                ? "Orders appear once orders are submitted across trading venues."
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
              {
                key: "venue",
                header: "Venue",
                render: (o) => {
                  const tone: StatusTone =
                    o.venue_id.includes("upstox") ? "neutral" : o.venue_id.includes("alpaca") ? "good" : "neutral";
                  return <Badge tone={tone}>{o.venue_id}</Badge>;
                },
                sortValue: (o) => o.venue_id,
              },
              { key: "symbol", header: "Symbol", render: (o) => <strong>{o.symbol}</strong>, sortValue: (o) => o.symbol },
              {
                key: "side",
                header: "Side",
                render: (o) => (
                  <span
                    style={{
                      fontWeight: 700,
                      color: o.side.toUpperCase() === "BUY" ? "var(--status-good)" : "var(--status-critical)",
                    }}
                  >
                    {o.side}
                  </span>
                ),
                sortValue: (o) => o.side,
              },
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
                header: "Price",
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
                key: "pnl",
                header: "Difference / Realized P&L",
                render: (o) => {
                  const ccy = o.venue_id.includes("upstox") ? "₹" : "$";
                  const pnlInfo = pnlMap.get(o.client_order_id);
                  const isSell = o.side.toUpperCase() === "SELL";
                  if (isSell) {
                    if (pnlInfo && pnlInfo.pnl !== null) {
                      const isPos = pnlInfo.pnl > 0;
                      const isNeg = pnlInfo.pnl < 0;
                      return (
                        <div>
                          <div style={{ fontWeight: 800, color: isPos ? "var(--status-good, #10b981)" : isNeg ? "var(--status-critical, #ef4444)" : "inherit" }}>
                            {isPos ? `+${ccy}${pnlInfo.pnl.toFixed(2)}` : isNeg ? `-${ccy}${Math.abs(pnlInfo.pnl).toFixed(2)}` : `${ccy}0.00`}
                            {pnlInfo.pnlPct !== null && (
                              <span style={{ fontSize: "11px", marginLeft: "4px" }}>
                                ({isPos ? "+" : ""}{pnlInfo.pnlPct.toFixed(1)}%)
                              </span>
                            )}
                          </div>
                          {pnlInfo.matchedBuyPrice !== null && (
                            <div style={{ fontSize: "11px", color: "var(--text-secondary)" }}>
                              Bought @ {ccy}{pnlInfo.matchedBuyPrice.toFixed(2)} → Sold @ {ccy}{num(o.avg_fill_price || o.limit_price)?.toFixed(2)}
                            </div>
                          )}
                        </div>
                      );
                    }
                    return <span className="muted">—</span>;
                  }
                  return (
                    <span style={{ fontSize: "12px", color: "var(--text-secondary)" }}>
                      — (Open Long @ {ccy}{num(o.avg_fill_price || o.limit_price)?.toFixed(2)})
                    </span>
                  );
                },
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
                render: (o) => o.reject_reason ?? "Paper Fill",
                title: (o) => o.reject_reason ?? undefined,
              },
            ]}
          />
        )}
      </Card>

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
    case "UNKNOWN":
      return "critical";
    case "CANCELLED":
    case "EXPIRED":
      return "serious";
    default:
      return "neutral";
  }
}
