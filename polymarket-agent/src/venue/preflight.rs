//! Can this venue actually be traded, right now, with these credentials?
//!
//! The question `--dry-run` exists to answer, and the last chance to answer
//! it cheaply: everything here is a read, and the alternative is finding out
//! from a rejected order partway into a fourteen-day paper window.
//!
//! In the library rather than in `main.rs` so it can be tested against
//! `StubVenue`. The first version lived in the binary, where integration
//! tests cannot reach it, and shipped with none — which is how it came to
//! hardcode a scan limit that made it blind to the exact misconfiguration it
//! advertised catching.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::venue::types::ScanFilter;
use crate::venue::{venue_has_work_at, Venue};

/// One line of a venue's report card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// Worked, with what it found.
    Ok(String),
    /// Worth saying, but not a reason to refuse to start.
    Warn(String),
    /// The venue cannot be traded until this is fixed.
    Failed(String),
}

impl Check {
    pub fn is_failure(&self) -> bool {
        matches!(self, Check::Failed(_))
    }

    /// Marker for console output. Never the only carrier of meaning — every
    /// variant also carries its own sentence.
    pub fn mark(&self) -> &'static str {
        match self {
            Check::Ok(_) => "✅",
            Check::Warn(_) => "⚠️ ",
            Check::Failed(_) => "❌",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Check::Ok(m) | Check::Warn(m) | Check::Failed(m) => m,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preflight {
    pub venue_id: String,
    /// `(label, outcome)`, in the order they were run — each step is only
    /// meaningful if the one before it worked.
    pub checks: Vec<(&'static str, Check)>,
}

impl Preflight {
    pub fn failures(&self) -> impl Iterator<Item = &Check> {
        self.checks
            .iter()
            .map(|(_, c)| c)
            .filter(|c| c.is_failure())
    }

    pub fn passed(&self) -> bool {
        self.failures().next().is_none()
    }
}

/// Run every read-only check a venue supports.
///
/// `max_results` is the caller's *real* scan limit, not a sample size. The
/// first version of this passed a hardcoded 5, so a `scanning.max_markets`
/// set below the symbol universe — which truncates the live scan to nothing
/// and is the single most consequential config mistake available — came back
/// green.
pub async fn check(
    venue: &dyn Venue,
    configured_symbols: &[String],
    max_results: usize,
    now: DateTime<Utc>,
) -> Preflight {
    let venue_id = venue.id().to_string();
    let mut checks: Vec<(&'static str, Check)> = Vec::new();

    // Balance doubles as the auth check: every venue needs a credentialed
    // call to answer it, so a 403 from mismatched paper/live keys surfaces
    // here rather than on the first order of the first cycle.
    let balance = match venue.balance().await {
        Ok(b) => b,
        Err(e) => {
            checks.push(("auth + balance", Check::Failed(format!("{e:#}"))));
            // Everything below needs credentials too. Reporting four more
            // copies of one failure buries it.
            return Preflight { venue_id, checks };
        }
    };

    // Asked for separately, because it is a separate call now: at a spot venue
    // it costs a quote per holding, and the dry run is the one place that
    // cost is worth paying to find out whether the breakers can run at all.
    let equity = venue.equity().await;

    let equity_note = match &equity {
        Ok(Some(value)) => format!(", {value} equity"),
        _ => String::new(),
    };
    checks.push((
        "auth + balance",
        Check::Ok(format!(
            "{} {} available{equity_note}",
            balance.available, balance.ccy
        )),
    ));

    match equity {
        Ok(Some(_)) => {}
        // Failed, not warned, and in both cases.
        //
        // The consequence is the same either way and it is not advisory: no
        // equity means `combined_equity` is `None` for the *whole* registry,
        // the grace cycle is spent, and cycle two raises an `UntilResume`
        // halt that needs a human. A dry run that exits 0 on that sends an
        // operator to fund an account the agent will refuse to trade.
        //
        // The two are still told apart, because they send you to different
        // places: a structural gap is a venue or configuration problem, an
        // error is an outage or a parse bug — and folding the error into the
        // same sentence hid it entirely, since the log line this module
        // writes is one an operator reading the console never sees.
        Ok(None) => checks.push((
            "equity",
            Check::Failed(
                "venue reports no equity figure — the circuit breakers cannot run, so the \
                 agent will halt rather than trade"
                    .to_string(),
            ),
        )),
        Err(e) => checks.push((
            "equity",
            Check::Failed(format!(
                "could not read account equity, so the circuit breakers cannot run: {e:#}"
            )),
        )),
    }
    if balance.available <= Decimal::ZERO {
        checks.push((
            "balance",
            Check::Warn(format!(
                "{} available — nothing can be sized",
                balance.available
            )),
        ));
    }

    // Whether the account is *permitted* to trade, which a balance call does
    // not establish: a `trading_blocked` account still answers `/v2/account`
    // with cash and equity, and then rejects every order.
    match venue.trading_readiness().await {
        Ok(()) => checks.push(("tradeable", Check::Ok("account can trade".to_string()))),
        Err(e) => checks.push(("tradeable", Check::Failed(format!("{e:#}")))),
    }

    let filter = ScanFilter {
        asset_classes: vec![],
        symbols: configured_symbols.to_vec(),
        min_volume_24h: None,
        max_days_to_resolution: None,
        max_results: Some(max_results),
    };
    let instruments = match venue.list_instruments(&filter).await {
        Ok(i) => i,
        Err(e) => {
            checks.push(("instruments", Check::Failed(format!("{e:#}"))));
            return Preflight { venue_id, checks };
        }
    };

    let wanted = configured_symbols.len();
    if instruments.is_empty() {
        checks.push((
            "instruments",
            Check::Failed(
                "none discovered — check [[venues]].symbols and scanning.max_markets, \
                 which caps the venue scan and not only the legacy loop"
                    .to_string(),
            ),
        ));
        return Preflight { venue_id, checks };
    }
    if wanted > 0 && instruments.len() < wanted {
        // Unlisted or mistyped symbols are dropped with a log warning that
        // an operator reading console output never sees. Paying for
        // valuations on a smaller universe than you configured is a quiet
        // way to under-trade a whole window.
        checks.push((
            "instruments",
            Check::Warn(format!(
                "{} of {wanted} configured symbols are tradeable — the rest are \
                 unlisted, untradeable or mistyped",
                instruments.len()
            )),
        ));
    } else {
        checks.push((
            "instruments",
            Check::Ok(format!("{} of {wanted} configured", instruments.len())),
        ));
    }

    // Quote something that is actually tradeable now. Quoting the first
    // instrument regardless picks a shut equity on a Saturday — which is
    // when someone sets up before a window — and turns a correct
    // configuration into a hard failure.
    let session_open = venue.is_open_at(now);
    let quotable = instruments
        .iter()
        .find(|i| i.asset_class.never_closes() || session_open);
    match quotable {
        Some(instrument) => match venue.quote(&instrument.id).await {
            Ok(q) => checks.push((
                "quote",
                Check::Ok(format!(
                    "{} {}/{} (mid {})",
                    instrument.symbol(),
                    q.bid,
                    q.ask,
                    q.mid
                )),
            )),
            Err(e) => checks.push((
                "quote",
                Check::Failed(format!("{} {e:#}", instrument.symbol())),
            )),
        },
        None => checks.push((
            "quote",
            Check::Warn(
                "every instrument's session is shut — nothing to quote until it opens".to_string(),
            ),
        )),
    }

    checks.push(("session", session_check(venue, now)));
    Preflight { venue_id, checks }
}

/// A closed equity session out of hours is normal. A venue that lists only
/// always-on assets and still reports closed is not, and `venue_has_work_at`
/// ORs the two together — so folding it into one line made them identical.
fn session_check(venue: &dyn Venue, now: DateTime<Utc>) -> Check {
    let open = venue.is_open_at(now);
    let always_on = venue.capabilities().has_always_on();
    match (open, always_on) {
        (true, _) => Check::Ok("open".to_string()),
        (false, true) => Check::Warn(
            "session reports closed, but this venue lists assets that never close — \
             trading continues, and the session is worth checking"
                .to_string(),
        ),
        (false, false) => Check::Ok(format!(
            "closed — nothing tradeable until it reopens (has work now: {})",
            venue_has_work_at(venue, now)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::venue::session::TradingSession;
    use crate::venue::test_support::StubVenue;
    use crate::venue::types::AssetClass;
    use rust_decimal_macros::dec;

    fn at() -> DateTime<Utc> {
        chrono::NaiveDate::from_ymd_opt(2026, 9, 21)
            .unwrap()
            .and_hms_opt(14, 0, 0)
            .unwrap()
            .and_utc()
    }

    fn syms(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// A session that is shut at `at()` — weekdays listed but no windows, so
    /// nothing is ever inside one.
    fn shut() -> TradingSession {
        TradingSession::Windows {
            tz: chrono_tz::America::New_York,
            weekdays: vec![chrono::Weekday::Mon],
            windows: Vec::new(),
            holidays: Vec::new(),
        }
    }

    #[tokio::test]
    async fn a_healthy_venue_passes_every_check() {
        let venue = StubVenue::new("alpaca", TradingSession::Always, &["BTC/USD"], false)
            .quoting(dec!(100));
        let report = check(&venue, &syms(&["BTC/USD"]), 50, at()).await;
        assert!(report.passed(), "{report:#?}");
        assert_eq!(report.venue_id, "alpaca");
    }

    /// The failure the whole command exists for, and the one the first
    /// version could not see: a scan limit below the symbol universe
    /// truncates discovery to nothing, and the agent then logs healthy
    /// cycles and trades nothing for as long as it is left running.
    #[tokio::test]
    async fn a_scan_limit_of_zero_is_a_failure_not_a_green_tick() {
        let venue = StubVenue::new("alpaca", TradingSession::Always, &["BTC/USD"], false)
            .quoting(dec!(100));
        let report = check(&venue, &syms(&["BTC/USD"]), 0, at()).await;
        assert!(!report.passed(), "a zero scan limit must fail: {report:#?}");
        assert!(report
            .failures()
            .any(|f| f.message().contains("max_markets")));
    }

    /// And the control: the same venue with a real limit passes. Without
    /// this, a check that failed unconditionally would satisfy the test above.
    #[tokio::test]
    async fn the_same_venue_passes_with_a_real_scan_limit() {
        let venue = StubVenue::new("alpaca", TradingSession::Always, &["BTC/USD"], false)
            .quoting(dec!(100));
        assert!(check(&venue, &syms(&["BTC/USD"]), 50, at()).await.passed());
    }

    #[tokio::test]
    async fn a_venue_that_will_not_authenticate_reports_once_and_stops() {
        // `fail` alone is not enough: the stub answers `balance`
        // unconditionally, so a rejected credential is its own mode.
        let venue =
            StubVenue::new("alpaca", TradingSession::Always, &["BTC/USD"], false).without_balance();
        let report = check(&venue, &syms(&["BTC/USD"]), 50, at()).await;
        assert!(!report.passed());
        assert_eq!(
            report.checks.len(),
            1,
            "four copies of one failure buries it: {report:#?}"
        );
        assert_eq!(report.checks[0].0, "auth + balance");
    }

    fn named<'a>(report: &'a Preflight, name: &str) -> &'a Check {
        report
            .checks
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, c)| c)
            .unwrap_or_else(|| panic!("no {name:?} check in {report:#?}"))
    }

    /// A venue that authenticates fine but cannot value its own book. The
    /// consequence is not advisory: no equity means the whole registry's sum
    /// is unknowable, and cycle two raises an `UntilResume` halt. A dry run
    /// that exits 0 on that sends an operator to fund an account the agent
    /// will refuse to trade.
    #[tokio::test]
    async fn a_venue_with_no_equity_figure_fails_the_dry_run() {
        let venue = StubVenue::new("coinbase", TradingSession::Always, &["BTC/USD"], false)
            .quoting(dec!(100))
            .without_equity();
        let report = check(&venue, &syms(&["BTC/USD"]), 50, at()).await;

        assert!(!report.passed(), "it must not exit 0: {report:#?}");
        let equity = named(&report, "equity");
        assert!(
            matches!(equity, Check::Failed(m) if m.contains("circuit breakers")),
            "and must say what it costs: {equity:?}"
        );
    }

    /// An error is a different problem from a structural gap — an outage or a
    /// parse bug rather than a venue that cannot value its book — and folding
    /// it into the same sentence hid it, because the log line this module
    /// writes is one an operator reading the console never sees.
    #[tokio::test]
    async fn an_equity_call_that_errors_reports_the_error_itself() {
        let venue = StubVenue::new("coinbase", TradingSession::Always, &["BTC/USD"], false)
            .quoting(dec!(100))
            .failing_equity();
        let report = check(&venue, &syms(&["BTC/USD"]), 50, at()).await;

        assert!(!report.passed());
        let equity = named(&report, "equity");
        assert!(
            matches!(equity, Check::Failed(m) if m.contains("account endpoint is down")),
            "the operator needs the cause, not a generic gap: {equity:?}"
        );
    }

    /// The happy path reports the figure rather than staying silent about it.
    #[tokio::test]
    async fn a_venue_that_reports_equity_shows_the_figure_beside_the_cash() {
        let venue = StubVenue::new("alpaca", TradingSession::Always, &["BTC/USD"], false)
            .quoting(dec!(100));
        let report = check(&venue, &syms(&["BTC/USD"]), 50, at()).await;

        let auth = named(&report, "auth + balance");
        assert!(
            // The stub's equity (275.50) differs from its cash (100) on
            // purpose: asserting only that the word "equity" appears would
            // pass a regression that printed the cash figure and labelled it
            // equity, which is exactly the confusion the trait doc warns of.
            matches!(auth, Check::Ok(m) if m.contains("275.50 equity") && m.contains("100")),
            "both numbers, and the right one against each label: {auth:?}"
        );
    }

    /// Symbols the venue does not list are dropped with a log warning the
    /// operator never sees on the console. Paying for valuations on a
    /// smaller universe than configured under-trades a whole window.
    #[tokio::test]
    async fn a_partially_unlisted_universe_is_reported() {
        let venue = StubVenue::new("alpaca", TradingSession::Always, &["BTC/USD"], false)
            .quoting(dec!(100));
        let report = check(&venue, &syms(&["BTC/USD", "TYPO/USD"]), 50, at()).await;
        let (_, instruments) = report
            .checks
            .iter()
            .find(|(l, _)| *l == "instruments")
            .expect("instrument check");
        assert!(
            matches!(instruments, Check::Warn(m) if m.contains("1 of 2")),
            "{instruments:?}"
        );
        // A partial universe is worth saying and not worth refusing to start
        // over — the operator may have meant to drop one.
        assert!(report.passed());
    }

    /// Setting up on a Saturday is normal. Quoting a shut equity and calling
    /// the venue broken is not.
    #[tokio::test]
    async fn a_closed_equity_session_does_not_fail_the_check() {
        let venue =
            StubVenue::with_classes("alpaca", shut(), &[("SPY", AssetClass::Equity)], false)
                .quoting(dec!(100));
        let report = check(&venue, &syms(&["SPY"]), 50, at()).await;
        assert!(
            report.passed(),
            "a shut equity market is not a misconfiguration: {report:#?}"
        );
    }

    /// A crypto venue whose session says closed still trades, but that
    /// combination is worth flagging rather than printing as normal.
    #[tokio::test]
    async fn an_always_on_venue_reporting_closed_is_flagged() {
        let venue = StubVenue::with_classes(
            "alpaca",
            shut(),
            &[("BTC/USD", AssetClass::CryptoSpot)],
            false,
        )
        .quoting(dec!(100));
        let report = check(&venue, &syms(&["BTC/USD"]), 50, at()).await;
        let (_, session) = report
            .checks
            .iter()
            .find(|(l, _)| *l == "session")
            .expect("session check");
        assert!(matches!(session, Check::Warn(_)), "{session:?}");
        assert!(report.passed(), "still tradeable, so not a failure");
    }
}
