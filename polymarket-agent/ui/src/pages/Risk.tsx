import { useMemo } from "react";
import { useDataset } from "../data/useDataset";
import { DatasetCard } from "../components/ui/DatasetCard";
import { Card } from "../components/ui/Card";
import { StatTile } from "../components/ui/StatTile";
import { EmptyState } from "../components/ui/States";
import { Badge, type StatusTone } from "../components/ui/Badge";
import { DataTable } from "../components/ui/DataTable";
import { LineChart } from "../components/charts/LineChart";
import { ChartOrTable } from "../components/charts/ChartOrTable";
import { HaltControl } from "../components/shell/HaltControl";
import { dataStore } from "../data/store";
import { num } from "../lib/decimal";
import { absTime, int, money, pctFromFraction, relTime, DASH } from "../lib/format";
import { parseTs } from "../lib/time";
import { useNow } from "../data/useNow";
import type {
  DailyEquity,
  Health,
  ReconciliationRun,
  RiskStatus,
} from "../api/types";

/**
 * How close the agent is to each of its limits, and whether the ledger still
 * matches the venues.
 *
 * The limits themselves live in a config file and the trips land in the logs;
 * neither answers the question an operator actually has, which is *how close
 * am I*. A breaker you only learn about by tripping it is a breaker you set
 * badly and never corrected.
 */
export function Risk() {
  const risk = useDataset<RiskStatus>("risk");
  const equity = useDataset<DailyEquity[]>("equity");
  const recon = useDataset<ReconciliationRun[]>("reconciliation");
  const health = useDataset<Health>("health");
  const now = useNow();

  const h = health.data;

  return (
    <div className="page">
      <Card title="Trading">
        {h && "halted" in h ? (
          <HaltControl
            halted={h.halted === true}
            current={h.halt}
            onChanged={() => {
              dataStore.refresh("health");
              dataStore.refresh("risk");
            }}
          />
        ) : (
          <p className="muted" style={{ margin: 0 }}>
            This agent build does not report a halt state.
          </p>
        )}
      </Card>

      <DatasetCard title="Circuit breakers" label="Risk limits" dataset={risk} datasetKey="risk">
        {(r) => {
          const rows = breakerRows(r);
          return (
            <>
              <div className="grid grid--tiles">
                <StatTile
                  label="Day (UTC)"
                  value={r.day}
                  sub={`mode: ${r.mode}`}
                />
                <StatTile
                  label="Opening equity"
                  value={money(num(r.starting_equity))}
                  sub={r.starting_equity === null ? "not recorded yet today" : undefined}
                />
                <StatTile
                  label="Current equity"
                  value={money(num(r.current_equity))}
                />
                <StatTile
                  label="All-time high"
                  value={money(num(r.high_water_mark))}
                  sub="what drawdown is measured from"
                />
              </div>

              <DataTable
                rows={rows}
                rowKey={(b) => b.name}
                columns={[
                  { key: "n", header: "Limit", render: (b) => b.name },
                  { key: "now", header: "Now", numeric: true, render: (b) => b.current },
                  { key: "max", header: "Limit", numeric: true, render: (b) => b.limit },
                  {
                    key: "use",
                    header: "Used",
                    numeric: true,
                    render: (b) =>
                      b.used === null ? DASH : <UsageBar fraction={b.used} />,
                    sortValue: (b) => b.used,
                  },
                  {
                    key: "scope",
                    header: "If tripped",
                    render: (b) => <span className="muted">{b.scope}</span>,
                  },
                ]}
              />

              {!r.limits.live_caps_apply && (
                <p className="muted" style={{ fontSize: 12, marginBottom: 0 }}>
                  The two absolute cash caps ($
                  {r.limits.max_live_notional_per_position_usd} per position, $
                  {r.limits.max_live_total_notional_usd} total) are listed for
                  reference — they bind only in live mode. A percentage of a
                  paper balance is a number nobody agreed to.
                </p>
              )}
            </>
          );
        }}
      </DatasetCard>

      <DatasetCard title="Equity and drawdown" label="Daily equity" dataset={equity} datasetKey="equity">
        {(rows) => <EquitySection rows={rows} />}
      </DatasetCard>

      <DatasetCard
        title="Reconciliation"
        label="Reconciliation"
        dataset={recon}
        datasetKey="reconciliation"
      >
        {(rows) =>
          rows.length === 0 ? (
            <EmptyState
              title="No reconciliation runs yet"
              hint="Each cycle compares the ledger against every configured venue. With no venues configured there is nothing to compare."
            />
          ) : (
            <>
              <p className="muted" style={{ marginTop: 0 }}>
                A mismatch halts the agent until acknowledged. A venue that
                cannot be <em>asked</em> does not halt it — a venue that cannot
                answer cannot accept orders either.
              </p>
              <DataTable
                rows={rows}
                rowKey={(r) => String(r.id)}
                columns={[
                  {
                    key: "ok",
                    header: "Result",
                    render: (r) => (
                      <Badge tone={r.passed ? "good" : "critical"}>
                        {r.passed ? "clean" : "mismatch"}
                      </Badge>
                    ),
                    sortValue: (r) => (r.passed ? 1 : 0),
                  },
                  { key: "venue", header: "Venue", render: (r) => r.venue_id },
                  {
                    key: "ml",
                    header: "At venue only",
                    numeric: true,
                    render: (r) => int(r.positions_missing_locally),
                    sortValue: (r) => r.positions_missing_locally,
                  },
                  {
                    key: "mv",
                    header: "Local only",
                    numeric: true,
                    render: (r) => int(r.positions_missing_on_venue),
                    sortValue: (r) => r.positions_missing_on_venue,
                  },
                  {
                    key: "qm",
                    header: "Qty mismatch",
                    numeric: true,
                    render: (r) => int(r.qty_mismatches),
                    sortValue: (r) => r.qty_mismatches,
                  },
                  {
                    key: "uo",
                    header: "Unknown orders",
                    numeric: true,
                    render: (r) => int(r.unknown_open_orders),
                    sortValue: (r) => r.unknown_open_orders,
                  },
                  {
                    key: "when",
                    header: "When",
                    render: (r) => {
                      const t = parseTs(r.created_at);
                      return t ? relTime(t, now) : DASH;
                    },
                    sortValue: (r) => parseTs(r.created_at)?.getTime() ?? null,
                    title: (r) => absTime(parseTs(r.created_at)) || undefined,
                  },
                  {
                    key: "detail",
                    header: "Detail",
                    truncate: true,
                    render: (r) => r.detail ?? DASH,
                    title: (r) => r.detail ?? undefined,
                  },
                ]}
              />
            </>
          )
        }
      </DatasetCard>
    </div>
  );
}

