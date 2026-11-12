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
  strategy?: string;
}

export interface IndiaBook {
  strategy: string;
  capital_inr: string | number;
  trades_today: number;
  pnl_today_inr: string | number;
  pnl_total_inr: string | number;
  losses_inr?: string | number;
  profits_inr?: string | number;
  trading_capital_inr?: string | number;
}

export interface IndiaIntraday {
  capital_inr: string | number | null;
  trades_today: number;
  pnl_today_inr: string | number;
  pnl_total_inr: string | number;
  /** Losing trades only; they come off the capital. */
  losses_inr?: string | number;
  /** Winning trades only; set aside, never added to the capital. */
  profits_inr?: string | number;
  trading_capital_inr?: string | number | null;
  books?: IndiaBook[];
  trades: IndiaTrade[];
}

const BOOK_LABEL: Record<string, string> = {
  india_orb: "Opening-range breakout",
  india_burst: "Opening burst",
  india_gapgo: "Gap and go",
  india_orb_tfm_skip: "Breakout skipped by TimesFM (shadow)",
  india_orb_track: "Opening-range breakout, tracking only (no money)",
  india_burst_tfm_skip: "Burst skipped by TimesFM (shadow)",
};
const bookLabel = (s?: string) => (s ? BOOK_LABEL[s] ?? s : BOOK_LABEL.india_orb);

/** The U.S. intraday book: trades size from `trading_capital_usd`, never more than the capital. */
export interface UsBook {
  capital_usd: string;
  since: string | null;
  pnl_total_usd: string;
  pnl_today_usd: string;
  value_usd: string;
  trading_capital_usd: string;
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
  strategy?: string;
}

