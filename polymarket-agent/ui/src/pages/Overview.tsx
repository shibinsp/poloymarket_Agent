import { useDataset } from "../data/useDataset";
import { DatasetCard } from "../components/ui/DatasetCard";
import { Card } from "../components/ui/Card";
import { StatTile, toneOf } from "../components/ui/StatTile";
import { EmptyState } from "../components/ui/States";
import { Badge, tradeStatusTone } from "../components/ui/Badge";
import { DataTable } from "../components/ui/DataTable";
import { LineChart } from "../components/charts/LineChart";
import { ColumnChart } from "../components/charts/ColumnChart";
import { ChartOrTable } from "../components/charts/ChartOrTable";
import { Link } from "../router/Link";
import { cyclesAscending, tradesNewestFirst } from "../data/derive";
import { num } from "../lib/decimal";
import { DASH, int, money, moneySigned, pctFromFraction, price } from "../lib/format";
import { decimate } from "../lib/scale";
import type { CycleRecord, Health, Metrics, TradeRecord } from "../api/types";

export function Overview() {
  const metrics = useDataset<Metrics>("metrics");
  const cycles = useDataset<CycleRecord[]>("cyclesAll");
  const trades = useDataset<TradeRecord[]>("trades");
  useDataset<Health>("health"); // the shell renders it; keep it polled here too

  const ordered = cycles.data ? cyclesAscending(cycles.data) : [];
  const latest = ordered.at(-1) ?? null;
  const previous = ordered.at(-2) ?? null;

  const bankroll = num(latest?.bankroll);
  const prevBankroll = num(previous?.bankroll);
  const delta =
    bankroll !== null && prevBankroll !== null ? bankroll - prevBankroll : null;

  const m = metrics.data;

  return (
    <div className="page">
      <Card title="Portfolio">
        <div className="grid grid--tiles">
          <StatTile
            hero
            label="Bankroll"
            /*
             * From the most recent cycle, and nothing else. The old dashboard
             * fell back to `realized_pnl + 100`, hard-coding an initial
             * balance that is configurable and is not served by any endpoint.
             * With no completed cycle the honest answer is that we do not know.
             */
            value={bankroll === null ? DASH : money(bankroll)}
            tone={toneOf(delta)}
            sub={
              latest === null
                ? "no completed cycle yet"
                : delta === null
                  ? `as of cycle ${latest.cycle_number}`
                  : `${moneySigned(delta)} since cycle ${previous?.cycle_number}`
            }
          />
          <StatTile
            label="Net profit"
            value={money(num(m?.net_profit))}
            tone={toneOf(num(m?.net_profit))}
            sub="realized P&L less API cost"
          />
          <StatTile
            label="Realized P&L"
            value={money(num(m?.realized_pnl))}
            tone={toneOf(num(m?.realized_pnl))}
          />
          <StatTile
            label="Unrealized exposure"
            value={money(num(m?.unrealized_exposure))}
            sub="value of open positions"
          />
          <StatTile
            label="Win rate"
            value={pctFromFraction(num(m?.win_rate))}
            sub={
              m
                ? `${int(m.wins)}W / ${int(m.losses)}L of ${int(m.resolved_trades)} resolved`
                : undefined
            }
          />
          <StatTile
            label="ROI"
            value={pctFromFraction(num(m?.roi_pct))}
            title="Net profit divided by the initial bankroll. The field is a fraction despite being named roi_pct."
          />
          <StatTile label="Trades" value={int(m?.total_trades ?? null)} sub={m ? `${int(m.open_trades)} open` : undefined} />
          <StatTile label="Cycles" value={int(m?.cycles_completed ?? null)} />
          <StatTile label="API cost" value={money(num(m?.total_api_cost))} />
          <StatTile
            label="Sharpe"
            /* Null until two trades have resolved — showing 0 would read as
               "measured, and flat", which is a different claim. */
            value={m && m.sharpe_ratio === null ? "n/a" : (num(m?.sharpe_ratio)?.toFixed(2) ?? DASH)}
            sub={m && m.sharpe_ratio === null ? "needs ≥2 resolved trades" : undefined}
          />
        </div>
      </Card>

      <div className="grid grid--2">
        <DatasetCard
          title="Bankroll over cycles"
          label="Cycle history"
          dataset={cycles}
          datasetKey="cyclesAll"
        >
          {(rows) => {
            const asc = cyclesAscending(rows);
            const shown = decimate(asc, (c) => num(c.bankroll), 240);
            if (shown.length === 0) {
              return (
                <EmptyState
                  title="No cycles recorded yet"
                  hint="The first cycle is written when it completes."
                />
              );
            }
            return (
              <ChartOrTable
                label="Bankroll"
                chart={
                  <LineChart
                    ariaLabel="Bankroll over cycles"
                    categories={shown.map((c) => String(c.cycle_number))}
                    formatX={(c) => "C" + c}
                    formatY={(v) => money(v)}
                    series={[
                      {
                        name: "Bankroll",
                        color: "var(--series-1)",
                        values: shown.map((c) => num(c.bankroll)),
                      },
                    ]}
                  />
                }
                table={
                  <DataTable
                    rows={[...asc].reverse().slice(0, 100)}
                    rowKey={(c) => String(c.id ?? c.cycle_number)}
                    columns={[
                      { key: "c", header: "Cycle", numeric: true, render: (c) => int(c.cycle_number) },
                      { key: "b", header: "Bankroll", numeric: true, render: (c) => money(num(c.bankroll)) },
                    ]}
                  />
                }
              />
            );
          }}
        </DatasetCard>

        <DatasetCard
          title="Trades placed per cycle"
          label="Cycle history"
          dataset={cycles}
          datasetKey="cyclesAll"
        >
          {(rows) => {
            const asc = cyclesAscending(rows);
            const shown = decimate(asc, (c) => c.trades_placed, 120);
            if (shown.length === 0) {
              return <EmptyState title="No cycles recorded yet" />;
            }
            return (
              <ChartOrTable
                label="Trades placed"
                chart={
                  <ColumnChart
                    ariaLabel="Trades placed per cycle"
                    categories={shown.map((c) => String(c.cycle_number))}
                    formatX={(c) => "C" + c}
                    formatY={(v) => int(v)}
                    series={[
                      {
                        name: "Trades placed",
                        color: "var(--series-1)",
                        values: shown.map((c) => c.trades_placed),
                      },
                    ]}
                  />
                }
                table={
                  <DataTable
                    rows={[...asc].reverse().slice(0, 100)}
                    rowKey={(c) => String(c.id ?? c.cycle_number)}
                    columns={[
                      { key: "c", header: "Cycle", numeric: true, render: (c) => int(c.cycle_number) },
                      { key: "t", header: "Trades", numeric: true, render: (c) => int(c.trades_placed) },
                    ]}
                  />
                }
              />
            );
          }}
        </DatasetCard>
      </div>

      <DatasetCard
        title="Recent trades"
        label="Recent trades"
        dataset={trades}
        datasetKey="trades"
        actions={<Link to="/trades">View all →</Link>}
      >
        {(rows) =>
          rows.length === 0 ? (
            <EmptyState
              title="No trades yet"
              hint="Trades appear once a cycle finds an edge above the threshold."
            />
          ) : (
            <DataTable
              rows={tradesNewestFirst(rows).slice(0, 8)}
              rowKey={(t, i) => String(t.id ?? i)}
              columns={[
                {
                  key: "market",
                  header: "Market",
                  truncate: true,
                  render: (t) => t.market_question ?? t.market_id,
                  title: (t) => t.market_question ?? t.market_id,
                },
                { key: "dir", header: "Side", render: (t) => t.direction },
                { key: "price", header: "Price", numeric: true, render: (t) => price(num(t.entry_price)) },
                { key: "size", header: "Size", numeric: true, render: (t) => money(num(t.size)) },
                { key: "edge", header: "Edge", numeric: true, render: (t) => pctFromFraction(num(t.edge_at_entry)) },
                {
                  key: "status",
                  header: "Status",
                  render: (t) => <Badge tone={tradeStatusTone(t.status)}>{t.status}</Badge>,
                },
              ]}
            />
          )
        }
      </DatasetCard>
    </div>
  );
}