function EquitySection({ rows }: { rows: DailyEquity[] }) {
  const series = useMemo(() => {
    // Peak-to-trough, walked forward. The stored high-water mark is already
    // cumulative, but recomputing it here means the chart and the number
    // agree even against an older agent that never wrote one.
    let peak = -Infinity;
    return rows.map((r) => {
      const close = num(r.closing_equity) ?? num(r.starting_equity) ?? 0;
      peak = Math.max(peak, close);
      return {
        day: r.day,
        equity: close,
        peak,
        drawdown: peak > 0 ? (peak - close) / peak : 0,
      };
    });
  }, [rows]);

  if (series.length === 0) {
    return (
      <EmptyState
        title="No equity recorded yet"
        hint="One row is written per UTC day, on the first cycle of that day."
      />
    );
  }

  const worst = series.reduce((a, b) => (b.drawdown > a.drawdown ? b : a));
  const latest = series[series.length - 1];

  return (
    <>
      <div className="grid grid--tiles">
        <StatTile label="Latest equity" value={money(latest.equity)} />
        <StatTile
          label="Current drawdown"
          value={pctFromFraction(latest.drawdown, 2)}
          tone={latest.drawdown > 0 ? "neg" : null}
        />
        <StatTile
          label="Worst drawdown"
          value={pctFromFraction(worst.drawdown, 2)}
          sub={`on ${worst.day}`}
          tone={worst.drawdown > 0 ? "neg" : null}
        />
        <StatTile label="Days recorded" value={int(series.length)} />
      </div>
      <ChartOrTable
        label="Equity against its running peak"
        chart={
          <LineChart
            ariaLabel="Account equity against its all-time high"
            categories={series.map((s) => s.day)}
            formatX={(c) => c}
            formatY={(v) => money(v)}
            series={[
              {
                name: "Equity",
                color: "var(--series-1)",
                values: series.map((s) => s.equity),
              },
              // The peak is drawn alongside rather than as a drawdown
              // percentage: the gap between the two lines *is* the drawdown,
              // and it reads as a distance rather than a number to interpret.
              {
                name: "Peak",
                color: "var(--series-2)",
                values: series.map((s) => s.peak),
              },
            ]}
          />
        }
        table={
          <DataTable
            rows={series}
            rowKey={(r) => r.day}
            columns={[
              { key: "d", header: "Day", render: (r) => r.day },
              { key: "e", header: "Equity", numeric: true, render: (r) => money(r.equity) },
              { key: "p", header: "Peak", numeric: true, render: (r) => money(r.peak) },
              {
                key: "dd",
                header: "Drawdown",
                numeric: true,
                render: (r) => pctFromFraction(r.drawdown, 2),
              },
            ]}
          />
        }
      />
    </>
  );
}

