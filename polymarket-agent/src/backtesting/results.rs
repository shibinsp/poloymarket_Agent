//! Backtesting result tracking and analysis.
//!
//! Tracks simulated P&L, max drawdown, win rate, edge accuracy,
//! and other statistics across a backtest run.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::fmt;

use crate::market::models::Side;

/// A single simulated trade in the backtest.
#[derive(Debug, Clone)]
pub struct SimulatedTrade {
    pub market_id: String,
    pub question: String,
    pub side: Side,
    pub entry_price: Decimal,
    pub size_usd: Decimal,
    pub shares: Decimal,
    pub fair_value: Decimal,
    pub edge: Decimal,
    pub confidence: Decimal,
    /// The actual outcome price (1.0 for win, 0.0 for loss).
    pub outcome_price: Option<Decimal>,
    /// Realized P&L after resolution.
    pub pnl: Option<Decimal>,
}

impl SimulatedTrade {
    /// Resolve the trade with an outcome and compute P&L.
    pub fn resolve(&mut self, outcome_price: Decimal) {
        self.outcome_price = Some(outcome_price);
        // P&L = shares * (outcome_price - entry_price)
        self.pnl = Some(self.shares * (outcome_price - self.entry_price));
    }

    pub fn is_resolved(&self) -> bool {
        self.pnl.is_some()
    }

    pub fn is_win(&self) -> bool {
        self.pnl.map(|p| p > Decimal::ZERO).unwrap_or(false)
    }
}

/// Aggregated results from a backtest run.
#[derive(Debug, Clone)]
pub struct BacktestResults {
    pub total_trades: u64,
    pub wins: u64,
    pub losses: u64,
    pub win_rate: Decimal,
    pub total_pnl: Decimal,
    pub max_drawdown: Decimal,
    pub max_drawdown_pct: Decimal,
    pub peak_balance: Decimal,
    pub final_balance: Decimal,
    pub initial_balance: Decimal,
    pub roi_pct: Decimal,
    pub avg_edge: Decimal,
    pub avg_pnl_per_trade: Decimal,
    pub sharpe_ratio: Option<Decimal>,
    pub profit_factor: Decimal,
    pub edge_accuracy: Decimal,
    pub total_api_cost: Decimal,
    pub net_profit: Decimal,
}

impl fmt::Display for BacktestResults {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "=== Backtest Results ===\n\
             Trades: {} ({}W / {}L, {:.1}% win rate)\n\
             P&L: ${} total, ${} net (after ${} API costs)\n\
             ROI: {:.1}% | Sharpe: {}\n\
             Max Drawdown: ${} ({:.1}%)\n\
             Peak Balance: ${} | Final: ${}\n\
             Avg Edge: {:.1}% | Edge Accuracy: {:.1}%\n\
             Profit Factor: {:.2} | Avg P&L/Trade: ${}",
            self.total_trades,
            self.wins,
            self.losses,
            self.win_rate * dec!(100),
            self.total_pnl,
            self.net_profit,
            self.total_api_cost,
            self.roi_pct * dec!(100),
            self.sharpe_ratio
                .map(|s| format!("{:.2}", s))
                .unwrap_or_else(|| "N/A".to_string()),
            self.max_drawdown,
            self.max_drawdown_pct * dec!(100),
            self.peak_balance,
            self.final_balance,
            self.avg_edge * dec!(100),
            self.edge_accuracy * dec!(100),
            self.profit_factor,
            self.avg_pnl_per_trade,
        )
    }
}

/// Tracks running state during a backtest.
pub struct BacktestTracker {
    initial_balance: Decimal,
    balance: Decimal,
    peak_balance: Decimal,
    max_drawdown: Decimal,
    trades: Vec<SimulatedTrade>,
    total_api_cost: Decimal,
}

impl BacktestTracker {
    pub fn new(initial_balance: Decimal) -> Self {
        Self {
            initial_balance,
            balance: initial_balance,
            peak_balance: initial_balance,
            max_drawdown: Decimal::ZERO,
            trades: Vec::new(),
            total_api_cost: Decimal::ZERO,
        }
    }

    /// Record a new trade entry. Deducts cost from balance.
    pub fn record_entry(&mut self, trade: SimulatedTrade) {
        let cost = trade.entry_price * trade.shares;
        self.balance -= cost;
        self.trades.push(trade);
    }

    /// Record API cost for this cycle.
    pub fn record_api_cost(&mut self, cost: Decimal) {
        self.total_api_cost += cost;
        self.balance -= cost;
    }

    /// Resolve the last N unresolved trades with outcomes.
    pub fn resolve_trade(&mut self, index: usize, outcome_price: Decimal) {
        if index < self.trades.len() {
            self.trades[index].resolve(outcome_price);
            if self.trades[index].pnl.is_some() {
                // Return shares * outcome_price (payout)
                let payout =
                    self.trades[index].shares * outcome_price;
                self.balance += payout;

                // Track peak and drawdown
                if self.balance > self.peak_balance {
                    self.peak_balance = self.balance;
                }
                let drawdown = self.peak_balance - self.balance;
                if drawdown > self.max_drawdown {
                    self.max_drawdown = drawdown;
                }
            }
        }
    }

