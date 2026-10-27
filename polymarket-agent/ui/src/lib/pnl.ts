/**
 * Realized Profit and Loss calculation engine for multi-market trading.
 * Matches BUY lots against SELL orders to compute exact profit/loss and return %.
 */

export interface OrderInput {
  id: string;
  symbol: string;
  side: string;
  quantity: number;
  price: number | string | null;
  average_fill_price?: number | string | null;
  created_at: string | null;
}

export interface OrderPnlResult {
  pnl: number | null;
  pnlPct: number | null;
  matchedBuyPrice: number | null;
  matchedQty: number;
  isSell: boolean;
}

export function computeRealizedPnl(orders: OrderInput[]): Map<string, OrderPnlResult> {
  // Sort oldest first for chronological matching
  const sorted = [...orders].sort((a, b) => {
    const ta = a.created_at ? new Date(a.created_at).getTime() : 0;
    const tb = b.created_at ? new Date(b.created_at).getTime() : 0;
    return ta - tb;
  });

  interface Lot {
    qty: number;
    price: number;
  }

  const inventory = new Map<string, Lot[]>();
  const results = new Map<string, OrderPnlResult>();

  for (const o of sorted) {
    const sym = o.symbol.toUpperCase();
    if (!inventory.has(sym)) {
      inventory.set(sym, []);
    }
    const lots = inventory.get(sym)!;

    const fillPrice =
      typeof o.average_fill_price === "number"
        ? o.average_fill_price
        : typeof o.average_fill_price === "string" && o.average_fill_price.trim() !== ""
          ? parseFloat(o.average_fill_price)
          : typeof o.price === "number"
            ? o.price
            : typeof o.price === "string" && o.price.trim() !== ""
              ? parseFloat(o.price)
              : 0;

    const qty = o.quantity > 0 ? o.quantity : 1;
    const side = o.side ? o.side.toUpperCase() : "BUY";

    if (side === "BUY") {
      lots.push({ qty, price: fillPrice });
      results.set(o.id, {
        pnl: null,
        pnlPct: null,
        matchedBuyPrice: null,
        matchedQty: 0,
        isSell: false,
      });
    } else if (side === "SELL") {
      // Match against prior BUY lots (LIFO matching closes the latest scalp entry first)
      let needed = qty;
      let totalCost = 0;
      let matchedQty = 0;

      for (let i = lots.length - 1; i >= 0 && needed > 0; i--) {
        const lot = lots[i];
        if (lot.qty <= needed) {
          totalCost += lot.qty * lot.price;
          matchedQty += lot.qty;
          needed -= lot.qty;
          lots.splice(i, 1);
        } else {
          totalCost += needed * lot.price;
          matchedQty += needed;
          lot.qty -= needed;
          needed = 0;
        }
      }

      if (matchedQty > 0) {
        const proceeds = matchedQty * fillPrice;
        const pnl = proceeds - totalCost;
        const avgBuy = totalCost / matchedQty;
        const pnlPct = avgBuy > 0 ? ((fillPrice - avgBuy) / avgBuy) * 100 : 0;

        results.set(o.id, {
          pnl: Number(pnl.toFixed(2)),
          pnlPct: Number(pnlPct.toFixed(2)),
          matchedBuyPrice: Number(avgBuy.toFixed(2)),
          matchedQty,
          isSell: true,
        });
      } else {
        // Sold without recorded prior inventory in this session
        results.set(o.id, {
          pnl: 0,
          pnlPct: 0,
          matchedBuyPrice: fillPrice,
          matchedQty: qty,
          isSell: true,
        });
      }
    } else {
      results.set(o.id, {
        pnl: null,
        pnlPct: null,
        matchedBuyPrice: null,
        matchedQty: 0,
        isSell: false,
      });
    }
  }

  return results;
}
