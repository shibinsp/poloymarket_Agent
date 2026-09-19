import { useDataset } from "../data/useDataset";
import { DatasetCard } from "../components/ui/DatasetCard";
import { StatTile } from "../components/ui/StatTile";
import { EmptyState } from "../components/ui/States";
import { Badge, agentStateTone } from "../components/ui/Badge";
import { DataTable } from "../components/ui/DataTable";
import { ColumnChart } from "../components/charts/ColumnChart";
import { LineChart } from "../components/charts/LineChart";
import { ChartOrTable } from "../components/charts/ChartOrTable";
import { cyclesAscending } from "../data/derive";
import { num } from "../lib/decimal";
import { absTime, durationMs, int, money, moneySigned, relTime } from "../lib/format";
import { parseTs } from "../lib/time";
import { decimate } from "../lib/scale";
import { useNow } from "../data/useNow";
import type { CycleRecord, Metrics } from "../api/types";

/** The three scan stages, as separate small multiples. */
const FUNNEL: {
  key: "markets_scanned" | "opportunities_found" | "trades_placed";
  name: string;
  color: string;
}[] = [
  { key: "markets_scanned", name: "Markets scanned", color: "var(--series-1)" },
  { key: "opportunities_found", name: "Opportunities", color: "var(--series-2)" },
  { key: "trades_placed", name: "Trades placed", color: "var(--series-3)" },
];

