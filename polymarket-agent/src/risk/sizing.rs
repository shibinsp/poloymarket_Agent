//! Position sizing across asset classes.
//!
//! Prediction markets keep the existing binary Kelly: a contract that settles
//! at 0 or 1 has exactly the payoff Kelly assumes. Continuous assets don't —
//! there is no "win probability" with a known payout — so they use volatility
//! targeting instead: choose a stop distance from ATR, then buy the quantity
//! whose loss at that stop equals a fixed fraction of the bankroll.
//!
//! The practical difference is that the maximum loss per trade becomes a
//! number chosen in advance rather than an emergent property of a formula.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::warn;

use crate::config::{ContinuousSizingConfig, RiskConfig};
use crate::market::models::AgentState;
use crate::risk::kelly::{kelly_size, KellyResult};
use crate::venue::types::{AssetClass, Candle, Instrument};

/// A sized position, whatever method produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct SizeResult {
    /// Dollar notional to deploy. Zero means "do not trade".
    pub position_usd: Decimal,
    /// Fractional stop distance below entry, when the method defines one.
    pub stop_pct: Option<Decimal>,
    /// Fraction of bankroll risked if the stop is hit.
    pub risk_pct: Option<Decimal>,
    /// Why the size is zero, for logging.
    pub rejection: Option<&'static str>,
}

impl SizeResult {
    pub fn none(reason: &'static str) -> Self {
        Self {
            position_usd: Decimal::ZERO,
            stop_pct: None,
            risk_pct: None,
            rejection: Some(reason),
        }
    }

    pub fn should_trade(&self) -> bool {
        self.position_usd > Decimal::ZERO
    }
}

impl From<KellyResult> for SizeResult {
    fn from(k: KellyResult) -> Self {
        Self {
            position_usd: k.position_usd,
            stop_pct: None,
            risk_pct: None,
            rejection: None,
        }
    }
}

/// Inputs common to every sizing method.
pub struct SizeInputs<'a> {
    pub instrument: &'a Instrument,
    /// Model's probability the position resolves/moves in our favour.
    pub probability: Decimal,
    /// Price we expect to pay.
    pub price: Decimal,
    pub confidence: Decimal,
    pub bankroll: Decimal,
    pub state: AgentState,
    /// Recent bars, required for continuous assets.
    pub candles: &'a [Candle],
}

/// Size a position using the method appropriate to the asset class.
pub fn size_position(
    inputs: &SizeInputs<'_>,
    risk: &RiskConfig,
    continuous: &ContinuousSizingConfig,
) -> SizeResult {
    match inputs.instrument.asset_class {
        AssetClass::PredictionBinary => kelly_size(
            inputs.probability,
            inputs.price,
            inputs.confidence,
            inputs.bankroll,
            inputs.state,
            risk,
        )
        .into(),
        AssetClass::CryptoSpot | AssetClass::Equity => size_continuous(inputs, risk, continuous),
    }
}

/// Volatility-targeted sizing: `qty * stop_distance == risk_budget`.
fn size_continuous(
    inputs: &SizeInputs<'_>,
    risk: &RiskConfig,
    cfg: &ContinuousSizingConfig,
) -> SizeResult {
    let state_multiplier = state_multiplier(inputs.state);
    if state_multiplier.is_zero() {
        return SizeResult::none("agent state forbids new positions");
    }
    if inputs.price <= Decimal::ZERO {
        return SizeResult::none("non-positive price");
    }
    if inputs.bankroll <= Decimal::ZERO {
        return SizeResult::none("no bankroll");
    }

    // Stop distance from measured volatility, clamped so a quiet market can't
    // imply an absurdly tight stop (and therefore a huge position) and a wild
    // one can't imply a stop so wide the position is meaningless.
    let Some(atr_pct) = crate::risk::volatility::atr_pct(inputs.candles, cfg.atr_period) else {
        return SizeResult::none("insufficient price history to measure volatility");
    };
    let stop_pct = (atr_pct * cfg.atr_multiplier).clamp(cfg.min_stop_pct, cfg.max_stop_pct);
    if stop_pct <= Decimal::ZERO {
        return SizeResult::none("non-positive stop distance");
    }

    // Risk budget shrinks with low confidence and with agent state, the same
    // way Kelly scaling does for prediction markets.
    let risk_pct = cfg.risk_per_trade_pct * inputs.confidence * state_multiplier;
    let risk_usd = inputs.bankroll * risk_pct;

    // Losing `stop_pct` of the notional must cost exactly the risk budget.
    let mut position_usd = risk_usd / stop_pct;

    // The same hard caps prediction markets get. Note these frequently bind:
    // at the defaults (0.75% risk over a 3% stop) the target is 25% of
    // bankroll, far above max_position_pct, so the cap — not the risk budget
    // — sets the size, and realised risk per trade is correspondingly
    // smaller. That is deliberate for micro capital, but it means raising
    // risk_per_trade_pct alone changes nothing until the cap is raised too.
    let max_position = inputs.bankroll * risk.max_position_pct;
    if position_usd > max_position {
        position_usd = max_position;
    }

    if position_usd < risk.min_position_usd {
        return SizeResult::none("below minimum position size");
    }
    if !inputs.instrument.meets_min_notional(position_usd) {
        warn!(
            instrument = %inputs.instrument.id,
            position_usd = %position_usd,
            "Position is below the venue's minimum notional — skipping"
        );
        return SizeResult::none("below venue minimum notional");
    }

    SizeResult {
        position_usd,
        stop_pct: Some(stop_pct),
        risk_pct: Some(risk_pct),
        rejection: None,
    }
}

