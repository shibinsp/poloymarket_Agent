//! TimesFM's view of an India signal, from the local forecast service
//! (`tools/timesfm_service.py`, 127.0.0.1:8091).
//!
//! Before each entry a book sends the stock's last 512 five-minute closes
//! and asks where it will be at 15:15. A forecast on the wrong side of the
//! signal close skips the trade, which is then followed on that book's shadow
//! ledger so the filter can be judged on what it gave up.
//!
//! The user's call on 3 Nov 2026, India only. On May - Oct 2026:
//! - ORB: trades it agreed with made +Rs 342 (55), those it disagreed with
//!   lost Rs 2,125 (83), in both halves.
//! - Burst: agreed +Rs 963 (13), disagreed +Rs 49 (29); too few to trust,
//!   added the same day at the user's request to gather more.
//! Its plain direction calls were a coin flip (50.3%) and on US trades and
//! gap-and-go the split was reversed. A test, not an edge. When the service
//! is down the books trade as before.

use std::time::Duration;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::india::orb::{nse_slot, UpstoxBars};
use crate::us::orb::Bar;

/// Shadow ledgers: the trades each book skipped on TimesFM's say.
pub const ORB_SHADOW: &str = "india_orb_tfm_skip";
pub const BURST_SHADOW: &str = "india_burst_tfm_skip";
const URL: &str = "http://127.0.0.1:8091/forecast";
/// Context the model reads: about seven sessions of 5-minute bars.
pub const CONTEXT: usize = 512;
/// The bar the forecast is read at, counted from 9:15 in 5-minute bars: the
/// 15:15 bar, as in the 3 Nov 2026 test.
pub const LAST_BAR: usize = 72;

#[derive(Deserialize)]
struct Reply {
    end: f64,
}

/// What TimesFM thinks of a trade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub forecast_end: Decimal,
    pub agrees: bool,
}

/// Whether a forecast backs the trade: above the signal close for a long,
/// below it for a short. A flat forecast does not.
pub fn agrees(buy: bool, signal_close: Decimal, forecast_end: Decimal) -> bool {
    if buy {
        forecast_end > signal_close
    } else {
        forecast_end < signal_close
    }
}

/// Ask the service where `closes` will be `horizon` bars on.
pub async fn view(closes: &[Decimal], horizon: usize, buy: bool) -> Result<View> {
    let signal_close = *closes.last().context("no closes to forecast from")?;
    let series: Vec<f64> = closes[closes.len().saturating_sub(CONTEXT)..]
        .iter()
        .filter_map(|c| c.to_f64())
        .collect();
    let reply: Reply = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?
        .post(URL)
        .json(&serde_json::json!({ "series": series, "horizon": horizon }))
        .send()
        .await
        .context("TimesFM service unreachable")?
        .error_for_status()
        .context("TimesFM service refused the request")?
        .json()
        .await
        .context("TimesFM service sent an unreadable reply")?;
    let forecast_end = Decimal::try_from(reply.end)
        .context("TimesFM forecast is not a number")?
        .round_dp(2);
    Ok(View {
        forecast_end,
        agrees: agrees(buy, signal_close, forecast_end),
    })
}

/// TimesFM's 15:15 forecast for a signal on today's bar `index`: the context
/// is two weeks of 5-minute closes, then today's up to and including that bar.
pub async fn view_at_bar(
    data: &UpstoxBars,
    key: &str,
    today_bars: &[(usize, Bar)],
    index: usize,
    today: NaiveDate,
    buy: bool,
) -> Result<View> {
    let history = data
        .history(key, 5, today - chrono::Duration::days(14), today - chrono::Duration::days(1))
        .await?;
    let mut past: Vec<&Bar> = history
        .iter()
        .filter(|b| nse_slot(b.ts).is_some_and(|(d, _)| d < today))
        .collect();
    past.sort_by_key(|b| b.ts);
    let mut closes: Vec<Decimal> = past.iter().map(|b| b.close).collect();
    let mut todays: Vec<&(usize, Bar)> = today_bars.iter().filter(|(i, _)| *i <= index).collect();
    todays.sort_by_key(|(i, _)| *i);
    closes.extend(todays.iter().map(|(_, b)| b.close));
    view(&closes, LAST_BAR.saturating_sub(index).max(1), buy).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn a_forecast_must_back_the_side_of_the_trade() {
        assert!(agrees(true, dec!(3110), dec!(3125)));
        assert!(!agrees(true, dec!(3110), dec!(3095)));
        assert!(!agrees(true, dec!(3110), dec!(3110)), "flat is not agreement");
        assert!(agrees(false, dec!(100), dec!(99)));
        assert!(!agrees(false, dec!(100), dec!(101)));
    }
}
