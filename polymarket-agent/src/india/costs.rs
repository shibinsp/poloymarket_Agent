//! Indian statutory and regulatory transaction costs schedule.
//!
//! Models date-effective transaction taxes and fees for NSE cash equities:
//! - Securities Transaction Tax (STT)
//! - Exchange Transaction Charges (NSE)
//! - SEBI Turnover Fees
//! - State Stamp Duty
//! - Goods & Services Tax (GST @ 18%)
//! - Brokerage fees

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::venue::types::Side;

/// Configuration for Indian transaction fee schedule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndianCostConfig {
    /// Brokerage rate as a fraction (e.g. 0.0005 for 0.05%).
    pub brokerage_rate: Decimal,
    /// Maximum flat brokerage per order in INR (e.g. ₹20 for discount brokers like Upstox).
    pub max_brokerage_per_order: Decimal,
    /// Securities Transaction Tax rate on Buy (delivery: 0.1% = 0.001).
    pub stt_buy_rate: Decimal,
    /// Securities Transaction Tax rate on Sell (delivery: 0.1% = 0.001, intraday: 0.025% = 0.00025).
    pub stt_sell_rate: Decimal,
    /// NSE exchange transaction charge rate (0.00297% = 0.0000297).
    pub exchange_turnover_rate: Decimal,
    /// SEBI turnover fee (₹10 per crore = 0.000001).
    pub sebi_rate: Decimal,
    /// Stamp duty on Buy (delivery: 0.015% = 0.00015; nil on Sell).
    pub stamp_duty_buy_rate: Decimal,
    /// GST rate on (Brokerage + Exchange + SEBI) = 18% = 0.18.
    pub gst_rate: Decimal,
    /// Investor Protection Fund rate (0.0001% = 0.000001).
    pub ipft_rate: Decimal,
}

impl Default for IndianCostConfig {
    fn default() -> Self {
        Self {
            brokerage_rate: dec!(0.0005),            // 0.05%
            max_brokerage_per_order: dec!(20.0),     // Upstox standard ₹20 cap
            stt_buy_rate: dec!(0.001),               // 0.1% Delivery
            stt_sell_rate: dec!(0.001),              // 0.1% Delivery
            exchange_turnover_rate: dec!(0.0000297), // 0.00297% NSE
            sebi_rate: dec!(0.000001),               // ₹10 / crore
            stamp_duty_buy_rate: dec!(0.00015),      // 0.015% on Buy
            gst_rate: dec!(0.18),                    // 18% GST
            ipft_rate: dec!(0.000001),               // 0.0001% IPFT
        }
    }
}

/// Itemized transaction costs breakdown for one execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndianTransactionCosts {
    pub trade_value: Decimal,
    pub brokerage: Decimal,
    pub stt: Decimal,
    pub exchange_charges: Decimal,
    pub sebi_charges: Decimal,
    pub stamp_duty: Decimal,
    pub gst: Decimal,
    pub ipft: Decimal,
    pub total: Decimal,
}

impl IndianCostConfig {
    /// Compute all statutory and broker charges for an execution.
    pub fn calculate(&self, trade_value: Decimal, side: Side) -> IndianTransactionCosts {
        if trade_value <= Decimal::ZERO {
            return IndianTransactionCosts {
                trade_value: Decimal::ZERO,
                brokerage: Decimal::ZERO,
                stt: Decimal::ZERO,
                exchange_charges: Decimal::ZERO,
                sebi_charges: Decimal::ZERO,
                stamp_duty: Decimal::ZERO,
                gst: Decimal::ZERO,
                ipft: Decimal::ZERO,
                total: Decimal::ZERO,
            };
        }

        // 1. Brokerage (percentage capped at flat fee)
        let raw_brokerage = trade_value * self.brokerage_rate;
        let brokerage = if self.max_brokerage_per_order > Decimal::ZERO {
            raw_brokerage.min(self.max_brokerage_per_order)
        } else {
            raw_brokerage
        };

        // 2. STT (Securities Transaction Tax)
        let stt = match side {
            Side::Buy => trade_value * self.stt_buy_rate,
            Side::Sell => trade_value * self.stt_sell_rate,
        };

        // 3. Exchange turnover charges
        let exchange_charges = trade_value * self.exchange_turnover_rate;

        // 4. SEBI charges
        let sebi_charges = trade_value * self.sebi_rate;

        // 5. Stamp duty (applies only on Buy side)
        let stamp_duty = match side {
            Side::Buy => trade_value * self.stamp_duty_buy_rate,
            Side::Sell => Decimal::ZERO,
        };

        // 6. GST @ 18% on (Brokerage + Exchange Charges + SEBI charges)
        let gst = (brokerage + exchange_charges + sebi_charges) * self.gst_rate;

        // 7. IPFT
        let ipft = trade_value * self.ipft_rate;

        let total = brokerage + stt + exchange_charges + sebi_charges + stamp_duty + gst + ipft;

        IndianTransactionCosts {
            trade_value,
            brokerage: brokerage.round_dp(2),
            stt: stt.round_dp(2),
            exchange_charges: exchange_charges.round_dp(2),
            sebi_charges: sebi_charges.round_dp(2),
            stamp_duty: stamp_duty.round_dp(2),
            gst: gst.round_dp(2),
            ipft: ipft.round_dp(2),
            total: total.round_dp(2),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates_realistic_costs_for_buy_and_sell() {
        let config = IndianCostConfig::default();
        let trade_value = dec!(100000); // ₹1,00,000 notional

        let buy_costs = config.calculate(trade_value, Side::Buy);
        // Brokerage capped at ₹20
        assert_eq!(buy_costs.brokerage, dec!(20.00));
        // STT 0.1% = ₹100
        assert_eq!(buy_costs.stt, dec!(100.00));
        // Stamp duty 0.015% = ₹15
        assert_eq!(buy_costs.stamp_duty, dec!(15.00));
        // Total should be around ~₹140
        assert!(buy_costs.total > dec!(135.0) && buy_costs.total < dec!(145.0));

        let sell_costs = config.calculate(trade_value, Side::Sell);
        // Stamp duty on sell is 0
        assert_eq!(sell_costs.stamp_duty, dec!(0.00));
        // STT on delivery sell is ₹100
        assert_eq!(sell_costs.stt, dec!(100.00));
        assert!(sell_costs.total > dec!(120.0) && sell_costs.total < dec!(130.0));
    }
}