/// Mirrors the lifecycle scaling used by Kelly sizing.
fn state_multiplier(state: AgentState) -> Decimal {
    match state {
        AgentState::Alive => Decimal::ONE,
        AgentState::LowFuel => dec!(0.25),
        AgentState::CriticalSurvival | AgentState::Dead => Decimal::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::venue::types::{InstrumentId, InstrumentMeta, VenueId};
    use chrono::{Duration, Utc};

    fn risk_config() -> RiskConfig {
        RiskConfig {
            kelly_fraction: dec!(0.5),
            max_position_pct: dec!(0.06),
            max_total_exposure_pct: dec!(0.30),
            max_positions_per_category: 3,
            min_position_usd: dec!(1.0),
        }
    }

    /// Risk config whose position cap is loose enough not to bind, so the
    /// volatility-targeting arithmetic can be asserted on its own.
    fn uncapped_risk_config() -> RiskConfig {
        RiskConfig {
            max_position_pct: dec!(0.50),
            ..risk_config()
        }
    }

    fn continuous_config() -> ContinuousSizingConfig {
        ContinuousSizingConfig {
            risk_per_trade_pct: dec!(0.0075),
            atr_period: 14,
            atr_multiplier: dec!(2.0),
            min_stop_pct: dec!(0.03),
            max_stop_pct: dec!(0.12),
        }
    }

    fn equity(min_notional: Option<Decimal>) -> Instrument {
        Instrument {
            id: InstrumentId::new(VenueId::new("alpaca"), "AAPL"),
            asset_class: AssetClass::Equity,
            display_name: "Apple".to_string(),
            quote_ccy: "USD".to_string(),
            tick_size: Some(dec!(0.01)),
            lot_size: None,
            min_notional,
            fractional: true,
            meta: InstrumentMeta::Equity {
                exchange: "NASDAQ".to_string(),
            },
        }
    }

    fn prediction() -> Instrument {
        Instrument {
            id: InstrumentId::new(VenueId::new("polymarket"), "0xabc:YES"),
            asset_class: AssetClass::PredictionBinary,
            display_name: "Rain? [YES]".to_string(),
            quote_ccy: "USDC".to_string(),
            tick_size: Some(dec!(0.01)),
            lot_size: None,
            min_notional: None,
            fractional: false,
            meta: InstrumentMeta::Prediction {
                condition_id: "0xabc".to_string(),
                outcome: "Yes".to_string(),
                token_id: "tok".to_string(),
                question: "Rain?".to_string(),
                end_date: Utc::now() + Duration::days(3),
            },
        }
    }

    /// Bars with a 1% ATR on a $100 close.
    fn candles() -> Vec<Candle> {
        (0..20)
            .map(|i| Candle {
                ts: Utc::now() + Duration::hours(i),
                open: dec!(100),
                high: dec!(100.5),
                low: dec!(99.5),
                close: dec!(100),
                volume: dec!(1000),
            })
            .collect()
    }

    fn inputs<'a>(
        instrument: &'a Instrument,
        candles: &'a [Candle],
        bankroll: Decimal,
        state: AgentState,
    ) -> SizeInputs<'a> {
        SizeInputs {
            instrument,
            probability: dec!(0.65),
            price: dec!(100),
            confidence: dec!(0.80),
            bankroll,
            state,
            candles,
        }
    }

    #[test]
    fn continuous_size_makes_the_stop_loss_equal_the_risk_budget() {
        let inst = equity(None);
        let bars = candles();
        let result = size_position(
            &inputs(&inst, &bars, dec!(10_000), AgentState::Alive),
            &uncapped_risk_config(),
            &continuous_config(),
        );

        // ATR 1% x multiplier 2 = 2% raw, clamped up to the 3% floor.
        assert_eq!(result.stop_pct, Some(dec!(0.03)));
        // risk = 0.75% x 0.80 confidence = 0.6% of 10k = $60.
        assert_eq!(result.risk_pct, Some(dec!(0.006)));
        // $60 risked over a 3% stop = $2000 notional.
        assert_eq!(result.position_usd, dec!(2000));

        // The defining property: losing stop_pct of the position costs exactly
        // the risk budget.
        let loss_at_stop = result.position_usd * result.stop_pct.unwrap();
        assert_eq!(loss_at_stop, dec!(10_000) * result.risk_pct.unwrap());
    }

    #[test]
    fn max_position_pct_caps_the_volatility_target() {
        let inst = equity(None);
        let bars = candles();
        let result = size_position(
            &inputs(&inst, &bars, dec!(10_000), AgentState::Alive),
            &risk_config(),
            &continuous_config(),
        );
        // Uncapped the target is $2000 (20% of bankroll); the 6% cap wins.
        assert_eq!(result.position_usd, dec!(600.00));
    }

    #[test]
    fn at_default_settings_the_position_cap_dominates_the_risk_budget() {
        // Pins a real interaction rather than a bug: with a 0.75% risk budget
        // over a 3% stop the target is 25% of bankroll, so max_position_pct
        // sets the size and realised risk per trade is far below the
        // configured figure. Raising risk_per_trade_pct alone does nothing.
        let inst = equity(None);
        let bars = candles();
        let bankroll = dec!(10_000);
        let risk = risk_config();

        let base = size_position(
            &inputs(&inst, &bars, bankroll, AgentState::Alive),
            &risk,
            &continuous_config(),
        );

        let doubled = ContinuousSizingConfig {
            risk_per_trade_pct: dec!(0.015),
            ..continuous_config()
        };
        let after = size_position(
            &inputs(&inst, &bars, bankroll, AgentState::Alive),
            &risk,
            &doubled,
        );

        assert_eq!(base.position_usd, after.position_usd);
        assert_eq!(base.position_usd, bankroll * risk.max_position_pct);
    }

    #[test]
    fn no_price_history_means_no_position() {
        let inst = equity(None);
        let result = size_position(
            &inputs(&inst, &[], dec!(10_000), AgentState::Alive),
            &risk_config(),
            &continuous_config(),
        );
        assert!(!result.should_trade());
        assert_eq!(
            result.rejection,
            Some("insufficient price history to measure volatility")
        );
    }

    #[test]
    fn low_fuel_quarters_the_risk_and_dead_states_refuse() {
        let inst = equity(None);
        let bars = candles();
        let alive = size_position(
            &inputs(&inst, &bars, dec!(10_000), AgentState::Alive),
            &uncapped_risk_config(),
            &continuous_config(),
        );
        let low = size_position(
            &inputs(&inst, &bars, dec!(10_000), AgentState::LowFuel),
            &uncapped_risk_config(),
            &continuous_config(),
        );
        assert_eq!(low.position_usd, alive.position_usd / dec!(4));

        for state in [AgentState::CriticalSurvival, AgentState::Dead] {
            let r = size_position(
                &inputs(&inst, &bars, dec!(10_000), state),
                &uncapped_risk_config(),
                &continuous_config(),
            );
            assert!(!r.should_trade(), "{state:?} must not open positions");
        }
    }

    #[test]
    fn venue_minimum_notional_blocks_undersized_orders() {
        // Binance.US-style $10 floor against a bankroll that sizes below it.
        let inst = equity(Some(dec!(10)));
        let bars = candles();
        let result = size_position(
            &inputs(&inst, &bars, dec!(100), AgentState::Alive),
            &risk_config(),
            &continuous_config(),
        );
        assert!(!result.should_trade());
        assert_eq!(result.rejection, Some("below venue minimum notional"));
    }

    #[test]
    fn prediction_markets_still_use_binary_kelly() {
        let inst = prediction();
        let bars: Vec<Candle> = Vec::new();
        let mut i = inputs(&inst, &bars, dec!(1000), AgentState::Alive);
        // A binary contract priced at 0.50 that the model thinks is 0.65.
        i.price = dec!(0.50);

        let result = size_position(&i, &risk_config(), &continuous_config());
        // Kelly path needs no candles and yields a position.
        assert!(result.should_trade());
        assert_eq!(result.stop_pct, None, "binary sizing defines no stop");
        // And it respects the same 6% cap.
        assert!(result.position_usd <= dec!(1000) * dec!(0.06));
    }

    #[test]
    fn wide_volatility_is_clamped_to_the_max_stop() {
        let inst = equity(None);
        // 10% ATR x 2 = 20% raw stop, above the 12% ceiling.
        let bars: Vec<Candle> = (0..20)
            .map(|i| Candle {
                ts: Utc::now() + Duration::hours(i),
                open: dec!(100),
                high: dec!(105),
                low: dec!(95),
                close: dec!(100),
                volume: dec!(1000),
            })
            .collect();
        let result = size_position(
            &inputs(&inst, &bars, dec!(10_000), AgentState::Alive),
            &risk_config(),
            &continuous_config(),
        );
        assert_eq!(result.stop_pct, Some(dec!(0.12)));
    }
}
