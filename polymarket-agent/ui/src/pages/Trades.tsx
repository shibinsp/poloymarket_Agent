import { useMemo, useState } from "react";
import { useDataset } from "../data/useDataset";
import { DatasetCard } from "../components/ui/DatasetCard";
import { Card } from "../components/ui/Card";
import { StatTile, toneOf } from "../components/ui/StatTile";
import { EmptyState } from "../components/ui/States";
import { Badge, tradeStatusTone } from "../components/ui/Badge";
import { DataTable } from "../components/ui/DataTable";
import { ColumnChart } from "../components/charts/ColumnChart";
import { LineChart } from "../components/charts/LineChart";
import { ChartOrTable } from "../components/charts/ChartOrTable";
import { cumulative, tradesNewestFirst } from "../data/derive";
import { num, sum } from "../lib/decimal";
import { absTime, int, money, moneySigned, pctFromFraction, price, relTime, DASH } from "../lib/format";
import { parseTs } from "../lib/time";
import { useNow } from "../data/useNow";
import { TRADE_STATUSES, type TradeRecord } from "../api/types";

export function Trades() {
  const trades = useDataset<TradeRecord[]>("tradesAll");
  const now = useNow();

  const [status, setStatus] = useState<string>("ALL");
  const [direction, setDirection] = useState<string>("ALL");
  const [query, setQuery] = useState("");

  const all = trades.data ?? [];

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return all.filter((t) => {
      if (status !== "ALL" && t.status.toUpperCase() !== status) return false;
      if (direction !== "ALL" && t.direction.toUpperCase() !== direction) return false;
      if (q) {
        const hay = `${t.market_question ?? ""} ${t.market_id}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    });
  }, [all, status, direction, query]);

  const filtersActive = status !== "ALL" || direction !== "ALL" || query.trim() !== "";

  // Recomputed from the filtered set, and labelled as such — these are the
  // client's arithmetic over what is on screen, not the server's figures.
  const realized = sum(filtered.map((t) => t.pnl));
  const wins = filtered.filter((t) => t.status.toUpperCase() === "RESOLVED_WIN").length;
  const losses = filtered.filter((t) => t.status.toUpperCase() === "RESOLVED_LOSS").length;

  const byOldest = useMemo(
    () => [...filtered].sort((a, b) => (a.id ?? 0) - (b.id ?? 0)),
    [filtered],
  );
  const pnls = byOldest.map((t) => num(t.pnl));

  return (
    <div className="page">
      <Card title={filtersActive ? "Selection" : "All trades"}>
        <div className="grid grid--tiles">
          <StatTile label="Trades shown" value={int(filtered.length)} sub={`of ${int(all.length)} total`} />
          <StatTile label="Realized P&L" value={money(realized)} tone={toneOf(realized)} sub="summed on this page" />
          <StatTile label="Resolved" value={int(wins + losses)} sub={`${int(wins)}W / ${int(losses)}L`} />
        </div>
      </Card>

      <Card title="Filters">
        <div className="filters">
          <label className="field">
            <span>Status</span>
            <select className="select" value={status} onChange={(e) => setStatus(e.target.value)}>
              <option value="ALL">All statuses</option>
              {TRADE_STATUSES.map((s) => (
                <option key={s} value={s}>{s}</option>
              ))}
            </select>
          </label>
          <label className="field">
            <span>Direction</span>
            <select className="select" value={direction} onChange={(e) => setDirection(e.target.value)}>
              <option value="ALL">Both</option>
              <option value="YES">YES</option>
              <option value="NO">NO</option>
            </select>
          </label>
          <label className="field" style={{ flex: 1, minWidth: 200 }}>
            <span>Search market</span>
            <input
              className="input"
              value={query}
              placeholder="question or condition id"
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
          {filtersActive && (
            <button
              className="btn"
              onClick={() => {
                setStatus("ALL");
                setDirection("ALL");
                setQuery("");
              }}
            >
              Clear filters
            </button>
          )}
        </div>
      </Card>

      <DatasetCard title="Trades" label="All trades" dataset={trades} datasetKey="tradesAll">
        {(rows) =>
          rows.length === 0 ? (
            <EmptyState
              title="No trades yet"
              hint="A trade is recorded when a cycle finds an edge above the configured threshold."
            />
          ) : filtered.length === 0 ? (
            /* Deliberately a different message from "no trades yet": the data
               exists, the filters just exclude all of it. */
            <EmptyState
              title="No trades match these filters"
              hint={`${int(rows.length)} trades are loaded.`}
              action={
                <button
                  className="btn"
                  onClick={() => {
                    setStatus("ALL");
                    setDirection("ALL");
                    setQuery("");
                  }}
                >
                  Clear filters
                </button>
              }
            />
          ) : (
            <DataTable
              rows={tradesNewestFirst(filtered)}
              rowKey={(t, i) => String(t.id ?? i)}
              initialSort="created"
              columns={[
                {
                  key: "market",
                  header: "Market",
                  truncate: true,
                  render: (t) => t.market_question ?? t.market_id,
                  title: (t) => `${t.market_question ?? ""}\n${t.market_id}`,
                  sortValue: (t) => t.market_question ?? t.market_id,
                },
                { key: "dir", header: "Side", render: (t) => t.direction, sortValue: (t) => t.direction },
                { key: "price", header: "Entry", numeric: true, render: (t) => price(num(t.entry_price)), sortValue: (t) => num(t.entry_price) },
                { key: "fair", header: "Fair value", numeric: true, render: (t) => price(num(t.claude_fair_value)), sortValue: (t) => num(t.claude_fair_value) },
                { key: "size", header: "Size", numeric: true, render: (t) => money(num(t.size)), sortValue: (t) => num(t.size) },
                { key: "edge", header: "Edge", numeric: true, render: (t) => pctFromFraction(num(t.edge_at_entry)), sortValue: (t) => num(t.edge_at_entry) },
                { key: "conf", header: "Confidence", numeric: true, render: (t) => pctFromFraction(num(t.confidence)), sortValue: (t) => num(t.confidence) },
                { key: "kelly", header: "Kelly adj.", numeric: true, render: (t) => pctFromFraction(num(t.kelly_adjusted)), sortValue: (t) => num(t.kelly_adjusted) },
                {
                  key: "status",
                  header: "Status",
                  render: (t) => <Badge tone={tradeStatusTone(t.status)}>{t.status}</Badge>,
                  sortValue: (t) => t.status,
                },
                {
                  key: "pnl",
                  header: "P&L",
                  numeric: true,
                  render: (t) => {
                    const v = num(t.pnl);
                    return (
                      <span className={v === null ? "" : v > 0 ? "is-pos" : v < 0 ? "is-neg" : ""}>
                        {v === null ? DASH : moneySigned(v)}
                      </span>
                    );
                  },
                  sortValue: (t) => num(t.pnl),
                },
                {
                  key: "created",
                  header: "Opened",
                  render: (t) => relTime(parseTs(t.created_at), now),
                  title: (t) => absTime(parseTs(t.created_at)),
                  sortValue: (t) => parseTs(t.created_at)?.getTime() ?? null,
                },
              ]}
            />
          )
        }
      </DatasetCard>

      {byOldest.length > 0 && (
        <div className="grid grid--2">
          <DatasetCard title="Realized P&L per trade" label="All trades" dataset={trades} datasetKey="tradesAll">
            {() => (
              <ChartOrTable
                label="P&L per trade"
                chart={
                  <ColumnChart
                    ariaLabel="Realized profit and loss per trade"
                    diverging
                    categories={byOldest.map((t) => String(t.id ?? ""))}
                    formatX={(c) => "#" + c}
                    formatY={(v) => money(v)}
                    series={[{ name: "Realized P&L", color: "var(--series-1)", values: pnls }]}
                  />
                }
                table={
                  <DataTable
                    rows={byOldest}
                    rowKey={(t, i) => String(t.id ?? i)}
                    columns={[
                      { key: "id", header: "Trade", numeric: true, render: (t) => "#" + int(t.id ?? null) },
                      { key: "p", header: "P&L", numeric: true, render: (t) => moneySigned(num(t.pnl)) },
                    ]}
                  />
                }
              />
            )}
          </DatasetCard>

          <DatasetCard title="Cumulative realized P&L" label="All trades" dataset={trades} datasetKey="tradesAll">
            {() => (
              <ChartOrTable
                label="Cumulative P&L"
                chart={
                  <LineChart
                    ariaLabel="Cumulative realized profit and loss"
                    includeZero
                    categories={byOldest.map((t) => String(t.id ?? ""))}
                    formatX={(c) => "#" + c}
                    formatY={(v) => money(v)}
                    series={[{ name: "Cumulative", color: "var(--series-1)", values: cumulative(pnls) }]}
                  />
                }
                table={
                  <DataTable
                    rows={byOldest.map((t, i) => ({ t, c: cumulative(pnls)[i] }))}
                    rowKey={(r, i) => String(r.t.id ?? i)}
                    columns={[
                      { key: "id", header: "Trade", numeric: true, render: (r) => "#" + int(r.t.id ?? null) },
                      { key: "c", header: "Cumulative", numeric: true, render: (r) => money(r.c) },
                    ]}
                  />
                }
              />
            )}
          </DatasetCard>
        </div>
      )}
    </div>
  );
}
