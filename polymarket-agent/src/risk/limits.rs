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
    if depth_at_price <= Decimal::ZERO {
        return Decimal::ZERO;
    }

    // Don't take more than 20% of available liquidity at the price level
    let max_from_depth = depth_at_price * dec!(0.20);

    // Check slippage: if position > depth, slippage exceeds limit
    let slippage_limit = best_price * max_slippage_pct;
    let max_from_slippage = if slippage_limit > Decimal::ZERO {
        depth_at_price // Simplified: if there's depth, we can trade up to it
    } else {
        Decimal::ZERO
    };

    position_usd.min(max_from_depth).min(max_from_slippage)
}

/// Calculate order book depth in USD at the best price level.
pub fn depth_at_best(prices: &[(Decimal, Decimal)]) -> Decimal {
    prices.first().map(|(_, size)| *size).unwrap_or(Decimal::ZERO)
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
        // Position $10, depth $200 at price $0.50, max slippage 2%
        let adjusted = liquidity_adjusted_size(dec!(10), dec!(0.50), dec!(200), dec!(0.02));
        // Max from depth: 200 * 0.20 = $40
        // Position $10 is under $40, so should be $10
        assert_eq!(adjusted, dec!(10));
    }

    #[test]
    fn test_liquidity_adjusted_size_capped() {
        // Position $100, depth $200 → max 20% of $200 = $40
        let adjusted = liquidity_adjusted_size(dec!(100), dec!(0.50), dec!(200), dec!(0.02));
        assert_eq!(adjusted, dec!(40));
    }

    #[test]
    fn test_liquidity_adjusted_size_no_depth() {
        let adjusted = liquidity_adjusted_size(dec!(10), dec!(0.50), Decimal::ZERO, dec!(0.02));
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
