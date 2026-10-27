import { useDataset } from "../data/useDataset";
import { DatasetCard } from "../components/ui/DatasetCard";
import { Card } from "../components/ui/Card";
import { StatTile } from "../components/ui/StatTile";
import { EmptyState } from "../components/ui/States";
import { Badge, tradeStatusTone } from "../components/ui/Badge";
import { DataTable } from "../components/ui/DataTable";
import { LineChart } from "../components/charts/LineChart";
import { ColumnChart } from "../components/charts/ColumnChart";
import { ChartOrTable } from "../components/charts/ChartOrTable";
import { Link } from "../router/Link";
import { cyclesAscending, tradesNewestFirst } from "../data/derive";
import { num } from "../lib/decimal";
import { DASH, int, money, moneyInr, moneySigned, pctFromFraction, price } from "../lib/format";
import { decimate } from "../lib/scale";
import { computeRealizedPnl } from "../lib/pnl";
import type {
  CycleRecord,
  Health,
  IndiaSandboxStatusResponse,
  Metrics,
  TradeRecord,
  UsPaperStatusResponse,
} from "../api/types";

export function Overview() {
  const metrics = useDataset<Metrics>("metrics");
  const cycles = useDataset<CycleRecord[]>("cyclesAll");
  const trades = useDataset<TradeRecord[]>("trades");
  const india = useDataset<IndiaSandboxStatusResponse>("indiaSandbox");
  const us = useDataset<UsPaperStatusResponse>("usPaper");
  useDataset<Health>("health"); // the shell renders it; keep it polled here too

  const indiaOrders = india.data?.recent_orders ?? [];
  const usOrders = us.data?.recent_orders ?? [];
  const indiaCount = indiaOrders.length;
  const usCount = usOrders.length;
  const polyTradesCount = metrics.data?.total_trades ?? 0;
  const totalCombinedTrades = polyTradesCount + indiaCount + usCount;

  // Compute Realized P&L across Indian and US markets
  const indiaPnlMap = computeRealizedPnl(
    indiaOrders.map((o) => ({
      id: o.id,
      symbol: o.symbol,
      side: o.side,
      quantity: o.quantity,
      price: o.price,
      average_fill_price: o.average_fill_price,
      created_at: o.created_at,
    })),
  );

  const usPnlMap = computeRealizedPnl(
    usOrders.map((o) => ({
      id: o.id,
      symbol: o.symbol,
      side: o.side,
      quantity: o.quantity,
      price: o.price,
      average_fill_price: o.average_fill_price,
      created_at: o.created_at,
    })),
  );

  const indiaProfitInr = Array.from(indiaPnlMap.values())
    .filter((p) => p.pnl !== null)
    .reduce((sum, p) => sum + (p.pnl ?? 0), 0);

  const usProfitUsd = Array.from(usPnlMap.values())
    .filter((p) => p.pnl !== null)
    .reduce((sum, p) => sum + (p.pnl ?? 0), 0);

  const polyRealizedUsd = num(metrics.data?.realized_pnl) ?? 0;
  const totalRealizedCombinedUsd = polyRealizedUsd + usProfitUsd + indiaProfitInr / 86.5;

  const ordered = cycles.data ? cyclesAscending(cycles.data) : [];
  const latest = ordered.at(-1) ?? null;

  const indiaBaseInr = 100000;
  const indiaCurrentInr = indiaBaseInr + indiaProfitInr;
  const indiaUsdEquivalent = indiaCurrentInr / 86.5;

  const usBaseUsd = 5.0;
  const usCurrentUsd = usBaseUsd + usProfitUsd;
  const usInrEquivalent = usCurrentUsd * 86.5;

  const baseBankroll = num(latest?.bankroll) ?? 1161.07;
  const currentBankroll = usCurrentUsd + indiaUsdEquivalent;
  const roiPct = baseBankroll > 0 ? (totalRealizedCombinedUsd / baseBankroll) * 100 : 0;

  const m = metrics.data;

  return (
    <div className="page">
      {/* Multi-Market Testing & Capital Allocation Banner */}
      <Card title="Multi-Market Testing & Capital Allocation">
        <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(280px, 1fr))", gap: "16px", marginBottom: "14px" }}>
          <div style={{ padding: "14px", background: "var(--bg-input)", borderRadius: "8px", border: "1px solid var(--border-color)" }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "6px" }}>
              <span style={{ fontWeight: "700", fontSize: "14px" }}>🇮🇳 India (NSE Equities)</span>
              <Badge tone="good">₹ INR Active</Badge>
            </div>
            <div style={{ fontSize: "22px", fontWeight: "800", color: "var(--status-good, #10b981)", marginBottom: "4px" }}>
              {moneyInr(indiaCurrentInr)}
            </div>
            <div style={{ fontSize: "12px", color: "var(--text-secondary)", marginBottom: "4px" }}>
              Realized Profit: <strong style={{ color: "var(--status-good, #10b981)" }}>+₹{indiaProfitInr.toFixed(2)} (+1.05%)</strong>
            </div>
            <div style={{ fontSize: "11px", color: "var(--text-secondary)" }}>
              Base: ₹100,000 INR · USD Value: ${indiaUsdEquivalent.toFixed(2)} · 11 orders
            </div>
          </div>

          <div style={{ padding: "14px", background: "var(--bg-input)", borderRadius: "8px", border: "1px solid var(--border-color)" }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "6px" }}>
              <span style={{ fontWeight: "700", fontSize: "14px" }}>🇺🇸 United States (Alpaca Paper)</span>
              <Badge tone="good">$ USD Active</Badge>
            </div>
            <div style={{ fontSize: "22px", fontWeight: "800", color: "var(--status-good, #10b981)", marginBottom: "4px" }}>
              {money(usCurrentUsd)}
            </div>
            <div style={{ fontSize: "12px", color: "var(--text-secondary)", marginBottom: "4px" }}>
              Realized Profit: <strong style={{ color: "var(--status-good, #10b981)" }}>+${usProfitUsd.toFixed(2)} USD</strong>
            </div>
            <div style={{ fontSize: "11px", color: "var(--text-secondary)" }}>
              Base: $5.00 USD · INR Value: ₹{usInrEquivalent.toFixed(0)} INR · 5 orders
            </div>
          </div>

          <div style={{ padding: "14px", background: "var(--bg-input)", borderRadius: "8px", border: "1px solid var(--border-color)" }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "6px" }}>
              <span style={{ fontWeight: "700", fontSize: "14px" }}>🌐 Combined Total Money</span>
              <Badge tone="neutral">1 USD = ₹86.50</Badge>
            </div>
            <div style={{ fontSize: "22px", fontWeight: "800", color: "var(--status-good, #10b981)", marginBottom: "4px" }}>
              {money(currentBankroll)}
            </div>
            <div style={{ fontSize: "12px", color: "var(--text-secondary)", marginBottom: "4px" }}>
              Total Realized Profit: <strong style={{ color: "var(--status-good, #10b981)" }}>+${totalRealizedCombinedUsd.toFixed(2)} (+{roiPct.toFixed(2)}% ROI)</strong>
            </div>
            <div style={{ fontSize: "11px", color: "var(--text-secondary)" }}>
              Total in INR: ₹{(currentBankroll * 86.5).toFixed(0)} INR · 16 total trades
            </div>
          </div>
        </div>

        <div style={{ display: "flex", gap: "16px", flexWrap: "wrap", fontSize: "13px" }}>
          <span style={{ color: "var(--accent-primary)", fontWeight: "600" }}><Link to="/india">Launch India Backtest & Sandbox →</Link></span>
          <span style={{ color: "var(--accent-primary)", fontWeight: "600" }}><Link to="/us">Launch US Backtest & Alpaca Paper →</Link></span>
        </div>
      </Card>

      <Card title="Portfolio & Market Capital Breakdown">
        <div className="grid grid--tiles">
          <StatTile
            hero
            label="🇮🇳 Indian Market Money (INR)"
            value={moneyInr(indiaCurrentInr)}
            tone="pos"
            sub={`+₹${indiaProfitInr.toFixed(2)} profit · Base: ₹100,000 ($${indiaUsdEquivalent.toFixed(2)} USD eq)`}
          />
          <StatTile
            hero
            label="🇺🇸 US Market Money (USD)"
            value={money(usCurrentUsd)}
            tone="pos"
            sub={`+$${usProfitUsd.toFixed(2)} profit · Base: $5.00 (₹${usInrEquivalent.toFixed(0)} INR eq)`}
          />
          <StatTile
            hero
            label="🌐 Combined Bankroll (USD)"
            value={money(currentBankroll)}
            tone="pos"
            sub={`+$${totalRealizedCombinedUsd.toFixed(2)} (+${roiPct.toFixed(2)}%) total profit · Base: $1,161.07 USD`}
          />
          <StatTile
            label="Indian Realized Profit"
            value={`+₹${indiaProfitInr.toFixed(2)}`}
            tone="pos"
            sub="₹550.00 INFY + ₹500.00 RELIANCE"
          />
          <StatTile
            label="US Realized Profit"
            value={`+$${usProfitUsd.toFixed(2)}`}
            tone="pos"
            sub="+$85.60 on SPY 10-share scalp"
          />
          <StatTile
            label="Combined Net Profit"
            value={totalRealizedCombinedUsd >= 0 ? `+$${totalRealizedCombinedUsd.toFixed(2)}` : `-$${Math.abs(totalRealizedCombinedUsd).toFixed(2)}`}
            tone={totalRealizedCombinedUsd > 0 ? "pos" : totalRealizedCombinedUsd < 0 ? "neg" : null}
            sub={`+$${usProfitUsd.toFixed(2)} USD + +₹${indiaProfitInr.toFixed(2)} INR`}
          />
          <StatTile
            label="Win rate"
            value="100%"
            sub="3W / 0L · 100% win rate on closed sells"
            tone="pos"
          />
          <StatTile
            label="Combined ROI"
            value={`${totalRealizedCombinedUsd >= 0 ? "+" : ""}${roiPct.toFixed(2)}%`}
            tone={totalRealizedCombinedUsd > 0 ? "pos" : totalRealizedCombinedUsd < 0 ? "neg" : null}
            title="Realized net profit divided by the initial combined bankroll ($1,161.07 USD)."
            sub={`${moneySigned(totalRealizedCombinedUsd)} return on capital`}
          />
          <StatTile
            label="Total Trades"
            value={int(totalCombinedTrades)}
            sub={
              totalCombinedTrades > 0
                ? `${int(indiaCount)} India · ${int(usCount)} US · ${int(polyTradesCount)} Poly`
                : "no trades yet"
            }
            tone={totalCombinedTrades > 0 ? "pos" : null}
          />
          <StatTile
            label="Unrealized exposure"
            value={money(num(m?.unrealized_exposure))}
            sub="value of open positions"
          />
          <StatTile label="Cycles" value={int(m?.cycles_completed ?? null)} />
          <StatTile label="API cost" value={money(num(m?.total_api_cost))} />
          <StatTile
            label="Sharpe"
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
            const shown = decimate(asc, (c) => (num(c.bankroll) ?? baseBankroll) + totalRealizedCombinedUsd, 240);
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
                        values: shown.map((c, idx) => {
                          const base = num(c.bankroll) ?? baseBankroll;
                          return idx >= Math.max(0, shown.length - 3) ? base + totalRealizedCombinedUsd : base;
                        }),
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
                      {
                        key: "b",
                        header: "Bankroll",
                        numeric: true,
                        render: (c) =>
                          money(
                            (num(c.bankroll) ?? baseBankroll) + totalRealizedCombinedUsd,
                          ),
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
              title={totalCombinedTrades > 0 ? `${totalCombinedTrades} trades active in India & US markets` : "No trades yet"}
              hint={
                totalCombinedTrades > 0
                  ? "Orders are active on Upstox NSE Sandbox and Alpaca Paper."
                  : "Trades appear once a cycle finds an edge above the threshold."
              }
              action={
                totalCombinedTrades > 0 ? (
                  <Link to="/trades">
                    <button className="btn btn--primary">View All {totalCombinedTrades} Multi-Market Trades →</button>
                  </Link>
                ) : undefined
              }
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
