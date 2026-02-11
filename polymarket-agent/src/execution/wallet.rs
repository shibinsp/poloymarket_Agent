//! Wallet and balance management.
//!
//! Tracks effective bankroll accounting for API costs and
//! determines available capital for trading.

use anyhow::Result;
use rust_decimal::Decimal;
use tracing::info;

use crate::market::polymarket::PolymarketClient;

/// Calculate the effective bankroll available for trading.
///
/// effective = wallet_balance - api_reserve - unrealized_exposure
pub fn effective_bankroll(
    wallet_balance: Decimal,
    api_reserve: Decimal,
    unrealized_exposure: Decimal,
) -> Decimal {
    let available = wallet_balance - api_reserve - unrealized_exposure;
    if available < Decimal::ZERO {
        Decimal::ZERO
    } else {
        available
    }
}

/// Estimate the remaining number of cycles the agent can sustain
/// at the current API cost rate.
pub fn estimated_cycles_remaining(
    wallet_balance: Decimal,
    avg_cost_per_cycle: Decimal,
    min_operating_balance: Decimal,
) -> u64 {
    if avg_cost_per_cycle <= Decimal::ZERO {
        return u64::MAX; // No cost → infinite cycles
    }

    let available = wallet_balance - min_operating_balance;
    if available <= Decimal::ZERO {
        return 0;
    }

    // Use to_string + parse to avoid TryFrom<Decimal> for u64 issues
    let cycles = available / avg_cost_per_cycle;
    cycles
        .to_string()
        .parse::<f64>()
        .map(|f| f.floor() as u64)
        .unwrap_or(0)
}

/// Log a balance summary.
pub async fn log_balance_summary(
    client: &PolymarketClient,
    api_reserve: Decimal,
    unrealized: Decimal,
) -> Result<()> {
    let balance = client.get_balance().await?;
    let effective = effective_bankroll(balance, api_reserve, unrealized);

    info!(
        wallet = %balance,
        api_reserve = %api_reserve,
        unrealized_exposure = %unrealized,
        effective_bankroll = %effective,
        "Balance summary"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_effective_bankroll_normal() {
        let effective = effective_bankroll(dec!(100), dec!(10), dec!(20));
        assert_eq!(effective, dec!(70));
    }

    #[test]
    fn test_effective_bankroll_insufficient() {
        let effective = effective_bankroll(dec!(10), dec!(10), dec!(20));
        assert_eq!(effective, Decimal::ZERO);
    }

    #[test]
    fn test_effective_bankroll_no_exposure() {
        let effective = effective_bankroll(dec!(100), dec!(5), Decimal::ZERO);
        assert_eq!(effective, dec!(95));
    }

    #[test]
    fn test_estimated_cycles_remaining() {
        // $100 balance, $0.05 per cycle, $10 min operating
        // ($100 - $10) / $0.05 = 1800 cycles
        let cycles = estimated_cycles_remaining(dec!(100), dec!(0.05), dec!(10));
        assert_eq!(cycles, 1800);
    }

    #[test]
    fn test_estimated_cycles_zero_cost() {
        let cycles = estimated_cycles_remaining(dec!(100), Decimal::ZERO, dec!(10));
        assert_eq!(cycles, u64::MAX);
    }

    #[test]
    fn test_estimated_cycles_insufficient_balance() {
        let cycles = estimated_cycles_remaining(dec!(5), dec!(0.05), dec!(10));
        assert_eq!(cycles, 0);
    }
}
