//! Risk limits, max exposure, and drawdown tracking.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Check if the order book has sufficient liquidity for the position size.
/// Returns the maximum safely tradeable size.
pub fn liquidity_adjusted_size(
    position_usd: Decimal,
    best_price: Decimal,
    depth_at_price: Decimal,
    max_slippage_pct: Decimal,
) -> Decimal {
    if depth_at_price <= Decimal::ZERO || best_price <= Decimal::ZERO {
        return Decimal::ZERO;
    }

    // `depth_at_price` is denominated in shares (see `depth_at_best`), while
    // `position_usd` is a dollar amount. Convert to a dollar notional via
    // `best_price` before comparing against it, so both caps below are in
    // the same unit as `position_usd`.
    let notional_depth = depth_at_price * best_price;

    // Don't take more than 20% of available liquidity (in dollar terms) at
    // the price level.
    let max_from_depth = notional_depth * dec!(0.20);

    // Slippage cap: assume walking the book consumes depth roughly linearly,
    // so trading no more than `max_slippage_pct` of the notional depth at the
    // best price keeps expected price impact within the configured
    // tolerance.
    let max_from_slippage = notional_depth * max_slippage_pct;

    position_usd.min(max_from_depth).min(max_from_slippage)
}

/// Calculate order book depth in shares at the best price level.
pub fn depth_at_best(prices: &[(Decimal, Decimal)]) -> Decimal {
    prices
        .first()
        .map(|(_, size)| *size)
        .unwrap_or(Decimal::ZERO)
}

/// Calculate total depth across all levels in USD.
pub fn total_depth(prices: &[(Decimal, Decimal)]) -> Decimal {
    prices.iter().map(|(price, size)| price * size).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_liquidity_adjusted_size_normal() {
        // Position $1, depth 200 shares @ $0.50 ($100 notional), max slippage 2%
        // Depth cap: $100 * 0.20 = $20. Slippage cap: $100 * 0.02 = $2.
        // Position $1 is under both, so should be unconstrained at $1.
        let adjusted = liquidity_adjusted_size(dec!(1), dec!(0.50), dec!(200), dec!(0.02));
        assert_eq!(adjusted, dec!(1));
    }

    #[test]
    fn test_liquidity_adjusted_size_capped_by_slippage() {
        // Depth 200 shares @ $0.50 ($100 notional), max slippage 2% → cap $2.
        // Depth cap ($100 * 0.20 = $20) is looser, so slippage binds.
        let adjusted = liquidity_adjusted_size(dec!(10), dec!(0.50), dec!(200), dec!(0.02));
        assert_eq!(adjusted, dec!(2));
    }

    #[test]
    fn test_liquidity_adjusted_size_capped_by_depth() {
        // Thin book: 10 shares @ $0.50 ($5 notional). Depth cap: $5 * 0.20 = $1.
        // With a loose 50% slippage tolerance, slippage cap = $5 * 0.50 = $2.5,
        // so the tighter 20%-of-depth rule binds instead.
        let adjusted = liquidity_adjusted_size(dec!(100), dec!(0.50), dec!(10), dec!(0.50));
        assert_eq!(adjusted, dec!(1));
    }

    #[test]
    fn test_liquidity_adjusted_size_depth_cap_uses_dollars_not_shares() {
        // Regression test for a units mismatch: depth_at_price is in shares,
        // so a low-priced market with a large raw share count must not let
        // the 20%-of-depth cap be computed from the raw share count. 1000
        // shares @ $0.01 is only $10 of real notional depth, so the depth cap
        // should be $10 * 0.20 = $2 — not $200 (1000 * 0.20), which a
        // shares-based calculation would give and which would never bind
        // here, wrongly letting the looser 30% slippage cap ($3) win instead.
        let adjusted = liquidity_adjusted_size(dec!(50), dec!(0.01), dec!(1000), dec!(0.30));
        assert_eq!(adjusted, dec!(2));
    }

    #[test]
    fn test_liquidity_adjusted_size_no_depth() {
        let adjusted = liquidity_adjusted_size(dec!(10), dec!(0.50), Decimal::ZERO, dec!(0.02));
        assert_eq!(adjusted, Decimal::ZERO);
    }

    #[test]
    fn test_liquidity_adjusted_size_zero_price() {
        let adjusted = liquidity_adjusted_size(dec!(10), Decimal::ZERO, dec!(200), dec!(0.02));
        assert_eq!(adjusted, Decimal::ZERO);
    }

    #[test]
    fn test_depth_at_best() {
        let levels = vec![(dec!(0.50), dec!(100)), (dec!(0.49), dec!(200))];
        assert_eq!(depth_at_best(&levels), dec!(100));
    }

    #[test]
    fn test_total_depth() {
        let levels = vec![(dec!(0.50), dec!(100)), (dec!(0.49), dec!(200))];
        // 0.50*100 + 0.49*200 = 50 + 98 = 148
        assert_eq!(total_depth(&levels), dec!(148));
    }
}