/** A limit's usage, as a bar plus the number. Colour never carries it alone. */
function UsageBar({ fraction }: { fraction: number }) {
  const pct = Math.max(0, Math.min(1, fraction));
  const tone: StatusTone =
    pct >= 1 ? "critical" : pct >= 0.75 ? "serious" : pct >= 0.5 ? "warning" : "good";
  return (
    <span className="usage" title={`${(fraction * 100).toFixed(0)}% of the limit`}>
      <span className={`usage__bar usage__bar--${tone}`} style={{ width: `${pct * 100}%` }} />
      <span className="usage__label">{(fraction * 100).toFixed(0)}%</span>
    </span>
  );
}

interface BreakerRow {
  name: string;
  current: string;
  limit: string;
  /** Fraction of the limit consumed, or null when it cannot be computed yet. */
  used: number | null;
  scope: string;
}

function breakerRows(r: RiskStatus): BreakerRow[] {
  const start = num(r.starting_equity);
  const current = num(r.current_equity);
  const peak = num(r.high_water_mark);
  const lost = start !== null && current !== null ? start - current : null;

  const lossPct = lost !== null && start !== null && start > 0 ? lost / start : null;
  const drawdown = current !== null && peak !== null && peak > 0 ? (peak - current) / peak : null;

  const maxLossPct = num(r.limits.max_daily_loss_pct);
  const maxLossUsd = num(r.limits.max_daily_loss_usd);
  const maxDd = num(r.limits.max_drawdown_pct);

  const rows: BreakerRow[] = [
    {
      name: "Daily loss",
      current: lossPct === null ? DASH : pctFromFraction(Math.max(lossPct, 0), 2),
      limit: pctFromFraction(maxLossPct ?? 0, 2),
      used: lossPct !== null && maxLossPct ? Math.max(lossPct, 0) / maxLossPct : null,
      scope: "stops entries for the rest of the UTC day",
    },
    {
      name: "Daily loss (cash)",
      current: lost === null ? DASH : money(Math.max(lost, 0)),
      limit: money(maxLossUsd),
      used: lost !== null && maxLossUsd ? Math.max(lost, 0) / maxLossUsd : null,
      scope: "stops entries for the rest of the UTC day",
    },
    {
      name: "Drawdown from peak",
      current: drawdown === null ? DASH : pctFromFraction(Math.max(drawdown, 0), 2),
      limit: pctFromFraction(maxDd ?? 0, 2),
      used: drawdown !== null && maxDd ? Math.max(drawdown, 0) / maxDd : null,
      scope: "stops entries until a human resumes",
    },
    {
      name: "Trades today",
      current: int(r.trades_today),
      limit: int(r.limits.max_trades_per_day),
      used:
        r.limits.max_trades_per_day > 0
          ? r.trades_today / r.limits.max_trades_per_day
          : null,
      scope: "stops entries for the rest of the UTC day",
    },
    {
      name: "Losing streak",
      current: int(r.consecutive_losses),
      limit: int(r.limits.max_consecutive_losses),
      used:
        r.limits.max_consecutive_losses > 0
          ? r.consecutive_losses / r.limits.max_consecutive_losses
          : null,
      scope: "stops entries for the rest of the UTC day",
    },
  ];

  if (r.limits.live_caps_apply) {
    rows.push(
      {
        name: "Per-position notional",
        current: DASH,
        limit: money(num(r.limits.max_live_notional_per_position_usd)),
        used: null,
        scope: "caps sizing; never halts",
      },
      {
        name: "Total notional",
        current: DASH,
        limit: money(num(r.limits.max_live_total_notional_usd)),
        used: null,
        scope: "caps sizing; never halts",
      },
    );
  }

  return rows;
}
