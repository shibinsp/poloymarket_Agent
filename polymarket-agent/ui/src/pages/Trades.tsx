import { useEffect, useMemo, useState } from "react";
import { useDataset } from "../data/useDataset";
import { Card } from "../components/ui/Card";
import { StatTile } from "../components/ui/StatTile";
import { EmptyState } from "../components/ui/States";
import { Badge, tradeStatusTone, type StatusTone } from "../components/ui/Badge";
import { DataTable, type Column } from "../components/ui/DataTable";
import { num } from "../lib/decimal";
import { absTime, int, money, moneyInr, moneySigned, price, relTime } from "../lib/format";
import { parseTs } from "../lib/time";
import { useNow } from "../data/useNow";
import { computeRealizedPnl } from "../lib/pnl";
import {
  TRADE_STATUSES,
  type TradeRecord,
  type IndiaSandboxStatusResponse,
  type UsPaperStatusResponse,
} from "../api/types";

export interface UnifiedTradeItem {
  id: string;
  market_id: string;
  market_question: string;
  symbol: string;
  venue_name: string;
  market_group: "india" | "us" | "polymarket";
  direction: string;
  entry_price: string;
  raw_price: number;
  quantity: number;
  size: string;
  status: string;
  pnl_num: number | null;
  pnl_pct: number | null;
  currency_symbol: string;
  matched_buy_price: number | null;
  created_at: string | null;
}

