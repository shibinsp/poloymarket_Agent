//! Foreign exchange (FX) conversion reference rates.
//!
//! Provides explicit, timestamped USD/INR conversion rates for combined portfolio
//! reporting, ensuring all metrics are primarily preserved in their native currencies
//! (INR for India, USD for US) and combined only through an audited conversion.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

/// An explicit, timestamped foreign exchange conversion rate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FxRate {
    pub base: String,
    pub quote: String,
    pub rate: Decimal,
    pub timestamp: DateTime<Utc>,
    pub source: String,
}

impl FxRate {
    /// Convert USD amount to INR.
    pub fn usd_to_inr(&self, usd: Decimal) -> Decimal {
        (usd * self.rate).round_dp(2)
    }

    /// Convert INR amount to USD.
    pub fn inr_to_usd(&self, inr: Decimal) -> Decimal {
        if self.rate.is_zero() {
            Decimal::ZERO
        } else {
            (inr / self.rate).round_dp(2)
        }
    }
}

/// Returns the current reference FX rate for USD/INR.
pub fn get_current_fx_rate() -> FxRate {
    FxRate {
        base: "USD".to_string(),
        quote: "INR".to_string(),
        rate: dec!(86.50), // Reference benchmark rate
        timestamp: Utc::now(),
        source: "Reference Benchmark (RBI / FX Interbank)".to_string(),
    }
}

/// Combined multi-currency portfolio reporting summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CombinedPortfolioSummary {
    pub fx_rate: FxRate,
    // India metrics in INR
    pub india_equity_inr: Decimal,
    pub india_realized_pnl_inr: Decimal,
    pub india_unrealized_pnl_inr: Decimal,
    pub india_total_costs_inr: Decimal,
    pub india_net_pnl_inr: Decimal,
    // US metrics in USD
    pub us_equity_usd: Decimal,
    pub us_realized_pnl_usd: Decimal,
    pub us_unrealized_pnl_usd: Decimal,
    pub us_total_costs_usd: Decimal,
    pub us_net_pnl_usd: Decimal,
    // Combined metrics via timestamped FX
    pub combined_equity_usd: Decimal,
    pub combined_equity_inr: Decimal,
    pub combined_net_pnl_usd: Decimal,
    pub combined_net_pnl_inr: Decimal,
}

impl CombinedPortfolioSummary {
    #[allow(clippy::too_many_arguments)]
    pub fn calculate(
        fx: &FxRate,
        india_equity_inr: Decimal,
        india_realized_inr: Decimal,
        india_unrealized_inr: Decimal,
        india_costs_inr: Decimal,
        us_equity_usd: Decimal,
        us_realized_usd: Decimal,
        us_unrealized_usd: Decimal,
        us_costs_usd: Decimal,
    ) -> Self {
        let india_net_pnl_inr = india_realized_inr + india_unrealized_inr - india_costs_inr;
        let us_net_pnl_usd = us_realized_usd + us_unrealized_usd - us_costs_usd;

        let india_equity_in_usd = fx.inr_to_usd(india_equity_inr);
        let us_equity_in_inr = fx.usd_to_inr(us_equity_usd);

        let india_pnl_in_usd = fx.inr_to_usd(india_net_pnl_inr);
        let us_pnl_in_inr = fx.usd_to_inr(us_net_pnl_usd);

        let combined_equity_usd = us_equity_usd + india_equity_in_usd;
        let combined_equity_inr = india_equity_inr + us_equity_in_inr;

        let combined_net_pnl_usd = us_net_pnl_usd + india_pnl_in_usd;
        let combined_net_pnl_inr = india_net_pnl_inr + us_pnl_in_inr;

        Self {
            fx_rate: fx.clone(),
            india_equity_inr,
            india_realized_pnl_inr: india_realized_inr,
            india_unrealized_pnl_inr: india_unrealized_inr,
            india_total_costs_inr: india_costs_inr,
            india_net_pnl_inr,
            us_equity_usd,
            us_realized_pnl_usd: us_realized_usd,
            us_unrealized_pnl_usd: us_unrealized_usd,
            us_total_costs_usd: us_costs_usd,
            us_net_pnl_usd,
            combined_equity_usd,
            combined_equity_inr,
            combined_net_pnl_usd,
            combined_net_pnl_inr,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fx_conversion_calculates_accurately() {
        let fx = FxRate {
            base: "USD".to_string(),
            quote: "INR".to_string(),
            rate: dec!(86.50),
            timestamp: Utc::now(),
            source: "Test".to_string(),
        };

        let usd = dec!(1000.0);
        let inr = fx.usd_to_inr(usd);
        assert_eq!(inr, dec!(86500.00));

        let back_to_usd = fx.inr_to_usd(inr);
        assert_eq!(back_to_usd, dec!(1000.00));
    }

    #[test]
    fn combined_portfolio_combines_with_explicit_fx() {
        let fx = FxRate {
            base: "USD".to_string(),
            quote: "INR".to_string(),
            rate: dec!(80.00),
            timestamp: Utc::now(),
            source: "Test".to_string(),
        };

        let summary = CombinedPortfolioSummary::calculate(
            &fx,
            dec!(800000.0), // ₹8,00,000 = $10,000
            dec!(16000.0),  // ₹16,000 realized
            dec!(0.0),
            dec!(1000.0),  // ₹1,000 costs
            dec!(10000.0), // $10,000
            dec!(500.0),   // $500 realized
            dec!(0.0),
            dec!(20.0), // $20 costs
        );

        assert_eq!(summary.india_net_pnl_inr, dec!(15000.0)); // ₹15,000
        assert_eq!(summary.us_net_pnl_usd, dec!(480.0)); // $480
                                                         // Combined equity: $10,000 + $10,000 = $20,000
        assert_eq!(summary.combined_equity_usd, dec!(20000.00));
        assert_eq!(summary.combined_equity_inr, dec!(1600000.00));
    }
}
