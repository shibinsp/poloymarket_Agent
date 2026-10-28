//! Indian Market Paper Trading Simulator & Strategy Decision Engine.
//!
//! Enforces:
//! 1. Strictly virtual cash and positions: Starting capital is locked to ₹1,000 INR (or configured base capital).
//! 2. Zero live orders: Decisions are simulated locally in SQLite and NEVER touch Upstox's live order endpoint.
//! 3. Real-time strategy evaluation: Live market data ticks are evaluated for BUY, SELL, or WAIT signals.
//! 4. Full persistence across process restarts in the SQLite database.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
use tracing::info;
use uuid::Uuid;

use crate::db::store::{IndiaSandboxOrderRecord, Store};
use crate::india::costs::{IndianCostConfig, IndianTransactionCosts};
use crate::india::instruments::find_nse_instrument;
use crate::venue::types::Side;

/// Strategy decision on a market price tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrategyDecision {
    /// "BUY", "SELL", or "WAIT"
    pub action: String,
    pub symbol: String,
    pub price: Decimal,
    pub quantity: i64,
    pub reason: String,
    pub confidence: Decimal,
    pub timestamp: String,
}

/// Paper order submission request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndiaPaperOrderRequest {
    pub symbol: String,
    pub side: String,       // "BUY" | "SELL"
    pub order_type: String, // "LIMIT" | "MARKET"
    pub quantity: i64,
    pub price: Option<Decimal>,
    pub trigger_reason: Option<String>,
}

/// Paper order execution result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndiaPaperOrderResult {
    pub order: IndiaSandboxOrderRecord,
    pub costs: IndianTransactionCosts,
    pub available_cash_inr: Decimal,
    pub total_equity_inr: Decimal,
    pub disclaimer: String,
}

/// Consolidated portfolio state for the Indian paper simulator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndiaPaperPortfolioState {
    pub base_capital_inr: Decimal,
    pub available_cash_inr: Decimal,
    pub open_positions_cost_inr: Decimal,
    pub realized_profit_inr: Decimal,
    pub total_equity_inr: Decimal,
    pub holdings: HashMap<String, Decimal>,
    pub open_positions: Vec<IndiaOpenPosition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndiaOpenPosition {
    pub symbol: String,
    pub quantity: i64,
    pub average_buy_price: Decimal,
    pub current_price: Decimal,
    pub cost_basis: Decimal,
    pub current_value: Decimal,
    pub unrealized_pnl: Decimal,
    pub unrealized_pnl_pct: Decimal,
}

/// Strategy evaluator: Evaluates live price against current portfolio state.
pub fn evaluate_strategy_tick(
    symbol: &str,
    current_price: Decimal,
    available_cash: Decimal,
    current_holding: i64,
    avg_entry_price: Option<Decimal>,
) -> StrategyDecision {
    let now = Utc::now().to_rfc3339();

    // 1. If we hold shares of this stock, evaluate exit rules (Take-Profit & Stop-Loss)
    if current_holding > 0 {
        if let Some(entry) = avg_entry_price {
            if entry > Decimal::ZERO {
                let return_pct = (current_price - entry) / entry;

                // Take Profit Target (+2.0% realistic intraday target)
                if return_pct >= dec!(0.020) {
                    return StrategyDecision {
                        action: "SELL".to_string(),
                        symbol: symbol.to_string(),
                        price: current_price,
                        quantity: current_holding,
                        reason: format!(
                            "Take-Profit target (+{:.2}%) hit: entry ₹{entry:.2} -> live ₹{current_price:.2}",
                            return_pct * dec!(100)
                        ),
                        confidence: dec!(0.90),
                        timestamp: now,
                    };
                }

                // Stop Loss Threshold (-1.5% strict risk cut)
                if return_pct <= dec!(-0.015) {
                    return StrategyDecision {
                        action: "SELL".to_string(),
                        symbol: symbol.to_string(),
                        price: current_price,
                        quantity: current_holding,
                        reason: format!(
                            "Stop-Loss risk cutoff ({:.2}%) triggered: entry ₹{entry:.2} -> live ₹{current_price:.2}",
                            return_pct * dec!(100)
                        ),
                        confidence: dec!(0.95),
                        timestamp: now,
                    };
                }

                return StrategyDecision {
                    action: "WAIT".to_string(),
                    symbol: symbol.to_string(),
                    price: current_price,
                    quantity: 0,
                    reason: format!(
                        "Holding position: current return is {:.2}% (Target: +2.0%, Stop: -1.5%)",
                        return_pct * dec!(100)
                    ),
                    confidence: dec!(0.70),
                    timestamp: now,
                };
            }
        }
    }

    // 2. If we don't hold shares, entry decisions are deferred to the continuous
    // Pullback & Confirmed Rebound strategy worker which operates on completed 5-minute candles.
    let entry_charges = crate::india::costs::IndianCostConfig::default()
        .calculate(current_price, crate::venue::types::Side::Buy)
        .total;
    let single_share_cost = current_price + entry_charges;
    if single_share_cost > available_cash {
        return StrategyDecision {
            action: "WAIT".to_string(),
            symbol: symbol.to_string(),
            price: current_price,
            quantity: 0,
            reason: format!(
                "Price ₹{current_price:.2} plus estimated entry charges ₹{entry_charges:.2} exceeds available cash ₹{available_cash:.2}. Budget preserved."
            ),
            confidence: dec!(1.0),
            timestamp: now,
        };
    }

    StrategyDecision {
        action: "WAIT".to_string(),
        symbol: symbol.to_string(),
        price: current_price,
        quantity: 0,
        reason: format!(
            "No active position for {symbol}. Entries are evaluated on completed 5-minute candles by the Pullback & Confirmed Rebound strategy."
        ),
        confidence: dec!(0.60),
        timestamp: now,
    }
}