export function Cycles() {
  const cycles = useDataset<CycleRecord[]>("cyclesAll");
  const latest = useDataset<CycleRecord | null>("cycle");
  const metrics = useDataset<Metrics>("metrics");
  const now = useNow();

  const avgDuration = metrics.data?.avg_cycle_duration_ms ?? null;

  return (
    <div className="page">
      <DatasetCard title="Latest cycle" label="Latest cycle" dataset={latest} datasetKey="cycle">
        {(c) =>
          /* `/api/cycles` answers with a bare null before the first cycle —
             a successful "nothing yet", not a failure. */
          c === null ? (
            <EmptyState
              title="No cycle has completed yet"
              hint="The first record is written when the first cycle finishes."
            />
          ) : (
            <div className="grid grid--tiles">
              <StatTile label="Cycle" value={int(c.cycle_number)} sub={<Badge tone={agentStateTone(c.agent_state)}>{c.agent_state}</Badge>} />
              <StatTile label="Markets scanned" value={int(c.markets_scanned)} />
              <StatTile label="Opportunities" value={int(c.opportunities_found)} />
              <StatTile label="Trades placed" value={int(c.trades_placed)} />
              <StatTile label="Bankroll" value={money(num(c.bankroll))} />
              <StatTile label="Duration" value={durationMs(c.duration_ms)} />
              <StatTile
                label="Finished"
                value={relTime(parseTs(c.created_at), now)}
                title={absTime(parseTs(c.created_at))}
              />
            </div>
          )
        }
      </DatasetCard>

      <DatasetCard title="Bankroll and unrealized P&L" label="Cycle history" dataset={cycles} datasetKey="cyclesAll">
        {(rows) => {
          const asc = cyclesAscending(rows);
          const shown = decimate(asc, (c) => num(c.bankroll), 240);
          if (shown.length === 0) return <EmptyState title="No cycles recorded yet" />;
          const cats = shown.map((c) => String(c.cycle_number));
          return (
            /*
             * Two charts stacked on a shared x axis rather than one chart with
             * two y axes. A dual axis lets the author place the crossings
             * wherever they like, so the reader sees a relationship that is an
             * artifact of the scaling.
             */
            <div className="grid" style={{ gap: 12 }}>
              <div>
                <h3 className="muted">Bankroll</h3>
                <LineChart
                  ariaLabel="Bankroll over cycles"
                  height={180}
                  categories={cats}
                  formatX={(c) => "C" + c}
                  formatY={(v) => money(v)}
                  series={[{ name: "Bankroll", color: "var(--series-1)", values: shown.map((c) => num(c.bankroll)) }]}
                />
              </div>
              <div>
                <h3 className="muted">Unrealized P&L</h3>
                <ColumnChart
                  ariaLabel="Unrealized profit and loss per cycle"
                  diverging
                  height={150}
                  categories={cats}
                  formatX={(c) => "C" + c}
                  formatY={(v) => money(v)}
                  series={[{ name: "Unrealized", color: "var(--series-1)", values: shown.map((c) => num(c.unrealized_pnl)) }]}
                />
              </div>
            </div>
          );
        }}
      </DatasetCard>

      <DatasetCard title="Scan funnel" label="Cycle history" dataset={cycles} datasetKey="cyclesAll">
        {(rows) => {
          const asc = cyclesAscending(rows);
          const shown = decimate(asc, (c) => c.markets_scanned, 120);
          if (shown.length === 0) return <EmptyState title="No cycles recorded yet" />;
          const cats = shown.map((c) => String(c.cycle_number));
          return (
            <>
              <p className="muted" style={{ marginTop: 0 }}>
                Three separate scales. These stages are nested subsets, not parts of a
                whole — and a scan of ~1,000 markets against 0–3 trades on one axis
                would flatten the two that matter into the baseline.
              </p>
              <div className="grid grid--3">
                {FUNNEL.map((f) => (
                  <ChartOrTable
                    key={f.key}
                    label={f.name}
                    chart={
                      <div>
                        <h3 className="muted">{f.name}</h3>
                        <ColumnChart
                          ariaLabel={`${f.name} per cycle`}
                          height={150}
                          categories={cats}
                          formatX={(c) => "C" + c}
                          formatY={(v) => int(v)}
                          series={[{ name: f.name, color: f.color, values: shown.map((c) => c[f.key]) }]}
                        />
                      </div>
                    }
                    table={
                      <DataTable
                        rows={[...asc].reverse().slice(0, 50)}
                        rowKey={(c) => String(c.id ?? c.cycle_number)}
                        columns={[
                          { key: "c", header: "Cycle", numeric: true, render: (c) => int(c.cycle_number) },
                          { key: "v", header: f.name, numeric: true, render: (c) => int(c[f.key]) },
                        ]}
                      />
                    }
                  />
                ))}
              </div>
            </>
          );
        }}
      </DatasetCard>

      <DatasetCard title="Cycle duration" label="Cycle history" dataset={cycles} datasetKey="cyclesAll">
        {(rows) => {
          const asc = cyclesAscending(rows);
          const shown = decimate(asc, (c) => c.duration_ms, 120);
          if (shown.length === 0) return <EmptyState title="No cycles recorded yet" />;
          return (
            <>
              {avgDuration !== null && (
                <p className="muted" style={{ marginTop: 0 }}>
                  Mean across all cycles: {durationMs(avgDuration)}
                </p>
              )}
              <ColumnChart
                ariaLabel="Cycle duration"
                categories={shown.map((c) => String(c.cycle_number))}
                formatX={(c) => "C" + c}
                formatY={(v) => durationMs(v)}
                series={[{ name: "Duration", color: "var(--series-1)", values: shown.map((c) => c.duration_ms) }]}
              />
            </>
          );
        }}
      </DatasetCard>

      <DatasetCard title="All cycles" label="Cycle history" dataset={cycles} datasetKey="cyclesAll">
        {(rows) =>
          rows.length === 0 ? (
            <EmptyState title="No cycles recorded yet" />
          ) : (
            <DataTable
              rows={rows}
              rowKey={(c, i) => String(c.id ?? i)}
              initialSort="cycle"
              columns={[
                { key: "cycle", header: "Cycle", numeric: true, render: (c) => int(c.cycle_number), sortValue: (c) => c.cycle_number },
                { key: "m", header: "Markets", numeric: true, render: (c) => int(c.markets_scanned), sortValue: (c) => c.markets_scanned },
                { key: "o", header: "Opps", numeric: true, render: (c) => int(c.opportunities_found), sortValue: (c) => c.opportunities_found },
                { key: "t", header: "Trades", numeric: true, render: (c) => int(c.trades_placed), sortValue: (c) => c.trades_placed },
                { key: "b", header: "Bankroll", numeric: true, render: (c) => money(num(c.bankroll)), sortValue: (c) => num(c.bankroll) },
                { key: "u", header: "Unrealized", numeric: true, render: (c) => moneySigned(num(c.unrealized_pnl)), sortValue: (c) => num(c.unrealized_pnl) },
                { key: "a", header: "API cost", numeric: true, render: (c) => money(num(c.api_cost)), sortValue: (c) => num(c.api_cost) },
                { key: "d", header: "Duration", numeric: true, render: (c) => durationMs(c.duration_ms), sortValue: (c) => c.duration_ms },
                { key: "s", header: "State", render: (c) => <Badge tone={agentStateTone(c.agent_state)}>{c.agent_state}</Badge>, sortValue: (c) => c.agent_state },
                {
                  key: "at",
                  header: "Finished",
                  render: (c) => relTime(parseTs(c.created_at), now),
                  title: (c) => absTime(parseTs(c.created_at)),
                  sortValue: (c) => parseTs(c.created_at)?.getTime() ?? null,
                },
              ]}
            />
          )
        }
      </DatasetCard>
    </div>
  );
}
