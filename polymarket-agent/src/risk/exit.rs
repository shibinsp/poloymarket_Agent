//! Position re-evaluation and exit strategy.
//!
//! Evaluates open positions against current market prices to determine
//! if a stop-loss or other exit condition has been triggered.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::info;

use crate::market::models::Side;

/// Result of evaluating whether a position should be exited.
#[derive(Debug, Clone)]
pub struct ExitSignal {
    /// Market ID for the position.
    pub market_id: String,
    /// Whether the position should be exited.
    pub should_exit: bool,
    /// Current unrealized P&L as a percentage of entry.
    pub pnl_pct: Decimal,
    /// Reason for the exit signal (or "hold" if no exit).
    pub reason: String,
}

/// Default maximum loss before triggering a stop-loss exit.
pub const DEFAULT_MAX_LOSS_PCT: Decimal = dec!(0.20);

/// Evaluate whether an open position should be exited.
///
/// Currently implements a simple stop-loss: if unrealized loss exceeds
/// `max_loss_pct` of the entry price, signal an exit.
///
/// Future enhancements could include:
/// - Take-profit levels
/// - Re-valuation with updated Claude probability
/// - Time-based exits (approaching resolution with no edge)
pub fn evaluate_exit(
    market_id: &str,
    entry_price: Decimal,
    side: Side,
    current_midpoint: Decimal,
    max_loss_pct: Decimal,
) -> ExitSignal {
    if entry_price <= Decimal::ZERO {
        return ExitSignal {
            market_id: market_id.to_string(),
            should_exit: false,
            pnl_pct: Decimal::ZERO,
            reason: "Invalid entry price".to_string(),
        };
    }

    // Calculate P&L percentage
    let pnl_pct = match side {
        Side::Yes => (current_midpoint - entry_price) / entry_price,
        Side::No => {
            // For NO side, we bought at (1 - midpoint), so track against that
            let effective_entry = Decimal::ONE - entry_price;
            let effective_current = Decimal::ONE - current_midpoint;
            if effective_entry > Decimal::ZERO {
                (effective_current - effective_entry) / effective_entry
            } else {
                Decimal::ZERO
            }
        }
    };

    // Stop-loss check
    if pnl_pct < -max_loss_pct {
        let reason = format!(
            "Stop-loss triggered: unrealized loss {:.1}% exceeds max {:.1}%",
            pnl_pct * dec!(100),
            max_loss_pct * dec!(100)
        );
        info!(
            market_id,
            pnl_pct = %pnl_pct,
            max_loss_pct = %max_loss_pct,
            "EXIT SIGNAL: {}", reason
        );
        return ExitSignal {
            market_id: market_id.to_string(),
            should_exit: true,
            pnl_pct,
            reason,
        };
    }

    ExitSignal {
        market_id: market_id.to_string(),
        should_exit: false,
        pnl_pct,
        reason: format!("Hold — P&L {:.1}% within tolerance", pnl_pct * dec!(100)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_exit_within_tolerance() {
        let signal = evaluate_exit("mkt1", dec!(0.50), Side::Yes, dec!(0.45), dec!(0.20));
        assert!(!signal.should_exit);
    }

    #[test]
    fn test_exit_on_stop_loss() {
        // Bought YES at 0.60, now at 0.40 → -33% loss → exceeds 20%
        let signal = evaluate_exit("mkt1", dec!(0.60), Side::Yes, dec!(0.40), dec!(0.20));
        assert!(signal.should_exit);
        assert!(signal.pnl_pct < -dec!(0.20));
    }

    #[test]
    fn test_no_exit_on_profit() {
        let signal = evaluate_exit("mkt1", dec!(0.50), Side::Yes, dec!(0.70), dec!(0.20));
        assert!(!signal.should_exit);
        assert!(signal.pnl_pct > Decimal::ZERO);
    }

    #[test]
    fn test_no_side_exit() {
        // Bought NO at 0.40 (effective entry for complement = 0.60)
        // Current midpoint 0.80 → complement = 0.20 → loss vs 0.60 entry
        let signal = evaluate_exit("mkt1", dec!(0.40), Side::No, dec!(0.80), dec!(0.20));
        assert!(signal.should_exit);
    }

    #[test]
    fn test_zero_entry_price() {
        let signal = evaluate_exit("mkt1", Decimal::ZERO, Side::Yes, dec!(0.50), dec!(0.20));
        assert!(!signal.should_exit);
    }
}