/// Execute a virtual paper order in the simulator.
///
/// CRITICAL SAFETY GUARANTEE: Never makes network calls to Upstox's live order endpoint.
/// Everything is simulated locally using virtual cash and persisted in SQLite.
pub async fn execute_paper_order(
    store: &Store,
    base_capital: Decimal,
    req: &IndiaPaperOrderRequest,
    current_market_price: Decimal,
) -> Result<IndiaPaperOrderResult> {
    let instrument = find_nse_instrument(&req.symbol).context(format!(
        "Symbol {:?} is not in the verified NSE universe",
        req.symbol
    ))?;

    if req.quantity <= 0 {
        bail!(
            "Quantity must be a positive whole integer for NSE cash equities (got {})",
            req.quantity
        );
    }

    let is_buy = req.side.eq_ignore_ascii_case("BUY");
    let is_sell = req.side.eq_ignore_ascii_case("SELL");
    if !is_buy && !is_sell {
        bail!("Invalid order side {:?} — must be BUY or SELL", req.side);
    }

    let fill_price = match req.price {
        Some(p) if p > Decimal::ZERO => p,
        _ => current_market_price,
    };

    let existing_orders = store
        .get_india_sandbox_orders(200)
        .await
        .unwrap_or_default();
    let portfolio = compute_portfolio_accounting(base_capital, &existing_orders, &HashMap::new());

    let order_cost = fill_price * Decimal::from(req.quantity);
    let side_enum = if is_buy { Side::Buy } else { Side::Sell };
    let costs = IndianCostConfig::default().calculate(order_cost, side_enum);

    if is_buy {
        let total_debit = order_cost + costs.total;
        if total_debit > portfolio.available_cash_inr {
            bail!(
                "Insufficient virtual cash in Indian paper account. Available: ₹{:.2}, needed including charges: ₹{:.2}. US market ($) funds are strictly isolated.",
                portfolio.available_cash_inr,
                total_debit
            );
        }
    } else if is_sell {
        let current_qty = portfolio
            .holdings
            .get(&req.symbol.to_uppercase())
            .copied()
            .unwrap_or(Decimal::ZERO);
        let requested_qty = Decimal::from(req.quantity);
        if current_qty < requested_qty {
            bail!(
                "Cannot sell {} shares of {} — current virtual holding is only {} shares",
                req.quantity,
                req.symbol,
                current_qty
            );
        }
    }

    let now = Utc::now().to_rfc3339();
    let client_order_id = format!("SIM-NSE-{}", Uuid::new_v4().simple());
    let order_id = format!("ord-{}", Uuid::new_v4().simple());
    let venue_order_id = format!("SIMULATOR-FILL-{}", Uuid::new_v4().simple());

    let order_record = IndiaSandboxOrderRecord {
        id: order_id,
        client_order_id,
        venue_order_id: Some(venue_order_id),
        symbol: instrument.symbol.clone(),
        side: req.side.to_uppercase(),
        order_type: req.order_type.to_uppercase(),
        quantity: req.quantity,
        price: fill_price.to_string(),
        trigger_price: None,
        status: "ACKNOWLEDGED".to_string(),
        fill_quantity: req.quantity,
        average_fill_price: Some(fill_price.to_string()),
        rejection_reason: None,
        created_at: now.clone(),
        updated_at: now,
    };

    store.save_india_sandbox_order(&order_record).await?;

    info!(
        symbol = %instrument.symbol,
        side = %order_record.side,
        qty = req.quantity,
        price = %fill_price,
        taxes = %costs.total,
        "Paper simulator executed virtual trade (Zero live orders sent to Upstox)"
    );

    let updated_orders = store
        .get_india_sandbox_orders(200)
        .await
        .unwrap_or_default();
    let updated_portfolio =
        compute_portfolio_accounting(base_capital, &updated_orders, &HashMap::new());

    Ok(IndiaPaperOrderResult {
        order: order_record,
        costs,
        available_cash_inr: updated_portfolio.available_cash_inr,
        total_equity_inr: updated_portfolio.total_equity_inr,
        disclaimer: "Executed in local Paper Trading Simulator against Upstox Live Market Data. Real-money live order placement is blocked by construction.".to_string(),
    })
}