const usBook = (s?: string) => (s === "us_burst" ? "Opening burst" : "Opening-range breakout");

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
// Older rows stored the broker's error text as the reason (3 Nov ORCL: the
// day-end sell was refused while the resting stop held the shares).
const exitLabel = (r: string | null) =>
  !r ? "Open" : EXIT_LABEL[r] ?? (r.startsWith("Failed to place Alpaca order") ? "Closed at day end (Alpaca refused the first sell order)" : r);

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
  // Losing trades come off the capital; winning trades are set aside.
  const losses = num(data?.losses_inr ?? null) ?? Math.min(total, 0);
  const kept = num(data?.profits_inr ?? null) ?? Math.max(total, 0);
  const tradingCapital = num(data?.trading_capital_inr ?? null) ?? (capital === null ? null : capital + losses);
  const trades = data?.trades ?? [];
  const books = data?.books ?? [];
  return (
    <Card
      title="🇮🇳 India intraday — paper ledgers (real figures)"
      actions={<External href="https://pro.upstox.com/">View live market on Upstox ↗</External>}
    >
      <div className="grid grid--tiles">
        <StatTile label="Capital (all books)" value={capital === null ? "—" : inr(capital)} sub="NIFTY 50 · no leverage · whole shares" />
        <StatTile label="Trading capital" value={tradingCapital === null ? "—" : inr(tradingCapital)} sub={`Capital minus losses ${inrSigned(losses)} · profit ${inrSigned(kept)} kept aside`} />
        <StatTile label="Closed P&L (all time)" value={inrSigned(total)} tone={toneOf(total)} sub="After brokerage, STT and fees" />
        <StatTile label="Today" value={inrSigned(data?.pnl_today_inr ?? 0)} tone={toneOf(num(data?.pnl_today_inr ?? null))} sub={`${data?.trades_today ?? 0} trade(s) today`} />
      </div>
      {books.length > 1 && (
        <div className="grid grid--tiles" style={{ marginTop: "12px" }}>
          {books.map((b) => (
            <StatTile
              key={b.strategy}
              label={bookLabel(b.strategy)}
              value={inrSigned(b.pnl_total_inr)}
              tone={toneOf(num(b.pnl_total_inr))}
              sub={`Capital ${inr(b.trading_capital_inr ?? b.capital_inr)} of ${inr(b.capital_inr)} · today ${inrSigned(b.pnl_today_inr)} · ${b.trades_today} trade(s)`}
            />
          ))}
        </div>
      )}
      <h3 style={{ margin: "16px 0 8px", fontSize: "14px" }}>Trades</h3>
      <DataTable
        rows={trades}
        rowKey={(t, i) => `${t.entry_at}-${t.symbol}-${i}`}
        empty={<EmptyState title="No India trades yet" />}
        columns={[
          { key: "d", header: "Bought / shorted", render: (t) => istTime(parseTs(t.entry_at)) },
          { key: "b", header: "Strategy", render: (t) => bookLabel(t.strategy) },
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
      title="🇺🇸 US intraday — breakout and opening burst, Alpaca paper (real figures)"
      actions={<External href="https://app.alpaca.markets/paper/dashboard/overview">View the Alpaca paper account ↗</External>}
    >
      <div className="grid grid--tiles">
        <StatTile label="Alpaca paper account" value={equity === null ? "—" : money(equity)} sub="Live equity as Alpaca reports it" />
        <StatTile label="Strategy capital" value={capital === null ? "—" : money(capital)} sub="$150 a trade, at most 3 a day across both" />
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
          { key: "b", header: "Strategy", render: (t) => usBook(t.strategy) },
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

interface UsOptionTrade {
  day: string;
  underlying: string;
  contract: string;
  kind: "CALL" | "PUT";
  qty: number;
  status: string;
  entry_price: string | null;
  exit_price: string | null;
  exit_reason: string | null;
  pnl: string | null;
  entry_at: string;
  exit_at: string | null;
}

export interface UsOptionsBook {
  capital_usd: string | number;
  trades_today: number;
  pnl_today_usd: string | number;
  pnl_total_usd: string | number;
  losses_usd: string | number;
  profits_usd: string | number;
  trading_capital_usd: string | number;
}

const optionExit: Record<string, string> = {
  STOCK_EXIT: "Stock trade closed",
  FLAT_1550: "15:50 flat",
};

/** The options book: a call or put on each ORB / burst entry, its own $500. */
export function UsOptionsLedger() {
  const data = usePolled<{ book: UsOptionsBook | null; trades: UsOptionTrade[] }>("/api/us/options");
  const book = data?.book ?? null;
  const trades = data?.trades ?? [];
  if (data && !book) return null;
  return (
    <Card title="🇺🇸 US options — calls and puts on the breakout signals, Alpaca paper">
      <div className="grid grid--tiles">
        <StatTile
          label="Trading capital"
          value={book ? money(num(book.trading_capital_usd)) : "—"}
          sub={book ? `${money(num(book.capital_usd))} minus losses ${moneySigned(num(book.losses_usd))} · profit ${moneySigned(num(book.profits_usd))} kept aside` : ""}
        />
        <StatTile label="Closed P&L" value={moneySigned(num(book?.pnl_total_usd ?? null))} tone={toneOf(num(book?.pnl_total_usd ?? null))} sub="Long premium only: a trade can lose at most what it paid" />
        <StatTile label="Today" value={moneySigned(num(book?.pnl_today_usd ?? null))} tone={toneOf(num(book?.pnl_today_usd ?? null))} sub={`${book?.trades_today ?? 0} trade(s) today, at most 3`} />
      </div>
      <h3 style={{ margin: "16px 0 8px", fontSize: "14px" }}>Trades</h3>
      <DataTable
        rows={trades}
        rowKey={(t) => `${t.contract}-${t.entry_at}`}
        empty={<EmptyState title="No option trades yet" />}
        columns={[
          { key: "d", header: "Entered", render: (t) => etTime(parseTs(t.entry_at)) },
          { key: "u", header: "Stock", render: (t) => t.underlying },
          { key: "k", header: "Type", render: (t) => t.kind },
          { key: "c", header: "Contract", render: (t) => (t.contract === "-" ? "—" : t.contract) },
          { key: "q", header: "Contracts", numeric: true, render: (t) => t.qty || "—" },
          { key: "e", header: "Paid", numeric: true, render: (t) => price(num(t.entry_price)) },
          { key: "x", header: "Sold", numeric: true, render: (t) => price(num(t.exit_price)) },
          { key: "s", header: "Status", render: (t) => (t.status === "CLOSED" ? optionExit[t.exit_reason ?? ""] ?? "Closed" : t.status === "CANCELLED" ? t.exit_reason ?? "No trade" : t.status) },
          { key: "p", header: "P&L", numeric: true, render: (t) => (t.status === "CLOSED" ? moneySigned(num(t.pnl)) : "—") },
        ]}
      />
    </Card>
  );
}

export interface CryptoBook {
  book: string;
  capital_usd: string | number;
  losses_usd: string | number;
  profits_usd: string | number;
  trading_capital_usd: string | number;
  open_cost_usd: string | number;
  open_value_usd: string | number;
  unrealized_usd: string | number;
  value_usd: string | number;
}

interface CryptoTrade {
  book: string;
  symbol: string;
  status: string;
  qty: string;
  cost: string;
  entry_price: string;
  entry_at: string;
  entry_reason: string;
  exit_price: string | null;
  exit_at: string | null;
  exit_reason: string | null;
  pnl: string | null;
}

interface CryptoCheck {
  at: string;
  symbol: string;
  kind: string;
  price: string;
  chg_24h_pct: string | null;
  chg_7d_pct: string | null;
  chg_60d_pct: string | null;
  rule_says: string;
  news_risk: string;
  news_reason: string | null;
  market_view: string | null;
  headlines: string | null;
  action: string;
}

const cryptoBookLabel: Record<string, string> = {
  crypto: "With news guard (Alpaca paper)",
  crypto_rule: "Rule only (shadow)",
  crypto_sma50: "Rival: above 50-day average",
  crypto_donchian: "Rival: 20-day high in, 10-day low out",
  crypto_mom_sma20: "Rival: 60-day rule or above 20-day average",
};

const pct = (v: string | null) => (v === null ? "—" : `${Number(v) >= 0 ? "+" : ""}${Number(v).toFixed(1)}%`);

/** The crypto books: BTC + ETH on the 60-day rule, one with a news guard. */
export function CryptoLedger() {
  const data = usePolled<{ books: CryptoBook[]; trades: CryptoTrade[]; checks: CryptoCheck[] }>("/api/crypto");
  const books = data?.books ?? [];
  const trades = data?.trades ?? [];
  const checks = data?.checks ?? [];
  if (data && books.length === 0) return null;
  return (
    <Card title="₿ Crypto — BTC + ETH, 60-day rule with news watched, against 3 rival rules (paper)">
      <div className="grid grid--tiles">
        {books.map((b) => (
          <StatTile
            key={b.book}
            label={cryptoBookLabel[b.book] ?? b.book}
            value={money(num(b.value_usd))}
            tone={toneOf((num(b.value_usd) ?? 0) - (num(b.capital_usd) ?? 0))}
            sub={`Capital ${money(num(b.trading_capital_usd))} of ${money(num(b.capital_usd))} · open ${money(num(b.open_value_usd))} (${moneySigned(num(b.unrealized_usd))}) · profit ${moneySigned(num(b.profits_usd))} kept aside`}
          />
        ))}
      </div>
      <h3 style={{ margin: "16px 0 8px", fontSize: "14px" }}>What it sees — market and news checks</h3>
      <DataTable
        rows={checks.slice(0, 20)}
        rowKey={(c) => `${c.symbol}-${c.at}`}
        empty={<EmptyState title="No checks yet" />}
        columns={[
          { key: "t", header: "When (IST)", render: (c) => istTime(parseTs(c.at)) },
          { key: "s", header: "Coin", render: (c) => c.symbol },
          { key: "p", header: "Price", numeric: true, render: (c) => price(num(c.price)) },
          { key: "m", header: "24h / 7d / 60d", render: (c) => `${pct(c.chg_24h_pct)} / ${pct(c.chg_7d_pct)} / ${pct(c.chg_60d_pct)}` },
          { key: "r", header: "Rule", render: (c) => c.rule_says },
          { key: "n", header: "News", render: (c) => `${c.news_risk}${c.news_reason ? ` — ${c.news_reason}` : ""}` },
          { key: "v", header: "Market view", render: (c) => c.market_view || "—" },
          { key: "a", header: "Action", render: (c) => c.action },
        ]}
      />
      <h3 style={{ margin: "16px 0 8px", fontSize: "14px" }}>Trades</h3>
      <DataTable
        rows={trades}
        rowKey={(t) => `${t.book}-${t.symbol}-${t.entry_at}`}
        empty={<EmptyState title="No crypto trades yet" />}
        columns={[
          { key: "b", header: "Book", render: (t) => cryptoBookLabel[t.book] ?? t.book },
          { key: "s", header: "Coin", render: (t) => t.symbol },
          { key: "d", header: "Bought (IST)", render: (t) => istTime(parseTs(t.entry_at)) },
          { key: "c", header: "Paid", numeric: true, render: (t) => money(num(t.cost)) },
          { key: "e", header: "At", numeric: true, render: (t) => price(num(t.entry_price)) },
          { key: "x", header: "Sold at", numeric: true, render: (t) => price(num(t.exit_price)) },
          { key: "st", header: "Status", render: (t) => (t.status === "CLOSED" ? t.exit_reason ?? "Closed" : "Holding") },
          { key: "p", header: "P&L", numeric: true, render: (t) => (t.status === "CLOSED" ? moneySigned(num(t.pnl)) : "—") },
        ]}
      />
    </Card>
  );
}

interface PolymarketTrade {
  id: number;
  market_id: string;
  question: string | null;
  side: string;
  entry_price: string;
  size: string;
  fair_value: string | null;
  status: string;
  pnl: string | null;
  entry_at: string | null;
  closed_at: string | null;
}

interface PolymarketBook {
  capital_usd: string | number | null;
  open_cost_usd: string | number;
  realized_pnl_usd: string | number;
  cash_usd: string | number | null;
  trades: PolymarketTrade[];
}

/** The Polymarket paper book: its own $50 wallet, apart from the US and India books. */
export function PolymarketLedger() {
  const data = usePolled<PolymarketBook>("/api/polymarket/trades");
  const trades = data?.trades ?? [];
  const capital = num(data?.capital_usd ?? null);
  const realized = num(data?.realized_pnl_usd ?? null) ?? 0;
  const open = num(data?.open_cost_usd ?? null) ?? 0;
  return (
    <Card
      title="🌐 Polymarket — paper ledger (real figures)"
      actions={<External href="https://polymarket.com/">View markets on Polymarket ↗</External>}
    >
      <div className="grid grid--tiles">
        <StatTile label="Capital" value={capital === null ? "—" : money(capital)} sub="Its own paper wallet · simulated fills" />
        <StatTile label="Cash free" value={money(num(data?.cash_usd ?? null) ?? 0)} sub="Capital plus settled P&L minus open bets" />
        <StatTile label="In open bets" value={money(open)} sub={`${trades.filter((t) => t.status === "OPEN").length} open position(s)`} />
        <StatTile label="Settled P&L" value={moneySigned(realized)} tone={toneOf(realized)} sub={`${trades.filter((t) => t.status !== "OPEN").length} closed bet(s)`} />
      </div>
      <h3 style={{ margin: "16px 0 8px", fontSize: "14px" }}>Bets</h3>
      <DataTable
        rows={trades}
        rowKey={(t) => String(t.id)}
        empty={<EmptyState title="No Polymarket bets yet" hint="It checks markets every 10 minutes and bets only when its estimate differs from the price by at least 5%." />}
        columns={[
          { key: "d", header: "Placed", render: (t) => istTime(parseTs(t.entry_at)) },
          { key: "q", header: "Market", truncate: true, render: (t) => t.question ?? t.market_id },
          { key: "s", header: "Side", render: (t) => t.side },
          { key: "p", header: "Price", numeric: true, render: (t) => `${(Number(t.entry_price) * 100).toFixed(1)}¢` },
          { key: "f", header: "Its estimate", numeric: true, render: (t) => (t.fair_value ? `${(Number(t.fair_value) * 100).toFixed(1)}%` : "—") },
          { key: "z", header: "Shares", numeric: true, render: (t) => Number(t.size).toFixed(2) },
          { key: "st", header: "Status", render: (t) => t.status },
          { key: "pl", header: "P&L", numeric: true, render: (t) => (t.pnl ? moneySigned(num(t.pnl)) : "—") },
        ]}
      />
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

interface UsSpreadTrade {
  underlying: string;
  short_strike: string;
  long_strike: string;
  qty: number;
  entry_at: string;
  entry_credit: string | null;
  exit_debit: string | null;
  pnl: string | null;
  status: string;
}

interface UsIbsTrade {
  id: number;
  symbol: string;
  qty: string | number;
  entry_day: string;
  entry_price: string | null;
  exit_price: string | null;
  pnl: string | null;
  status: string;
}

/** The latest trades from every strategy, newest first. */
export function RecentStrategyTrades({ limit = 8 }: { limit?: number }) {
  const india = usePolled<IndiaIntraday>("/api/india/intraday");
  const us = usePolled<{ trades: UsOrbTrade[] }>("/api/us/orb/trades");
  const spreads = usePolled<{ trades: UsSpreadTrade[] }>("/api/us/spreads");
  const ibs = usePolled<{ trades: UsIbsTrade[] }>("/api/us/ibs");
  const tt = usePolled<{ trades: UsIbsTrade[] }>("/api/us/turnaround");
  const pf = usePolled<{ trades: UsIbsTrade[] }>("/api/us/prefomc");
  const rows: RecentRow[] = [
    // Bear call spreads: sold for a credit, bought back for a debit (per share).
    ...(spreads?.trades ?? [])
      .filter((t) => t.status !== "CANCELLED")
      .map((t) => {
        const pnl = num(t.pnl);
        const closed = t.status === "CLOSED";
        return {
          key: `sp-${t.underlying}-${t.entry_at}`,
          at: parseTs(t.entry_at),
          market: "US" as const,
          symbol: t.underlying,
          side: `SPREAD ${t.short_strike}/${t.long_strike}C`,
          qty: String(t.qty),
          entry: `${price(num(t.entry_credit))} cr`,
          exit: closed && t.exit_debit ? price(num(t.exit_debit)) : "Open",
          result: closed ? moneySigned(pnl) : "—",
          resultValue: closed ? pnl : null,
          status: t.status,
        };
      }),
    // Pre-FOMC drift: bought the close before a Fed decision, sold the decision day's close (simulated).
    ...(pf?.trades ?? []).map((t) => {
      const pnl = num(t.pnl);
      return {
        key: `pf-${t.id}`,
        at: parseTs(`${t.entry_day}T20:55:00Z`),
        market: "US" as const,
        symbol: t.symbol,
        side: "LONG (pre-Fed)",
        qty: String(t.qty),
        entry: price(num(t.entry_price)),
        exit: t.exit_price ? price(num(t.exit_price)) : "Open",
        result: t.pnl ? moneySigned(pnl) : "—",
        resultValue: t.pnl ? pnl : null,
        status: t.status,
      };
    }),
    // Turnaround Tuesday: bought Monday's open, sold Tuesday's close (simulated).
    ...(tt?.trades ?? []).map((t) => {
      const pnl = num(t.pnl);
      return {
        key: `tt-${t.id}`,
        at: parseTs(`${t.entry_day}T14:30:00Z`),
        market: "US" as const,
        symbol: t.symbol,
        side: "LONG (Turnaround Tue)",
        qty: String(t.qty),
        entry: price(num(t.entry_price)),
        exit: t.exit_price ? price(num(t.exit_price)) : "Open",
        result: t.pnl ? moneySigned(pnl) : "—",
        resultValue: t.pnl ? pnl : null,
        status: t.status,
      };
    }),
    // IBS: bought at a weak close, sold at the next close (simulated).
    ...(ibs?.trades ?? []).map((t) => {
      const pnl = num(t.pnl);
      return {
        key: `ibs-${t.id}`,
        at: parseTs(`${t.entry_day}T20:00:00Z`),
        market: "US" as const,
        symbol: t.symbol,
        side: "LONG (IBS)",
        qty: String(t.qty),
        entry: price(num(t.entry_price)),
        exit: t.exit_price ? price(num(t.exit_price)) : "Open",
        result: t.pnl ? moneySigned(pnl) : "—",
        resultValue: t.pnl ? pnl : null,
        status: t.status,
      };
    }),
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
