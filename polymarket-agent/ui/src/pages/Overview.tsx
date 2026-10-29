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
import { int, money, moneyInr, pctFromFraction, price } from "../lib/format";
import { decimate } from "../lib/scale";
import { parseTs } from "../lib/time";
import type {
  CycleRecord,
  Health,
  IndiaSandboxStatusResponse,
  Metrics,
  RiskStatus,
  TradeRecord,
  UsPaperStatusResponse,
} from "../api/types";

export function Overview() {
  const metrics = useDataset<Metrics>("metrics");
  const cycles = useDataset<CycleRecord[]>("cyclesAll");
  const trades = useDataset<TradeRecord[]>("trades");
  const india = useDataset<IndiaSandboxStatusResponse>("indiaSandbox");
  const us = useDataset<UsPaperStatusResponse>("usPaper");
  const health = useDataset<Health>("health");
  const risk = useDataset<RiskStatus>("risk");

  const orderedCycles = cycles.data ? cyclesAscending(cycles.data) : [];
  const latestCycle = orderedCycles.at(-1) ?? null;
  const indiaEquity = num(india.data?.total_equity_inr);
  const usEquity = num(us.data?.total_equity_usd);
  const polymarketBankroll = num(latestCycle?.bankroll);
  const m = metrics.data;
  const halted = health.data?.halted === true || health.data?.halt != null;
  const agentState = health.data
    ? halted
      ? "HALTED"
      : health.data.agent_state || "UNKNOWN"
    : "CONNECTING";
  const netProfit = num(m?.net_profit);
  const drawdownLimit = num(risk.data?.limits.max_daily_loss_pct);
  const lastCycleAt = parseTs(health.data?.last_cycle_at);

  return (
    <div className="page overview">
      <header className="page__head overview__head">
        <div>
          <h1>Overview</h1>
          <p>Portfolio status and recent agent activity</p>
        </div>
        <div className="overview__status">
          <Badge
            tone={
              !health.data
                ? "neutral"
                : halted
                  ? "serious"
                  : agentState === "ALIVE"
                    ? "good"
                    : "warning"
            }
            title="Agent lifecycle state from the health endpoint"
          >
            {agentState}
          </Badge>
          <span className="muted">
            {lastCycleAt
              ? `Last cycle ${lastCycleAt.toLocaleString()}`
              : "Cycle activity will appear when health data is available"}
          </span>
        </div>
      </header>

      <section className="grid grid--3" aria-label="Portfolio balances">
        <Card title="India · NSE sandbox">
          <div className="overview__balance">{moneyInr(indiaEquity)}</div>
          <div className="overview__detail">
            {india.data
              ? `${int(india.data.recent_orders.length)} recent orders · ${india.data.currency}`
              : "Balance unavailable"}
          </div>
          <Link to="/india">Open India sandbox</Link>
        </Card>
        <Card title="United States · Alpaca paper">
          <div className="overview__balance">{money(usEquity)}</div>
          <div className="overview__detail">
            {us.data
              ? `${int(us.data.recent_orders.length)} recent orders · ${us.data.currency}`
              : "Balance unavailable"}
          </div>
          <Link to="/us">Open US paper account</Link>
        </Card>
        <Card title="Polymarket bankroll">
          <div className="overview__balance">{money(polymarketBankroll)}</div>
          <div className="overview__detail">
            {latestCycle
              ? `Cycle ${int(latestCycle.cycle_number)} · ${int(m?.open_trades)} open positions`
              : "No cycle balance recorded"}
          </div>
          <Link to="/trades">Review trade ledger</Link>
        </Card>
      </section>

      <Card title="Performance and risk">
        <div className="grid grid--tiles">
          <StatTile
            label="Win rate"
            value={m ? pctFromFraction(num(m.win_rate)) : "—"}
            sub={m ? `${int(m.resolved_trades)} resolved trades` : "Metrics unavailable"}
          />
          <StatTile
            label="Net profit"
            value={money(netProfit)}
            tone={toneOf(netProfit)}
            sub="Reported by the agent"
          />
          <StatTile
            label="Open positions"
            value={int(m?.open_trades)}
            sub="Polymarket trade ledger"
          />
          <StatTile
            label="Daily loss limit"
            value={pctFromFraction(drawdownLimit)}
            sub={
              risk.data
                ? `${money(num(risk.data.limits.max_daily_loss_usd))} maximum`
                : "Risk configuration unavailable"
            }
          />
        </div>
      </Card>

      <DatasetCard
        title="Polymarket bankroll by cycle"
        label="Recorded cycle balances"
        dataset={cycles}
        datasetKey="cyclesAll"
      >
        {(rows) => {
          const ascending = cyclesAscending(rows);
          const shown = decimate(ascending, (cycle) => num(cycle.bankroll) ?? 0, 120);
          if (shown.length === 0) {
            return <EmptyState title="No cycle balances recorded" />;
          }
          return (
            <ChartOrTable
              label="Bankroll"
              chart={
                <LineChart
                  ariaLabel="Polymarket bankroll by cycle"
                  categories={shown.map((cycle) => String(cycle.cycle_number))}
                  formatX={(cycle) => `C${cycle}`}
                  formatY={(value) => money(value)}
                  series={[
                    {
                      name: "Bankroll",
                      color: "var(--series-1)",
                      values: shown.map((cycle) => num(cycle.bankroll) ?? 0),
                    },
                  ]}
                />
              }
              table={
                <DataTable
                  rows={[...ascending].reverse().slice(0, 100)}
                  rowKey={(cycle) => String(cycle.id ?? cycle.cycle_number)}
                  columns={[
                    {
                      key: "cycle",
                      header: "Cycle",
                      numeric: true,
                      render: (cycle) => int(cycle.cycle_number),
                    },
                    {
                      key: "bankroll",
                      header: "Bankroll",
                      numeric: true,
                      render: (cycle) => money(num(cycle.bankroll)),
                    },
                  ]}
                />
              }
            />
          );
        }}
      </DatasetCard>

      <DatasetCard
        title="Trades placed per cycle"
        label="Cycle activity"
        dataset={cycles}
        datasetKey="cyclesAll"
      >
        {(rows) => {
          const ascending = cyclesAscending(rows);
          const shown = decimate(ascending, (cycle) => cycle.trades_placed, 120);
          if (shown.length === 0) {
            return <EmptyState title="No cycle activity recorded" />;
          }
          return (
            <ChartOrTable
              label="Trades placed"
              chart={
                <ColumnChart
                  ariaLabel="Trades placed per cycle"
                  categories={shown.map((cycle) => String(cycle.cycle_number))}
                  formatX={(cycle) => `C${cycle}`}
                  formatY={(value) => int(value)}
                  series={[
                    {
                      name: "Trades placed",
                      color: "var(--series-1)",
                      values: shown.map((cycle) => cycle.trades_placed),
                    },
                  ]}
                />
              }
              table={
                <DataTable
                  rows={[...ascending].reverse().slice(0, 100)}
                  rowKey={(cycle) => String(cycle.id ?? cycle.cycle_number)}
                  columns={[
                    {
                      key: "cycle",
                      header: "Cycle",
                      numeric: true,
                      render: (cycle) => int(cycle.cycle_number),
                    },
                    {
                      key: "trades",
                      header: "Trades",
                      numeric: true,
                      render: (cycle) => int(cycle.trades_placed),
                    },
                  ]}
                />
              }
            />
          );
        }}
      </DatasetCard>

      <DatasetCard
        title="Recent Polymarket trades"
        label="Trade ledger"
        dataset={trades}
        datasetKey="trades"
        actions={<Link to="/trades">View all trades</Link>}
      >
        {(rows) =>
          rows.length === 0 ? (
            <EmptyState
              title="No Polymarket trades recorded"
              hint="Trades will appear here after the agent records an execution."
            />
          ) : (
            <DataTable
              rows={tradesNewestFirst(rows).slice(0, 8)}
              rowKey={(trade, index) => String(trade.id ?? index)}
              columns={[
                {
                  key: "market",
                  header: "Market",
                  truncate: true,
                  render: (trade) => trade.market_question ?? trade.market_id,
                  title: (trade) => trade.market_question ?? trade.market_id,
                },
                { key: "side", header: "Side", render: (trade) => trade.direction },
                {
                  key: "price",
                  header: "Price",
                  numeric: true,
                  render: (trade) => price(num(trade.entry_price)),
                },
                {
                  key: "size",
                  header: "Size",
                  numeric: true,
                  render: (trade) => money(num(trade.size)),
                },
                {
                  key: "edge",
                  header: "Edge",
                  numeric: true,
                  render: (trade) => pctFromFraction(num(trade.edge_at_entry)),
                },
                {
                  key: "status",
                  header: "Status",
                  render: (trade) => (
                    <Badge tone={tradeStatusTone(trade.status)}>{trade.status}</Badge>
                  ),
                },
              ]}
            />
          )
        }
      </DatasetCard>
    </div>
  );
}
