import { useEffect, useState } from "react";
import { Card } from "../ui/Card";
import { StatTile, toneOf } from "../ui/StatTile";
import { DataTable } from "../ui/DataTable";
import { EmptyState } from "../ui/States";
import { num } from "../../lib/decimal";
import { etTime, inr, inrSigned, istTime, money, moneySigned, price } from "../../lib/format";
import { parseTs } from "../../lib/time";

/**
 * The two strategies the agent actually trades, read from their own ledgers.
 * The sandbox and simulator books elsewhere on these pages are older and
 * separate; these figures are the ones that match the broker and the reports.
 */

/** Fetch a JSON endpoint now and every 30 seconds, keeping the last good value. */
export function usePolled<T>(path: string): T | null {
  const [data, setData] = useState<T | null>(null);
  useEffect(() => {
    let active = true;
    async function load() {
      try {
        const res = await fetch(path);
        if (res.ok && active) setData(await res.json());
      } catch {
        // keep the last good value
      }
    }
    load();
    const timer = setInterval(load, 30000);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, [path]);
  return data;
}

interface IndiaTrade {
  day: string;
  symbol: string;
  side: string;
  qty: number;
  entry_price: string;
  stop_price: string;
  entry_at: string;
  exit_price: string | null;
  exit_reason: string | null;
  charges: string | null;
  pnl: string | null;
}

export interface IndiaIntraday {
  capital_inr: string | number | null;
  trades_today: number;
  pnl_today_inr: string | number;
  pnl_total_inr: string | number;
  trades: IndiaTrade[];
}

export interface UsOrbTrade {
  id: number;
  symbol: string | null;
  side: string | null;
  qty: string | null;
  entry_price: string | null;
  stop_price: string | null;
  mark_price: string | null;
  realized_pnl: string | null;
  unrealized_pnl: string | null;
  close_reason: string | null;
  status: string | null;
  entry_at: string | null;
  closed_at: string | null;
}

export interface RiskSnapshot {
  current_equity: string | null;
  starting_equity: string | null;
  limits?: {
    max_daily_loss_pct?: string | null;
    max_daily_loss_usd?: string | null;
    max_drawdown_pct?: string | null;
  };
}

interface DeskCard {
  at: string;
  symbol: string;
  vega?: { side?: string; stop?: number };
  atlas?: { relative_volume?: number };
  titan?: { reason?: string };
  nova?: { action?: string; reasons?: string[] };
}

const EXIT_LABEL: Record<string, string> = {
  FLAT_1515: "Sold at 15:15",
  SESSION_CLOSE: "Sold before the close",
  STOP: "Stop hit",
  STOP_LOSS: "Stop hit",
};
const exitLabel = (r: string | null) => (r ? EXIT_LABEL[r] ?? r : "Open");

/** A link that opens outside the dashboard, in a new tab. */
function External({ href, children }: { href: string; children: string }) {
  return (
    <a href={href} target="_blank" rel="noopener noreferrer">
      {children}
    </a>
  );
}

const nseChart = (symbol: string) =>
  `https://www.tradingview.com/chart/?symbol=${encodeURIComponent(`NSE:${symbol}`)}`;
const usChart = (symbol: string) =>
  `https://www.tradingview.com/chart/?symbol=${encodeURIComponent(symbol)}`;

function DeskTable({
  cards,
  time,
  chart,
}: {
  cards: DeskCard[];
  time: (d: Date | null) => string;
  chart: (symbol: string) => string;
}) {
  return (
    <DataTable
      rows={cards}
      rowKey={(c, i) => `${c.at}-${c.symbol}-${i}`}
      empty={<EmptyState title="No desk decisions yet" />}
      columns={[
        { key: "t", header: "Time", render: (c) => time(parseTs(c.at)) },
        { key: "s", header: "Stock", render: (c) => <External href={chart(c.symbol)}>{`${c.symbol} ↗`}</External> },
        { key: "d", header: "Side", render: (c) => c.vega?.side ?? "—" },
        { key: "v", header: "Volume vs normal", numeric: true, render: (c) => (c.atlas?.relative_volume ? `${c.atlas.relative_volume.toFixed(2)}x` : "—") },
        { key: "a", header: "Decision", render: (c) => c.nova?.action ?? "—" },
        { key: "r", header: "Why", truncate: true, render: (c) => (c.nova?.reasons?.length ? c.nova.reasons.join("; ") : c.titan?.reason ?? "—") },
      ]}
    />
  );
}

export function IndiaOrbLedger() {
  const data = usePolled<IndiaIntraday>("/api/india/intraday");
  const desk = usePolled<{ cards: DeskCard[] }>("/api/india/desk");
  const capital = num(data?.capital_inr ?? null);
  const total = num(data?.pnl_total_inr ?? null) ?? 0;
  const trades = data?.trades ?? [];
  return (
    <Card
      title="🇮🇳 India opening-range breakout — paper ledger (real figures)"
      actions={<External href="https://pro.upstox.com/">View live market on Upstox ↗</External>}
    >
      <div className="grid grid--tiles">
        <StatTile label="Capital" value={capital === null ? "—" : inr(capital)} sub="NIFTY 50 · no leverage · whole shares" />
        <StatTile label="Now" value={capital === null ? "—" : inr(capital + total)} sub="Capital plus closed P&L after charges" />
        <StatTile label="Closed P&L (all time)" value={inrSigned(total)} tone={toneOf(total)} sub="After brokerage, STT and fees" />
        <StatTile label="Today" value={inrSigned(data?.pnl_today_inr ?? 0)} tone={toneOf(num(data?.pnl_today_inr ?? null))} sub={`${data?.trades_today ?? 0} trade(s) today`} />
      </div>
      <h3 style={{ margin: "16px 0 8px", fontSize: "14px" }}>Trades</h3>
      <DataTable
        rows={trades}
        rowKey={(t, i) => `${t.entry_at}-${t.symbol}-${i}`}
        empty={<EmptyState title="No India trades yet" />}
        columns={[
          { key: "d", header: "Bought / shorted", render: (t) => istTime(parseTs(t.entry_at)) },
          { key: "s", header: "Stock", render: (t) => <External href={nseChart(t.symbol)}>{`${t.symbol} ↗`}</External> },
          { key: "side", header: "Side", render: (t) => t.side },
          { key: "q", header: "Shares", numeric: true, render: (t) => String(t.qty) },
          { key: "e", header: "Entry", numeric: true, render: (t) => inr(t.entry_price) },
          { key: "st", header: "Stop", numeric: true, render: (t) => inr(t.stop_price) },
          { key: "x", header: "Exit", numeric: true, render: (t) => (t.exit_price ? inr(t.exit_price) : "Open") },
          { key: "w", header: "Why it closed", render: (t) => exitLabel(t.exit_reason) },
          { key: "c", header: "Charges", numeric: true, render: (t) => (t.charges ? inr(t.charges) : "—") },
          { key: "p", header: "P&L", numeric: true, render: (t) => (t.pnl ? inrSigned(t.pnl) : "—") },
        ]}
      />
      <h3 style={{ margin: "16px 0 8px", fontSize: "14px" }}>Desk decisions</h3>
      <DeskTable cards={desk?.cards ?? []} time={istTime} chart={nseChart} />
    </Card>
  );
}

export function UsOrbLedger() {
  const data = usePolled<{ trades: UsOrbTrade[] }>("/api/us/orb/trades");
  const risk = usePolled<RiskSnapshot>("/api/risk");
  const status = usePolled<{ us?: { capital_allocation_usd?: string | number } }>("/api/intraday/status");
  const desk = usePolled<{ cards: DeskCard[] }>("/api/us/desk");
  const trades = data?.trades ?? [];
  const closed = trades.reduce((sum, t) => sum + (num(t.realized_pnl) ?? 0), 0);
  const open = trades.filter((t) => t.status === "OPEN").reduce((sum, t) => sum + (num(t.unrealized_pnl) ?? 0), 0);
  const equity = num(risk?.current_equity ?? null);
  const capital = num(status?.us?.capital_allocation_usd ?? null);
  return (
    <Card
      title="🇺🇸 US opening-range breakout — Alpaca paper (real figures)"
      actions={<External href="https://app.alpaca.markets/paper/dashboard/overview">View the Alpaca paper account ↗</External>}
    >
      <div className="grid grid--tiles">
        <StatTile label="Alpaca paper account" value={equity === null ? "—" : money(equity)} sub="Live equity as Alpaca reports it" />
        <StatTile label="Strategy capital" value={capital === null ? "—" : money(capital)} sub="Equal dollars per trade, at most 3 a day" />
        <StatTile label="Closed P&L (this strategy)" value={moneySigned(closed)} tone={toneOf(closed)} sub={`${trades.filter((t) => t.status === "CLOSED").length} closed trade(s)`} />
        <StatTile label="Open P&L" value={moneySigned(open)} tone={toneOf(open)} sub={`${trades.filter((t) => t.status === "OPEN").length} open position(s)`} />
      </div>
      <h3 style={{ margin: "16px 0 8px", fontSize: "14px" }}>Trades</h3>
      <DataTable
        rows={trades}
        rowKey={(t) => String(t.id)}
        empty={<EmptyState title="No US breakout trades yet" />}
        columns={[
          { key: "d", header: "Entered", render: (t) => etTime(parseTs(t.entry_at)) },
          { key: "s", header: "Stock", render: (t) => (t.symbol ? <External href={usChart(t.symbol)}>{`${t.symbol} ↗`}</External> : "—") },
          { key: "side", header: "Side", render: (t) => (t.side === "SELL" ? "SHORT" : "LONG") },
          { key: "q", header: "Shares", numeric: true, render: (t) => t.qty ?? "—" },
          { key: "e", header: "Entry", numeric: true, render: (t) => price(num(t.entry_price)) },
          { key: "st", header: "Stop", numeric: true, render: (t) => price(num(t.stop_price)) },
          { key: "x", header: "Exit", numeric: true, render: (t) => (t.status === "CLOSED" ? price(num(t.mark_price)) : "Open") },
          { key: "w", header: "Why it closed", render: (t) => (t.status === "CLOSED" ? exitLabel(t.close_reason) : t.status ?? "—") },
          { key: "p", header: "P&L", numeric: true, render: (t) => moneySigned(num(t.status === "CLOSED" ? t.realized_pnl : t.unrealized_pnl)) },
        ]}
      />
      <h3 style={{ margin: "16px 0 8px", fontSize: "14px" }}>Desk decisions</h3>
      <DeskTable cards={desk?.cards ?? []} time={etTime} chart={usChart} />
    </Card>
  );
}

interface RecentRow {
  key: string;
  at: Date | null;
  market: "India" | "US";
  symbol: string;
  side: string;
  qty: string;
  entry: string;
  exit: string;
  result: string;
  resultValue: number | null;
  status: string;
}

/** The latest trades from both strategies, newest first. */
export function RecentStrategyTrades({ limit = 8 }: { limit?: number }) {
  const india = usePolled<IndiaIntraday>("/api/india/intraday");
  const us = usePolled<{ trades: UsOrbTrade[] }>("/api/us/orb/trades");
  const rows: RecentRow[] = [
    ...(india?.trades ?? []).map((t, i) => ({
      key: `in-${i}-${t.entry_at}`,
      at: parseTs(t.entry_at),
      market: "India" as const,
      symbol: t.symbol,
      side: t.side,
      qty: String(t.qty),
      entry: inr(t.entry_price),
      exit: t.exit_price ? inr(t.exit_price) : "Open",
      result: t.pnl ? inrSigned(t.pnl) : "—",
      resultValue: num(t.pnl),
      status: t.exit_price ? "CLOSED" : "OPEN",
    })),
    ...(us?.trades ?? []).map((t) => {
      const closed = t.status === "CLOSED";
      const pnl = num(closed ? t.realized_pnl : t.unrealized_pnl);
      return {
        key: `us-${t.id}`,
        at: parseTs(t.entry_at),
        market: "US" as const,
        symbol: t.symbol ?? "—",
        side: t.side === "SELL" ? "SHORT" : "LONG",
        qty: t.qty ?? "—",
        entry: price(num(t.entry_price)),
        exit: closed ? price(num(t.mark_price)) : "Open",
        result: moneySigned(pnl),
        resultValue: pnl,
        status: t.status ?? "—",
      };
    }),
  ]
    .sort((a, b) => (b.at?.getTime() ?? 0) - (a.at?.getTime() ?? 0))
    .slice(0, limit);
  return (
    <DataTable
      rows={rows}
      rowKey={(r) => r.key}
      empty={<EmptyState title="No trades yet" />}
      columns={[
        { key: "t", header: "Entered", render: (r) => (r.market === "India" ? istTime(r.at) : etTime(r.at)) },
        { key: "m", header: "Market", render: (r) => r.market },
        {
          key: "s",
          header: "Stock",
          render: (r) => <External href={r.market === "India" ? nseChart(r.symbol) : usChart(r.symbol)}>{`${r.symbol} ↗`}</External>,
        },
        { key: "side", header: "Side", render: (r) => r.side },
        { key: "q", header: "Shares", numeric: true, render: (r) => r.qty },
        { key: "e", header: "Entry", numeric: true, render: (r) => r.entry },
        { key: "x", header: "Exit", numeric: true, render: (r) => r.exit },
        {
          key: "p",
          header: "P&L",
          numeric: true,
          render: (r) => <span className={r.resultValue === null ? undefined : r.resultValue >= 0 ? "is-pos" : "is-neg"}>{r.result}</span>,
        },
        { key: "st", header: "Status", render: (r) => r.status },
      ]}
    />
  );
}
