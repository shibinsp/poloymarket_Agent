import { useSyncExternalStore } from "react";
import { useDataset } from "../data/useDataset";
import { DatasetCard } from "../components/ui/DatasetCard";
import { Card } from "../components/ui/Card";
import { StatTile } from "../components/ui/StatTile";
import { EmptyState } from "../components/ui/States";
import { DataTable } from "../components/ui/DataTable";
import { ColumnChart } from "../components/charts/ColumnChart";
import { LineChart } from "../components/charts/LineChart";
import { HBarChart } from "../components/charts/HBarChart";
import { ChartOrTable } from "../components/charts/ChartOrTable";
import { costsByProvider, costsByUtcDay, cumulative } from "../data/derive";
import { num, sum } from "../lib/decimal";
import { absTime, compact, int, money, price, relTime, DASH } from "../lib/format";
import { parseTs, todayUtcStart } from "../lib/time";
import { settings } from "../data/settings";
import { useNow } from "../data/useNow";
import type { ApiCostRecord, Metrics } from "../api/types";

export function Costs() {
  const costs = useDataset<ApiCostRecord[]>("costs");
  const metrics = useDataset<Metrics>("metrics");
  const now = useNow();
  const cfg = useSyncExternalStore(settings.subscribe, settings.getSnapshot, settings.getSnapshot);

  const rows = costs.data ?? [];
  const dayStart = todayUtcStart().getTime();

  // Matches the agent's own get_today_api_cost, which filters on SQLite
  // date('now') — i.e. the UTC day, not the viewer's midnight.
  const today = sum(
    rows.filter((r) => (parseTs(r.created_at)?.getTime() ?? 0) >= dayStart).map((r) => r.cost),
  );
  const totalIn = sum(rows.map((r) => r.input_tokens));
  const totalOut = sum(rows.map((r) => r.output_tokens));
  const total = num(metrics.data?.total_api_cost) ?? sum(rows.map((r) => r.cost));

  const byDay = costsByUtcDay(rows);
  const byProvider = costsByProvider(rows);

  const perMillion =
    total !== null && totalIn !== null && totalOut !== null && totalIn + totalOut > 0
      ? (total / (totalIn + totalOut)) * 1_000_000
      : null;

  const budget = cfg.localDailyBudget;

  return (
    <div className="page">
      <Card title="API spend">
        <div className="grid grid--tiles">
          <StatTile hero label="Total spend" value={money(total)} sub={`${int(rows.length)} recorded calls`} />
          <StatTile
            label="Spend today"
            value={money(today)}
            sub={
              budget === null
                ? "UTC day, matching the agent"
                : `of ${money(budget)} local budget`
            }
          />
          <StatTile label="Input tokens" value={compact(totalIn)} />
          <StatTile label="Output tokens" value={compact(totalOut)} />
          <StatTile
            label="Cost per 1M tokens"
            value={perMillion === null ? DASH : money(perMillion)}
            sub="blended in + out"
          />
        </div>
        {budget === null && (
          <p className="muted" style={{ marginBottom: 0, marginTop: 10, fontSize: 12 }}>
            The agent's configured <code>daily_api_budget</code> is not exposed over
            the API, so no budget line is drawn here. You can set a local one in
            Settings; it is a display aid only and does not affect the agent.
          </p>
        )}
      </Card>

      <div className="grid grid--2">
        <DatasetCard title="Spend by provider" label="API costs" dataset={costs} datasetKey="costs">
          {() =>
            byProvider.length === 0 ? (
              <EmptyState title="No API calls recorded yet" />
            ) : (
              <HBarChart
                ariaLabel="API spend by provider"
                rows={byProvider}
                formatValue={(v) => money(v)}
              />
            )
          }
        </DatasetCard>

        <DatasetCard title="Spend per UTC day" label="API costs" dataset={costs} datasetKey="costs">
          {() =>
            byDay.length === 0 ? (
              <EmptyState title="No API calls recorded yet" />
            ) : (
              <ChartOrTable
                label="Daily spend"
                chart={
                  <ColumnChart
                    ariaLabel="API spend per UTC day"
                    categories={byDay.map((d) => d.day)}
                    formatX={(d) => d.slice(5)}
                    formatY={(v) => money(v)}
                    series={[{ name: "Spend", color: "var(--series-1)", values: byDay.map((d) => d.cost) }]}
                  />
                }
                table={
                  <DataTable
                    rows={[...byDay].reverse()}
                    rowKey={(d) => d.day}
                    columns={[
                      { key: "d", header: "UTC day", render: (d) => d.day },
                      { key: "c", header: "Spend", numeric: true, render: (d) => money(d.cost) },
                    ]}
                  />
                }
              />
            )
          }
        </DatasetCard>
      </div>

      {byDay.length > 1 && (
        <DatasetCard title="Cumulative spend" label="API costs" dataset={costs} datasetKey="costs">
          {() => (
            /* A separate chart rather than a second axis on the daily bars. */
            <LineChart
              ariaLabel="Cumulative API spend"
              includeZero
              categories={byDay.map((d) => d.day)}
              formatX={(d) => d.slice(5)}
              formatY={(v) => money(v)}
              series={[
                {
                  name: "Cumulative",
                  color: "var(--series-1)",
                  values: cumulative(byDay.map((d) => d.cost)),
                },
              ]}
            />
          )}
        </DatasetCard>
      )}

      {byDay.some((d) => d.inputTokens !== null || d.outputTokens !== null) && (
        <DatasetCard title="Tokens in and out" label="API costs" dataset={costs} datasetKey="costs">
          {() => (
            <ChartOrTable
              label="Token usage"
              chart={
                <ColumnChart
                  ariaLabel="Input and output tokens per UTC day"
                  categories={byDay.map((d) => d.day)}
                  formatX={(d) => d.slice(5)}
                  formatY={(v) => compact(v)}
                  series={[
                    { name: "Input", color: "var(--series-1)", values: byDay.map((d) => d.inputTokens) },
                    { name: "Output", color: "var(--series-2)", values: byDay.map((d) => d.outputTokens) },
                  ]}
                />
              }
              table={
                <DataTable
                  rows={[...byDay].reverse()}
                  rowKey={(d) => d.day}
                  columns={[
                    { key: "d", header: "UTC day", render: (d) => d.day },
                    { key: "i", header: "Input", numeric: true, render: (d) => compact(d.inputTokens) },
                    { key: "o", header: "Output", numeric: true, render: (d) => compact(d.outputTokens) },
                  ]}
                />
              }
            />
          )}
        </DatasetCard>
      )}

      <DatasetCard title="Call log" label="API costs" dataset={costs} datasetKey="costs">
        {(all) =>
          all.length === 0 ? (
            <EmptyState
              title="No API calls recorded yet"
              hint="A row is written each time the agent calls its valuation model."
            />
          ) : (
            <DataTable
              rows={all}
              rowKey={(r, i) => String(r.id ?? i)}
              initialSort="at"
              columns={[
                { key: "p", header: "Provider", render: (r) => r.provider, sortValue: (r) => r.provider },
                { key: "e", header: "Endpoint", truncate: true, render: (r) => r.endpoint ?? DASH, title: (r) => r.endpoint ?? undefined },
                { key: "i", header: "In", numeric: true, render: (r) => int(r.input_tokens), sortValue: (r) => r.input_tokens },
                { key: "o", header: "Out", numeric: true, render: (r) => int(r.output_tokens), sortValue: (r) => r.output_tokens },
                { key: "c", header: "Cost", numeric: true, render: (r) => price(num(r.cost)), sortValue: (r) => num(r.cost) },
                { key: "cy", header: "Cycle", numeric: true, render: (r) => int(r.cycle), sortValue: (r) => r.cycle },
                {
                  key: "at",
                  header: "When",
                  render: (r) => relTime(parseTs(r.created_at), now),
                  title: (r) => absTime(parseTs(r.created_at)),
                  sortValue: (r) => parseTs(r.created_at)?.getTime() ?? null,
                },
              ]}
            />
          )
        }
      </DatasetCard>
    </div>
  );
}