/// Compute exact virtual cash, holdings, and P&L from SQLite order records.
pub fn compute_portfolio_accounting(
    base_capital: Decimal,
    orders: &[IndiaSandboxOrderRecord],
    current_prices: &HashMap<String, Decimal>,
) -> IndiaPaperPortfolioState {
    // Lots store entry price, remaining quantity, and entry fee per share.
    let mut holdings_lots: HashMap<String, Vec<(Decimal, Decimal, Decimal)>> = HashMap::new();
    let mut realized_profit = Decimal::ZERO;

    // Chronological order processing (oldest to newest)
    let mut sorted = orders.to_vec();
    sorted.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    for o in &sorted {
        if o.status.eq_ignore_ascii_case("REJECTED") || o.status.eq_ignore_ascii_case("CANCELLED") {
            continue;
        }

        let qty = Decimal::from(o.fill_quantity);
        if qty <= Decimal::ZERO {
            continue;
        }

        let fill_price = o
            .average_fill_price
            .as_deref()
            .and_then(|p| Decimal::from_str(p).ok())
            .unwrap_or_else(|| Decimal::from_str(&o.price).unwrap_or(Decimal::ZERO));

        let sym = o.symbol.to_uppercase();
        let side = if o.side.eq_ignore_ascii_case("BUY") {
            Side::Buy
        } else {
            Side::Sell
        };
        let fees = IndianCostConfig::default()
            .calculate(fill_price * qty, side)
            .total;

        if o.side.eq_ignore_ascii_case("BUY") {
            holdings_lots
                .entry(sym)
                .or_default()
                .push((fill_price, qty, fees / qty));
        } else if o.side.eq_ignore_ascii_case("SELL") {
            if let Some(buys) = holdings_lots.get_mut(&sym) {
                let mut remaining_to_sell = qty;
                let mut gross_realized = Decimal::ZERO;
                let mut allocated_buy_fees = Decimal::ZERO;
                while remaining_to_sell > Decimal::ZERO && !buys.is_empty() {
                    let (buy_p, buy_q, buy_fee_per_share) = buys.pop().unwrap();
                    if buy_q <= remaining_to_sell {
                        gross_realized += (fill_price - buy_p) * buy_q;
                        allocated_buy_fees += buy_fee_per_share * buy_q;
                        remaining_to_sell -= buy_q;
                    } else {
                        gross_realized += (fill_price - buy_p) * remaining_to_sell;
                        allocated_buy_fees += buy_fee_per_share * remaining_to_sell;
                        buys.push((buy_p, buy_q - remaining_to_sell, buy_fee_per_share));
                        remaining_to_sell = Decimal::ZERO;
                    }
                }
                realized_profit += gross_realized - allocated_buy_fees - fees;
            }
        }
    }

    let mut open_positions_cost = Decimal::ZERO;
    let mut holdings_qty: HashMap<String, Decimal> = HashMap::new();
    let mut open_positions = Vec::new();

    for (sym, lots) in &holdings_lots {
        let total_q: Decimal = lots.iter().map(|(_, q, _)| *q).sum();
        if total_q > Decimal::ZERO {
            let shares_cost: Decimal = lots.iter().map(|(p, q, _)| *p * *q).sum();
            let entry_fees: Decimal = lots
                .iter()
                .map(|(_, q, fee_per_share)| *q * *fee_per_share)
                .sum();
            let total_cost = shares_cost + entry_fees;
            let avg_p = shares_cost / total_q;
            open_positions_cost += total_cost;
            holdings_qty.insert(sym.clone(), total_q);

            let mark_price = current_prices.get(sym).copied().unwrap_or(avg_p);
            let current_val = mark_price * total_q;
            let estimated_exit_fees = IndianCostConfig::default()
                .calculate(current_val, Side::Sell)
                .total;
            let unrealized = current_val - total_cost - estimated_exit_fees;
            let unrealized_pct = if total_cost > Decimal::ZERO {
                (unrealized / total_cost) * dec!(100)
            } else {
                Decimal::ZERO
            };

            open_positions.push(IndiaOpenPosition {
                symbol: sym.clone(),
                quantity: total_q.to_string().parse::<i64>().unwrap_or(1),
                average_buy_price: avg_p,
                current_price: mark_price,
                cost_basis: total_cost,
                current_value: current_val,
                unrealized_pnl: unrealized,
                unrealized_pnl_pct: unrealized_pct,
            });
        }
    }

    let available_cash = (base_capital + realized_profit - open_positions_cost).max(Decimal::ZERO);
    let total_equity = base_capital
        + realized_profit
        + open_positions
            .iter()
            .map(|position| position.unrealized_pnl)
            .sum::<Decimal>();

    IndiaPaperPortfolioState {
        base_capital_inr: base_capital,
        available_cash_inr: available_cash,
        open_positions_cost_inr: open_positions_cost,
        realized_profit_inr: realized_profit,
        total_equity_inr: total_equity,
        holdings: holdings_qty,
        open_positions,
    }
}
