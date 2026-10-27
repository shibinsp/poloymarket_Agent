//! US statutory, regulatory and transaction cost schedule.
//!
//! Models date-effective transaction costs for US-listed cash equities and ETFs:
//! - SEC Section 31 Fee (applies to SELL orders only, e.g. $27.80 per million)
//! - FINRA Trading Activity Fee (TAF, applies to SELL orders only, $0.000166/share capped at $8.30)
//! - Brokerage / Commission (zero for modern retail/paper brokers, or configurable)
//! - Half-spread and market slippage modeling

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::venue::types::Side;

/// Configuration for US transaction fee schedule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsCostConfig {
    /// Broker commission per share (zero for Alpaca, Robinhood, etc.).
    pub commission_per_share: Decimal,
    /// Minimum commission per order (zero for zero-commission brokers).
    pub min_commission: Decimal,
    /// SEC Section 31 fee rate on SELL principal (e.g. $27.80 per million = 0.0000278).
    pub sec_fee_rate: Decimal,
    /// FINRA Trading Activity Fee (TAF) per share on SELL (e.g. $0.000166).
    pub finra_taf_per_share: Decimal,
    /// FINRA TAF maximum cap per trade (e.g. $8.30).
    pub finra_taf_max: Decimal,
    /// Modeled half-spread percentage (e.g. 0.01% = 0.0001 for mega-caps like SPY/AAPL).
    pub half_spread_pct: Decimal,
    /// Modeled execution slippage percentage (e.g. 0.02% = 0.0002).
    pub slippage_pct: Decimal,
}

impl Default for UsCostConfig {
    fn default() -> Self {
        Self {
            commission_per_share: Decimal::ZERO,
            min_commission: Decimal::ZERO,
            sec_fee_rate: dec!(0.0000278),     // $27.80 per million (SEC FY24/25 rate)
            finra_taf_per_share: dec!(0.000166), // FINRA TAF rate per share
            finra_taf_max: dec!(8.30),          // $8.30 maximum per trade
            half_spread_pct: dec!(0.0001),      // 0.01% typical spread for SPY/AAPL
            slippage_pct: dec!(0.0002),         // 0.02% conservative slippage
        }
    }
}

/// Itemized transaction costs breakdown for one US execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsTransactionCosts {
    pub trade_value: Decimal,
    pub shares: Decimal,
    pub commission: Decimal,
    pub sec_fee: Decimal,
    pub finra_taf: Decimal,
    pub estimated_spread_cost: Decimal,
    pub estimated_slippage_cost: Decimal,
    pub total_regulatory_fees: Decimal,
    pub total: Decimal,
}

impl UsCostConfig {
    /// Compute all statutory, regulatory and execution costs for a US trade.
    pub fn calculate(&self, trade_value: Decimal, shares: Decimal, side: Side) -> UsTransactionCosts {
        if trade_value <= Decimal::ZERO || shares <= Decimal::ZERO {
            return UsTransactionCosts {
                trade_value: Decimal::ZERO,
                shares: Decimal::ZERO,
                commission: Decimal::ZERO,
                sec_fee: Decimal::ZERO,
                finra_taf: Decimal::ZERO,
                estimated_spread_cost: Decimal::ZERO,
                estimated_slippage_cost: Decimal::ZERO,
                total_regulatory_fees: Decimal::ZERO,
                total: Decimal::ZERO,
            };
        }

        // 1. Commission
        let raw_commission = shares * self.commission_per_share;
        let commission = if raw_commission > Decimal::ZERO && raw_commission < self.min_commission {
            self.min_commission
        } else {
            raw_commission
        };

        // 2. SEC Section 31 Fee (Sell side only, rounded up to nearest cent)
        let sec_fee = match side {
            Side::Buy => Decimal::ZERO,
            Side::Sell => {
                let raw_sec = trade_value * self.sec_fee_rate;
                if raw_sec > Decimal::ZERO {
                    // SEC rules require rounding up to the next cent
                    (raw_sec * dec!(100)).ceil() / dec!(100)
                } else {
                    Decimal::ZERO
                }
            }
        };

        // 3. FINRA Trading Activity Fee (Sell side only, rounded up to nearest cent, capped at max)
        let finra_taf = match side {
            Side::Buy => Decimal::ZERO,
            Side::Sell => {
                let raw_taf = (shares * self.finra_taf_per_share).min(self.finra_taf_max);
                if raw_taf > Decimal::ZERO {
                    let rounded = (raw_taf * dec!(100)).ceil() / dec!(100);
                    rounded.max(dec!(0.01)) // minimum $0.01 if shares > 0
                } else {
                    Decimal::ZERO
                }
            }
        };

        // 4. Spread and Slippage
        let estimated_spread_cost = trade_value * self.half_spread_pct;
        let estimated_slippage_cost = trade_value * self.slippage_pct;

        let total_regulatory_fees = sec_fee + finra_taf;
        let total = commission + total_regulatory_fees + estimated_spread_cost + estimated_slippage_cost;

        UsTransactionCosts {
            trade_value,
            shares,
            commission: commission.round_dp(2),
            sec_fee: sec_fee.round_dp(2),
            finra_taf: finra_taf.round_dp(2),
            estimated_spread_cost: estimated_spread_cost.round_dp(2),
            estimated_slippage_cost: estimated_slippage_cost.round_dp(2),
            total_regulatory_fees: total_regulatory_fees.round_dp(2),
            total: total.round_dp(2),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates_us_costs_for_buy_and_sell() {
        let config = UsCostConfig::default();
        let trade_value = dec!(50000); // $50,000 notional (e.g. 100 shares of SPY @ $500)
        let shares = dec!(100);

        let buy = config.calculate(trade_value, shares, Side::Buy);
        // On Buy: SEC and FINRA fees are $0
        assert_eq!(buy.sec_fee, Decimal::ZERO);
        assert_eq!(buy.finra_taf, Decimal::ZERO);
        assert_eq!(buy.commission, Decimal::ZERO);
        // Spread $5, slippage $10 = total $15
        assert_eq!(buy.total, dec!(15.00));

        let sell = config.calculate(trade_value, shares, Side::Sell);
        // On Sell: SEC fee $50,000 * 0.0000278 = $1.39
        assert_eq!(sell.sec_fee, dec!(1.39));
        // FINRA TAF: 100 * $0.000166 = $0.0166 -> $0.02
        assert_eq!(sell.finra_taf, dec!(0.02));
        assert_eq!(sell.total_regulatory_fees, dec!(1.41));
        assert_eq!(sell.total, dec!(16.41));
    }
}