    /// Current balance.
    pub fn balance(&self) -> Decimal {
        self.balance
    }

    /// Number of trades recorded.
    pub fn trade_count(&self) -> usize {
        self.trades.len()
    }

    /// Compute final results.
    pub fn finalize(&self) -> BacktestResults {
        let resolved: Vec<&SimulatedTrade> =
            self.trades.iter().filter(|t| t.is_resolved()).collect();

        let total_trades = resolved.len() as u64;
        let wins = resolved.iter().filter(|t| t.is_win()).count() as u64;
        let losses = total_trades.saturating_sub(wins);

        let win_rate = if total_trades > 0 {
            Decimal::from(wins) / Decimal::from(total_trades)
        } else {
            Decimal::ZERO
        };

        let total_pnl: Decimal = resolved
            .iter()
            .filter_map(|t| t.pnl)
            .sum();

        let avg_pnl_per_trade = if total_trades > 0 {
            total_pnl / Decimal::from(total_trades)
        } else {
            Decimal::ZERO
        };

        let avg_edge = if total_trades > 0 {
            let total_edge: Decimal = resolved.iter().map(|t| t.edge).sum();
            total_edge / Decimal::from(total_trades)
        } else {
            Decimal::ZERO
        };

        // Edge accuracy: how often the predicted direction was correct
        let correct_predictions = resolved
            .iter()
            .filter(|t| {
                if let Some(pnl) = t.pnl {
                    // Edge predicted positive return and trade was profitable
                    (t.edge > Decimal::ZERO && pnl > Decimal::ZERO)
                        || (t.edge <= Decimal::ZERO && pnl <= Decimal::ZERO)
                } else {
                    false
                }
            })
            .count() as u64;

        let edge_accuracy = if total_trades > 0 {
            Decimal::from(correct_predictions) / Decimal::from(total_trades)
        } else {
            Decimal::ZERO
        };

        // Profit factor: gross_profit / gross_loss
        let gross_profit: Decimal = resolved
            .iter()
            .filter_map(|t| t.pnl)
            .filter(|p| *p > Decimal::ZERO)
            .sum();
        let gross_loss: Decimal = resolved
            .iter()
            .filter_map(|t| t.pnl)
            .filter(|p| *p < Decimal::ZERO)
            .map(|p| p.abs())
            .sum();

        let profit_factor = if gross_loss > Decimal::ZERO {
            gross_profit / gross_loss
        } else if gross_profit > Decimal::ZERO {
            dec!(999.99) // Infinite profit factor capped
        } else {
            Decimal::ZERO
        };

        let max_drawdown_pct = if self.peak_balance > Decimal::ZERO {
            self.max_drawdown / self.peak_balance
        } else {
            Decimal::ZERO
        };

        let roi_pct = if self.initial_balance > Decimal::ZERO {
            (self.balance - self.initial_balance) / self.initial_balance
        } else {
            Decimal::ZERO
        };

        let net_profit = total_pnl - self.total_api_cost;

        // Sharpe ratio from per-trade P&L
        let pnl_values: Vec<Decimal> = resolved.iter().filter_map(|t| t.pnl).collect();
        let sharpe_ratio = compute_sharpe(&pnl_values);

        BacktestResults {
            total_trades,
            wins,
            losses,
            win_rate,
            total_pnl,
            max_drawdown: self.max_drawdown,
            max_drawdown_pct,
            peak_balance: self.peak_balance,
            final_balance: self.balance,
            initial_balance: self.initial_balance,
            roi_pct,
            avg_edge,
            avg_pnl_per_trade,
            sharpe_ratio,
            profit_factor,
            edge_accuracy,
            total_api_cost: self.total_api_cost,
            net_profit,
        }
    }
}