export function Trades() {
  const [indiaFeed, setIndiaFeed] = useState<{
    connected: boolean; status: string; symbols_subscribed: string[];
  } | null>(null);
  useEffect(() => {
    const load = () => fetch("/api/upstox/feed/status").then((r) => r.ok ? r.json() : null).then(setIndiaFeed).catch(() => {});
    load();
    const timer = setInterval(load, 2000);
    return () => clearInterval(timer);
  }, []);
  const trades = useDataset<TradeRecord[]>("tradesAll");
  const india = useDataset<IndiaSandboxStatusResponse>("indiaSandbox");
  const us = useDataset<UsPaperStatusResponse>("usPaper");
  const now = useNow();

  const [marketFilter, setMarketFilter] = useState<"ALL" | "INDIA" | "US" | "POLYMARKET">("ALL");
  const [status, setStatus] = useState<string>("ALL");
  const [direction, setDirection] = useState<string>("ALL");
  const [query, setQuery] = useState("");

  const polyTrades = trades.data ?? [];
  const indiaOrders = india.data?.recent_orders ?? [];
  const usOrders = us.data?.recent_orders ?? [];

  // Compute exact Realized P&L for Indian and US sandbox orders via lot matching
  const indiaPnlMap = useMemo(() => {
    return computeRealizedPnl(
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
  }, [indiaOrders]);

  const usPnlMap = useMemo(() => {
    return computeRealizedPnl(
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
  }, [usOrders]);

  // Combine trades across all active trading venues with realized P&L
  const allUnified: UnifiedTradeItem[] = useMemo(() => {
    const list: UnifiedTradeItem[] = [];

    // 1. Polymarket Trades
    for (const t of polyTrades) {
      const sym = (t.market_id ?? "").toUpperCase();
      const isIndianStock = [
        "RELIANCE", "TCS", "INFY", "HDFCBANK", "ICICIBANK", "SBIN",
        "BHARTIARTL", "ITC", "LT", "TATAMOTORS", "LICI", "HINDUNILVR", "BAJFINANCE"
      ].includes(sym);
      const isUsStock = [
        "SIRI", "NOK", "LCID", "PLUG", "NIO", "SOFI", "SPY", "AAPL", "NVDA", "QQQ"
      ].includes(sym);
      // Equities belong to their respective venues (Indian Paper or US Paper). Do not render them as Polymarket CLOB in USD!
      if (isIndianStock || isUsStock || t.direction === "BUY" || t.direction === "SELL") {
        continue;
      }

      const pnlNum = num(t.pnl);
      list.push({
        id: `poly-${t.id}`,
        market_id: t.market_id,
        market_question: t.market_question ?? t.market_id,
        symbol: t.market_id.slice(0, 8),
        venue_name: "Polymarket CLOB",
        market_group: "polymarket",
        direction: t.direction || "YES",
        entry_price: price(num(t.entry_price)),
        raw_price: num(t.entry_price) ?? 0,
        quantity: num(t.size) ?? 1,
        size: money(num(t.size)),
        status: t.status,
        pnl_num: pnlNum,
        pnl_pct: null,
        currency_symbol: "$",
        matched_buy_price: null,
        created_at: t.created_at,
      });
    }

    // 2. Indian paper-market orders
    for (const o of indiaOrders) {
      const rawP = num(o.average_fill_price || o.price) ?? 0;
      const p = rawP;
      const match = indiaPnlMap.get(o.id);
      list.push({
        id: `ind-${o.id}`,
        market_id: `NSE:${o.symbol}`,
        market_question: `${o.symbol} · NSE Cash Equity (Intraday)`,
        symbol: o.symbol,
        venue_name: "Indian Paper (NSE)",
        market_group: "india",
        direction: o.side,
        entry_price: p > 0 ? `₹${p.toFixed(2)}` : "—",
        raw_price: p,
        quantity: o.quantity,
        size: `${o.quantity} shares`,
        status: o.status,
        pnl_num: match?.pnl ?? null,
        pnl_pct: match?.pnlPct ?? null,
        currency_symbol: "₹",
        matched_buy_price: match?.matchedBuyPrice ?? null,
        created_at: o.created_at,
      });
    }

    // 3. US Alpaca Paper Orders
    for (const o of usOrders) {
      const p = num(o.average_fill_price || o.price) ?? 0;
      const match = usPnlMap.get(o.id);
      list.push({
        id: `us-${o.id}`,
        market_id: `US:${o.symbol}`,
        market_question: `${o.symbol} · US Stock & ETF (Paper)`,
        symbol: o.symbol,
        venue_name: "Alpaca Paper (US)",
        market_group: "us",
        direction: o.side,
        entry_price: p > 0 ? `$${p.toFixed(2)}` : "—",
        raw_price: p,
        quantity: o.quantity,
        size: `${o.quantity} shares`,
        status: o.status,
        pnl_num: match?.pnl ?? null,
        pnl_pct: match?.pnlPct ?? null,
        currency_symbol: "$",
        matched_buy_price: match?.matchedBuyPrice ?? null,
        created_at: o.created_at,
      });
    }

    // Sort newest first
    return list.sort((a, b) => {
      const ta = a.created_at ? new Date(a.created_at).getTime() : 0;
      const tb = b.created_at ? new Date(b.created_at).getTime() : 0;
      return tb - ta;
    });
  }, [polyTrades, indiaOrders, usOrders, indiaPnlMap, usPnlMap]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return allUnified.filter((t) => {
      if (marketFilter === "INDIA" && t.market_group !== "india") return false;
      if (marketFilter === "US" && t.market_group !== "us") return false;
      if (marketFilter === "POLYMARKET" && t.market_group !== "polymarket") return false;
      if (status !== "ALL" && t.status.toUpperCase() !== status) return false;
      if (direction !== "ALL" && t.direction.toUpperCase() !== direction) return false;
      if (q) {
        const hay = `${t.market_question} ${t.market_id} ${t.symbol} ${t.venue_name}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    });
  }, [allUnified, marketFilter, status, direction, query]);

  const filtersActive =
    marketFilter !== "ALL" || status !== "ALL" || direction !== "ALL" || query.trim() !== "";

  // Aggregate Realized Profit/Loss
  const indiaProfitInrValue = useMemo(() => {
    return filtered
      .filter((t) => t.market_group === "india" && t.pnl_num !== null)
      .reduce((acc, t) => acc + (t.pnl_num ?? 0), 0);
  }, [filtered]);

  const usProfitUsdValue = useMemo(() => {
    return filtered
      .filter((t) => t.market_group === "us" && t.pnl_num !== null)
      .reduce((acc, t) => acc + (t.pnl_num ?? 0), 0);
  }, [filtered]);

  const polyProfitUsdValue = useMemo(() => {
    return filtered
      .filter((t) => t.market_group === "polymarket" && t.pnl_num !== null)
      .reduce((acc, t) => acc + (t.pnl_num ?? 0), 0);
  }, [filtered]);

  const indiaProfitInr =
    india.data === undefined ||
    !filtered.some((t) => t.market_group === "india" && t.pnl_num !== null)
      ? null
      : indiaProfitInrValue;
  const usProfitUsd =
    us.data === undefined ||
    !filtered.some((t) => t.market_group === "us" && t.pnl_num !== null)
      ? null
      : usProfitUsdValue;
  const polyProfitUsd =
    trades.data === undefined ||
    !filtered.some((t) => t.market_group === "polymarket" && t.pnl_num !== null)
      ? null
      : polyProfitUsdValue;

  const realizedRows = filtered.filter((t) => t.pnl_num !== null);
  const winCount = realizedRows.filter((t) => (t.pnl_num ?? 0) > 0).length;
  const lossCount = realizedRows.filter((t) => (t.pnl_num ?? 0) < 0).length;
  const flatCount = realizedRows.filter((t) => (t.pnl_num ?? 0) === 0).length;

  const countIndia = allUnified.filter((t) => t.market_group === "india").length;
  const countUs = allUnified.filter((t) => t.market_group === "us").length;
  const countPoly = allUnified.filter((t) => t.market_group === "polymarket").length;

  const columns: Column<UnifiedTradeItem>[] = [
    {
      key: "market",
      header: "Market & Asset",
      truncate: true,
      render: (t) => (
        <div>
          <div style={{ fontWeight: 600, fontSize: "14px" }}>{t.symbol}</div>
          <div className="muted" style={{ fontSize: "12px" }}>{t.market_question}</div>
        </div>
      ),
      sortValue: (t) => t.symbol,
    },
    {
      key: "venue",
      header: "Trading Venue",
      render: (t) => {
        const tone: StatusTone =
          t.market_group === "india" ? "neutral" : t.market_group === "us" ? "good" : "neutral";
        return <Badge tone={tone}>{t.venue_name}</Badge>;
      },
      sortValue: (t) => t.venue_name,
    },
    {
      key: "dir",
      header: "Side",
      render: (t) => {
        const isBuy = t.direction.toUpperCase() === "BUY" || t.direction.toUpperCase() === "YES";
        return (
          <span
            style={{
              fontWeight: 700,
              padding: "2px 8px",
              borderRadius: "4px",
              background: isBuy ? "rgba(16, 185, 129, 0.12)" : "rgba(239, 68, 68, 0.12)",
              color: isBuy ? "var(--status-good, #10b981)" : "var(--status-critical, #ef4444)",
            }}
          >
            {t.direction}
          </span>
        );
      },
      sortValue: (t) => t.direction,
    },
    {
      key: "price",
      header: "Execution Price",
      numeric: true,
      render: (t) => <span style={{ fontWeight: 600 }}>{t.entry_price}</span>,
      sortValue: (t) => t.raw_price,
    },
    {
      key: "size",
      header: "Quantity / Size",
      numeric: true,
      render: (t) => t.size,
      sortValue: (t) => t.quantity,
    },
    {
      key: "status",
      header: "Status",
      render: (t) => <Badge tone={tradeStatusTone(t.status)}>{t.status}</Badge>,
      sortValue: (t) => t.status,
    },
    {
      key: "pnl",
      header: "Realized P&L (Profit)",
      numeric: true,
      render: (t) => {
        if (t.direction.toUpperCase() === "BUY") {
          return (
            <span className="muted" style={{ fontSize: "12px" }}>
              — (Open Position)
            </span>
          );
        }
        const val = t.pnl_num;
        if (val === null || val === undefined) {
          return <span className="muted">—</span>;
        }
        const isPos = val > 0;
        const isNeg = val < 0;
        const formatted = `${t.currency_symbol}${Math.abs(val).toFixed(2)}`;
        return (
          <div>
            <div
              style={{
                fontWeight: 700,
                fontSize: "14px",
                color: isPos
                  ? "var(--status-good, #10b981)"
                  : isNeg
                    ? "var(--status-critical, #ef4444)"
                    : "var(--text-secondary)",
              }}
            >
              {isPos ? `+${formatted}` : isNeg ? `-${formatted}` : `${t.currency_symbol}0.00`}
            </div>
            <div style={{ display: "flex", gap: "6px", alignItems: "center", marginTop: "2px" }}>
              {t.pnl_pct !== null && (
                <span
                  style={{
                    fontSize: "11px",
                    fontWeight: 700,
                    color: isPos
                      ? "var(--status-good, #10b981)"
                      : isNeg
                        ? "var(--status-critical, #ef4444)"
                        : "var(--text-secondary)",
                  }}
                >
                  {isPos ? `+${t.pnl_pct.toFixed(2)}%` : `${t.pnl_pct.toFixed(2)}%`}
                </span>
              )}
              {t.matched_buy_price !== null && (
                <span className="muted" style={{ fontSize: "11px" }}>
                  (Bought @ {t.currency_symbol}{t.matched_buy_price.toFixed(2)})
                </span>
              )}
            </div>
          </div>
        );
      },
      sortValue: (t) => (t.pnl_num !== null ? t.pnl_num : -999999),
    },
    {
      key: "created",
      header: "Timestamp",
      render: (t) => relTime(parseTs(t.created_at), now),
      title: (t) => absTime(parseTs(t.created_at)),
      sortValue: (t) => (parseTs(t.created_at)?.getTime() ?? null),
    },
  ];

  return (
    <div className="page">
      {/* Portfolio Multi-Market Trade Summary */}
      <Card title={filtersActive ? "Selection P&L Performance" : "Realized P&L Performance (After Selling)"}>
        <div className="grid grid--tiles">
          <StatTile
            label="India P&L (INR)"
            value={indiaProfitInr === null ? "—" : moneyInr(indiaProfitInr)}
            sub="Resolved trades in current selection"
            tone={indiaProfitInr === null ? null : indiaProfitInr > 0 ? "pos" : indiaProfitInr < 0 ? "neg" : null}
          />
          <StatTile
            label="US P&L (USD)"
            value={usProfitUsd === null ? "—" : moneySigned(usProfitUsd)}
            sub="Resolved trades in current selection"
            tone={usProfitUsd === null ? null : usProfitUsd > 0 ? "pos" : usProfitUsd < 0 ? "neg" : null}
          />
          <StatTile
            label="Polymarket P&L (USD)"
            value={polyProfitUsd === null ? "—" : moneySigned(polyProfitUsd)}
            sub="Resolved trades in current selection"
            tone={polyProfitUsd === null ? null : polyProfitUsd > 0 ? "pos" : polyProfitUsd < 0 ? "neg" : null}
          />
          <StatTile
            label="Win rate"
            value={winCount + lossCount > 0 ? `${((winCount / (winCount + lossCount)) * 100).toFixed(0)}%` : "—"}
            sub={`${int(winCount)}W / ${int(lossCount)}L · ${int(flatCount)} flat`}
          />
          <StatTile
            label="Total Orders"
            value={int(filtered.length)}
            sub={`${int(realizedRows.length)} with realized P&L · ${int(filtered.length - realizedRows.length)} without`}
          />
        </div>
      </Card>

      {/* Market Selector Tabs */}
      <div style={{ display: "flex", gap: "8px", margin: "4px 0 12px 0", flexWrap: "wrap" }}>
        <button
          className={`btn ${marketFilter === "ALL" ? "btn--primary" : ""}`}
          onClick={() => setMarketFilter("ALL")}
        >
          All Markets ({allUnified.length})
        </button>
        <button
          className={`btn ${marketFilter === "INDIA" ? "btn--primary" : ""}`}
          onClick={() => setMarketFilter("INDIA")}
        >
          🇮🇳 Indian Equities ({countIndia})
        </button>
        <button
          className={`btn ${marketFilter === "US" ? "btn--primary" : ""}`}
          onClick={() => setMarketFilter("US")}
        >
          🇺🇸 US Equities & ETFs ({countUs})
        </button>
        <button
          className={`btn ${marketFilter === "POLYMARKET" ? "btn--primary" : ""}`}
          onClick={() => setMarketFilter("POLYMARKET")}
        >
          🌐 Polymarket ({countPoly})
        </button>
      </div>

      {/* Upstox background feed & local paper trading simulator */}
      {(marketFilter === "INDIA" || marketFilter === "ALL") && (
        <Card title="🇮🇳 Upstox Background Feed → Strategy → Local Paper Simulator">
          <div
            style={{
              display: "flex",
              flexWrap: "wrap",
              gap: "14px",
              alignItems: "center",
              marginBottom: "14px",
              padding: "12px 16px",
              borderRadius: "8px",
              background: "rgba(255,255,255,0.03)",
              border: "1px solid rgba(255,255,255,0.08)",
            }}
          >
            <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
              <span
                style={{
                  width: "10px",
                  height: "10px",
                  borderRadius: "50%",
                  background: indiaFeed?.connected ? "#10b981" : "#f59e0b",
                  display: "inline-block",
                  boxShadow: indiaFeed?.connected ? "0 0 8px #10b981" : "0 0 8px #f59e0b",
                }}
              />
              <span style={{ fontWeight: 600, fontSize: "13px" }}>
                Upstox Feed: {indiaFeed?.connected ? "Live WebSocket stream" : "Waiting for live data"}
              </span>
            </div>
            <div style={{ fontSize: "12px", color: "var(--text-secondary)" }}>
              Subscribed: {indiaFeed?.symbols_subscribed?.slice(0, 6).join(", ") || "No symbols subscribed yet"}
            </div>
            <div style={{ marginLeft: "auto", display: "flex", gap: "8px", alignItems: "center" }}>
              <Badge tone="good">100% Paper Simulator · Zero Live Orders</Badge>
              <a
                href="#/india-test"
                onClick={() => {
                  if (!window.location.hash.includes("india-test")) {
                    window.location.href = "/india-test";
                  }
                }}
                className="btn"
                style={{ fontSize: "11px", padding: "3px 8px", textDecoration: "none" }}
              >
                Strategy Evaluator ↗
              </a>
            </div>
          </div>

          <div style={{ margin: "0 0 14px", color: "var(--text-secondary)", fontSize: "12px" }}>
            The agent evaluates fresh Upstox updates in the background. Market prices are not displayed here.
            <a href="https://pro.upstox.com/" target="_blank" rel="noopener noreferrer" style={{ marginLeft: "8px" }}>View live market ↗</a>
          </div>

          <div
            className="grid grid--tiles"
            style={{ gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))" }}
          >
            <StatTile
              label="Virtual Indian Cash"
              value={moneyInr(num(india.data?.available_cash_inr))}
              sub="Available sandbox cash"
            />
            <StatTile
              label="Virtual Total Equity"
              value={moneyInr(num(india.data?.total_equity_inr))}
              sub="Reported sandbox equity"
            />
            <StatTile
              label="Open Virtual Positions"
              value={moneyInr(num(india.data?.open_positions_cost_inr))}
              sub={
                Object.keys(india.data?.holdings ?? {}).length > 0
                  ? Object.entries(india.data?.holdings ?? {})
                      .map(([sym, qty]) => `${sym}: ${qty}sh`)
                      .join(", ")
                  : "No open positions"
              }
            />
            <StatTile
              label="Realized P&L (Post-Sale)"
              value={
                indiaProfitInr === null ? "—" : moneyInr(indiaProfitInr)
              }
              sub="Saved in SQLite (survives restarts)"
              tone={indiaProfitInr === null ? null : indiaProfitInr > 0 ? "pos" : indiaProfitInr < 0 ? "neg" : null}
            />
          </div>
        </Card>
      )}

      {/* Filters Card */}
      <Card title="Filters & Search">
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
            <span>Side</span>
            <select className="select" value={direction} onChange={(e) => setDirection(e.target.value)}>
              <option value="ALL">All Sides</option>
              <option value="BUY">BUY</option>
              <option value="SELL">SELL</option>
              <option value="YES">YES</option>
              <option value="NO">NO</option>
            </select>
          </label>
          <label className="field" style={{ flex: 1, minWidth: 200 }}>
            <span>Search Symbol / Market</span>
            <input
              className="input"
              value={query}
              placeholder="Filter by symbol (e.g. RELIANCE, SPY, INFY)..."
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
          {filtersActive && (
            <button
              className="btn"
              onClick={() => {
                setMarketFilter("ALL");
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

      {/* Trades Table */}
      <Card title={`Executed Trades & Post-Sale P&L Ledger (${filtered.length})`}>
        {allUnified.length === 0 ? (
          <EmptyState
            title="No trades yet"
            hint="Trades appear here automatically when orders are placed in the Indian paper market, US Alpaca paper, or Polymarket."
          />
        ) : filtered.length === 0 ? (
          <EmptyState
            title="No trades match these filters"
            hint={`${int(allUnified.length)} trades are loaded across markets.`}
            action={
              <button
                className="btn"
                onClick={() => {
                  setMarketFilter("ALL");
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
            rows={filtered}
            rowKey={(t) => t.id}
            initialSort="created"
            columns={columns}
          />
        )}
      </Card>
    </div>
  );
}
