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
    /// Fraction of bankroll actually risked if the stop is hit — computed from
    /// the size being returned, not from the budget that was asked for. The
    /// two differ whenever a cap binds, which at the shipped defaults is the
    /// normal case rather than the exception.
    pub risk_pct: Option<Decimal>,
    /// Why the size is zero, for logging.
    pub rejection: Option<&'static str>,
    /// Kelly diagnostics, present only for prediction markets.
    ///
    /// `trades.kelly_raw` and `trades.kelly_adjusted` are NOT NULL in the
    /// schema and `Store::insert_trade` binds both, so the prediction path
    /// cannot afford to drop them on the way through this type.
    pub kelly: Option<KellyResult>,
}

impl SizeResult {
    pub fn none(reason: &'static str) -> Self {
        Self {
            position_usd: Decimal::ZERO,
            stop_pct: None,
            risk_pct: None,
            rejection: Some(reason),
            kelly: None,
        }
    }

    pub fn should_trade(&self) -> bool {
        self.position_usd > Decimal::ZERO
    }
}

impl From<KellyResult> for SizeResult {
    fn from(k: KellyResult) -> Self {
        // Kelly returns a zero position for a negative edge or a result below
        // the minimum, and this type documents `rejection` as "why the size is
        // zero". Leaving it None gave the prediction path a silent zero while
        // the continuous path always explained itself.
        let rejection = if k.position_usd > Decimal::ZERO {
            None
        } else if k.kelly_adjusted <= Decimal::ZERO {
            Some("no positive Kelly edge")
        } else {
            Some("Kelly size below the minimum position")
        };

        Self {
            position_usd: k.position_usd,
            stop_pct: None,
            risk_pct: None,
            rejection,
            kelly: Some(k),
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
    // A venue minimum expressed as a quantity has to be checked in quantity.
    // At micro capital this is exactly where it bites: a $6 slice of BTC is a
    // fraction small enough to sit near Alpaca's floor, and the rejection
    // would otherwise only arrive at submission.
    let qty = position_usd / inputs.price;
    if !inputs.instrument.meets_min_qty(qty) {
        warn!(
            instrument = %inputs.instrument.id,
            qty = %qty,
            "Position is below the venue's minimum order size — skipping"
        );
        return SizeResult::none("below venue minimum order size");
    }
    if !inputs.instrument.meets_min_notional(position_usd) {
        warn!(
            instrument = %inputs.instrument.id,
            position_usd = %position_usd,
            "Position is below the venue's minimum notional — skipping"
        );
        return SizeResult::none("below venue minimum notional");
    }

    // Report the risk actually being taken, not the budget that was requested.
    // Once the cap binds, losing `stop_pct` of the capped notional costs a
    // fraction of the budget — at the shipped defaults roughly a third of it —
    // and a caller aggregating this field into a portfolio risk number would
    // otherwise over-count every position.
    let realised_risk_pct = position_usd * stop_pct / inputs.bankroll;

    SizeResult {
        position_usd,
        stop_pct: Some(stop_pct),
        risk_pct: Some(realised_risk_pct),
        rejection: None,
        kelly: None,
    }
}

/// Mirrors the lifecycle scaling used by Kelly sizing.
fn state_multiplier(state: AgentState) -> Decimal {
    match state {
        AgentState::Alive => Decimal::ONE,
        AgentState::LowFuel => dec!(0.25),
        AgentState::CriticalSurvival | AgentState::Halted | AgentState::Dead => Decimal::ZERO,
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
            ..RiskConfig::default()
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
            min_p_up: dec!(0.55),
            max_orders_per_cycle: 2,
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
            min_qty: None,
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
            min_qty: None,
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
    fn a_halted_agent_is_sized_out_of_every_asset_class() {
        // The innermost of the three gates. `Agent::opens_positions` stops
        // the cycle being run at all and `apply_halt` settles the state, but
        // both of those are in the caller. This is the one that holds if a
        // future caller forgets — and it covers the prediction path too,
        // which goes through Kelly rather than through volatility targeting.
        let bars = candles();
        for inst in [equity(None), prediction()] {
            let result = size_position(
                &inputs(&inst, &bars, dec!(10_000), AgentState::Halted),
                &uncapped_risk_config(),
                &continuous_config(),
            );
            assert!(
                !result.should_trade(),
                "{:?} must not be sized while halted, got {result:?}",
                inst.asset_class
            );
            assert!(
                result.position_usd.is_zero(),
                "a halted size must be zero, got {}",
                result.position_usd
            );
        }
    }

    /// The control: the same inputs, not halted, do produce a position. Without
    /// this the test above would pass if sizing were broken for every state.
    #[test]
    fn the_same_inputs_are_sized_normally_when_not_halted() {
        let bars = candles();
        let result = size_position(
            &inputs(&equity(None), &bars, dec!(10_000), AgentState::Alive),
            &uncapped_risk_config(),
            &continuous_config(),
        );
        assert!(result.should_trade(), "the control case must trade");
        assert!(result.position_usd > Decimal::ZERO);
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

    /// Finding 9: the reported risk has to describe the position being
    /// returned. While the cap binds — the normal case at the shipped defaults
    /// — the budget that was asked for is several times the risk actually
    /// taken, and a caller summing this field into a portfolio number would
    /// over-count every single position.
    #[test]
    fn risk_pct_describes_the_capped_position_not_the_budget() {
        let inst = equity(None);
        let bars = candles();
        let bankroll = dec!(10_000);

        let sized = size_position(
            &inputs(&inst, &bars, bankroll, AgentState::Alive),
            &risk_config(),
            &continuous_config(),
        );

        let stop_pct = sized.stop_pct.expect("continuous sizing sets a stop");
        let risk_pct = sized.risk_pct.expect("continuous sizing reports risk");

        // The cap must actually be binding, or this proves nothing.
        assert_eq!(sized.position_usd, bankroll * dec!(0.06));

        // Losing the stop on the returned notional costs exactly risk_pct.
        assert_eq!(risk_pct * bankroll, sized.position_usd * stop_pct);

        // And that is well below the budget that was requested.
        let requested = continuous_config().risk_per_trade_pct * dec!(0.80);
        assert!(
            risk_pct < requested,
            "realised {risk_pct} should be below the requested budget {requested}"
        );
    }

    #[test]
    fn risk_pct_equals_the_budget_when_no_cap_binds() {
        let inst = equity(None);
        let bars = candles();
        let bankroll = dec!(10_000);

        let sized = size_position(
            &inputs(&inst, &bars, bankroll, AgentState::Alive),
            &uncapped_risk_config(),
            &continuous_config(),
        );

        let requested = continuous_config().risk_per_trade_pct * dec!(0.80);
        assert_eq!(sized.risk_pct, Some(requested));
    }

    /// Finding 10: `trades.kelly_raw` and `trades.kelly_adjusted` are NOT NULL
    /// in the schema, so the conversion cannot drop them on the way through.
    #[test]
    fn converting_a_kelly_result_keeps_its_diagnostics() {
        let k = KellyResult {
            kelly_raw: dec!(0.25),
            kelly_adjusted: dec!(0.10),
            position_usd: dec!(60),
            capped: true,
        };

        let sized: SizeResult = k.clone().into();

        assert_eq!(sized.position_usd, dec!(60));
        assert_eq!(sized.kelly, Some(k));
        assert_eq!(sized.rejection, None);
        assert!(sized.should_trade());
    }

    /// A zero size has to say why. The continuous path always explained
    /// itself; the prediction path returned a silent zero.
    #[test]
    fn a_zero_kelly_size_explains_itself() {
        let no_edge: SizeResult = KellyResult {
            kelly_raw: dec!(-0.05),
            kelly_adjusted: dec!(-0.02),
            position_usd: Decimal::ZERO,
            capped: false,
        }
        .into();
        assert_eq!(no_edge.rejection, Some("no positive Kelly edge"));
        assert!(!no_edge.should_trade());

        let too_small: SizeResult = KellyResult {
            kelly_raw: dec!(0.02),
            kelly_adjusted: dec!(0.01),
            position_usd: Decimal::ZERO,
            capped: false,
        }
        .into();
        assert_eq!(
            too_small.rejection,
            Some("Kelly size below the minimum position")
        );
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

    /// Finding 12, at the sizing layer: a quantity minimum has to be checked
    /// in quantity. A $6 slice of a $64k asset is 0.00009 — fine by notional,
    /// and the venue still rejects it if its floor is a size.
    #[test]
    fn venue_minimum_order_size_blocks_a_position_that_clears_on_notional() {
        let mut inst = equity(None);
        inst.min_qty = Some(dec!(1));
        let bars = candles();

        let mut ins = inputs(&inst, &bars, dec!(10_000), AgentState::Alive);
        // A $600 position at $100_000 a unit is 0.006 units — well over any
        // notional floor, well under a one-unit minimum.
        ins.price = dec!(100_000);

        let sized = size_position(&ins, &risk_config(), &continuous_config());
        assert!(!sized.should_trade());
        assert_eq!(sized.rejection, Some("below venue minimum order size"));
    }

    #[test]
    fn a_position_clearing_the_minimum_order_size_still_trades() {
        let mut inst = equity(None);
        inst.min_qty = Some(dec!(1));
        let bars = candles();

        // At $100 a share a $600 position is 6 shares, over the minimum.
        let sized = size_position(
            &inputs(&inst, &bars, dec!(10_000), AgentState::Alive),
            &risk_config(),
            &continuous_config(),
        );
        assert!(sized.should_trade());
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