/// Compute Sharpe ratio from per-trade P&L values.
fn compute_sharpe(pnl_values: &[Decimal]) -> Option<Decimal> {
    if pnl_values.len() < 2 {
        return None;
    }

    let n = Decimal::from(pnl_values.len() as u64);
    let sum: Decimal = pnl_values.iter().sum();
    let mean = sum / n;

    let variance_sum: Decimal = pnl_values
        .iter()
        .map(|p| {
            let diff = *p - mean;
            diff * diff
        })
        .sum();

    let variance = variance_sum / (n - Decimal::ONE);
    if variance <= Decimal::ZERO {
        return None;
    }

    // Newton's method sqrt
    let mut guess = variance / dec!(2);
    for _ in 0..20 {
        let next = (guess + variance / guess) / dec!(2);
        if (next - guess).abs() < dec!(0.0000001) {
            let std_dev = next;
            if std_dev <= Decimal::ZERO {
                return None;
            }
            return Some(mean / std_dev);
        }
        guess = next;
    }

    let std_dev = guess;
    if std_dev <= Decimal::ZERO {
        return None;
    }
    Some(mean / std_dev)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_trade(edge: Decimal, entry_price: Decimal, size_usd: Decimal) -> SimulatedTrade {
        let shares = size_usd / entry_price;
        SimulatedTrade {
            market_id: "m1".to_string(),
            question: "Test?".to_string(),
            side: Side::Yes,
            entry_price,
            size_usd,
            shares,
            fair_value: entry_price + edge,
            edge,
            confidence: dec!(0.8),
            outcome_price: None,
            pnl: None,
        }
    }

    #[test]
    fn test_simulated_trade_resolve_win() {
        let mut trade = make_trade(dec!(0.10), dec!(0.60), dec!(6));
        trade.resolve(Decimal::ONE); // YES outcome
        assert!(trade.is_resolved());
        assert!(trade.is_win());
        // shares = 6/0.60 = 10, pnl = 10 * (1.0 - 0.60) = 4.0
        assert_eq!(trade.pnl, Some(dec!(4)));
    }

    #[test]
    fn test_simulated_trade_resolve_loss() {
        let mut trade = make_trade(dec!(0.10), dec!(0.60), dec!(6));
        trade.resolve(Decimal::ZERO); // NO outcome
        assert!(trade.is_resolved());
        assert!(!trade.is_win());
        // shares = 10, pnl = 10 * (0.0 - 0.60) = -6.0
        assert_eq!(trade.pnl, Some(dec!(-6)));
    }

    #[test]
    fn test_tracker_basic_flow() {
        let mut tracker = BacktestTracker::new(dec!(100));

        // Enter trade: buy 10 shares at $0.60 = $6 cost
        let trade = make_trade(dec!(0.10), dec!(0.60), dec!(6));
        tracker.record_entry(trade);
        assert_eq!(tracker.balance(), dec!(94)); // 100 - 6

        // Resolve as win (payout = 10 * 1.0 = $10)
        tracker.resolve_trade(0, Decimal::ONE);
        assert_eq!(tracker.balance(), dec!(104)); // 94 + 10

        let results = tracker.finalize();
        assert_eq!(results.total_trades, 1);
        assert_eq!(results.wins, 1);
        assert_eq!(results.losses, 0);
        assert_eq!(results.win_rate, Decimal::ONE);
    }

    #[test]
    fn test_tracker_drawdown() {
        let mut tracker = BacktestTracker::new(dec!(100));

        // Trade 1: win
        let trade1 = make_trade(dec!(0.10), dec!(0.50), dec!(10));
        tracker.record_entry(trade1); // balance: 90
        tracker.resolve_trade(0, Decimal::ONE); // payout: 20 shares * 1.0 = 20, balance: 110

        // Trade 2: loss
        let trade2 = make_trade(dec!(0.10), dec!(0.50), dec!(10));
        tracker.record_entry(trade2); // balance: 100
        tracker.resolve_trade(1, Decimal::ZERO); // payout: 0, balance: 100

        let results = tracker.finalize();
        assert_eq!(results.peak_balance, dec!(110));
        assert_eq!(results.max_drawdown, dec!(10)); // 110 -> 100
    }

    #[test]
    fn test_tracker_api_costs() {
        let mut tracker = BacktestTracker::new(dec!(100));
        tracker.record_api_cost(dec!(0.05));
        tracker.record_api_cost(dec!(0.03));

        let results = tracker.finalize();
        assert_eq!(results.total_api_cost, dec!(0.08));
        assert_eq!(results.final_balance, dec!(99.92));
    }

    #[test]
    fn test_results_display() {
        let mut tracker = BacktestTracker::new(dec!(100));
        let trade = make_trade(dec!(0.10), dec!(0.60), dec!(6));
        tracker.record_entry(trade);
        tracker.resolve_trade(0, Decimal::ONE);

        let results = tracker.finalize();
        let display = format!("{results}");
        assert!(display.contains("Backtest Results"));
        assert!(display.contains("100.0% win rate"));
    }

    #[test]
    fn test_profit_factor() {
        let mut tracker = BacktestTracker::new(dec!(1000));

        // Win: $4 profit
        let t1 = make_trade(dec!(0.10), dec!(0.60), dec!(6));
        tracker.record_entry(t1);
        tracker.resolve_trade(0, Decimal::ONE);

        // Loss: $6 loss
        let t2 = make_trade(dec!(0.10), dec!(0.60), dec!(6));
        tracker.record_entry(t2);
        tracker.resolve_trade(1, Decimal::ZERO);

        let results = tracker.finalize();
        // Profit factor = 4.0 / 6.0 = 0.6667
        assert!(results.profit_factor > dec!(0.66));
        assert!(results.profit_factor < dec!(0.67));
    }
}
