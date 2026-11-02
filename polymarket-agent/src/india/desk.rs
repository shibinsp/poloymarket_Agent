//! The India trading desk around the monthly momentum rotation.
//!
//! Same six jobs as the U.S. desk (`us::desk`), once per rebalance:
//! - ATLAS ranks the watchlist by 9-month momentum, and ranks all 51 NIFTY 50
//!   stocks as an advisory *shadow* list.
//! - VEGA reads NIFTY against its 200-day average (the switch) and each
//!   pick's own 50-day trend.
//! - ORION reads each pick's last day of Google News and tags results,
//!   governance red flags, deals, corporate actions and rating changes.
//! - LUNA reads India VIX, breadth (share of the watchlist above its 50-day
//!   average) and headline tone.
//! - TITAN checks each order fits the cash in whole shares.
//! - NOVA records the month's call on one card.
//!
//! Only the backtested rules act: the NIFTY switch, the momentum ranking and
//! TITAN. The shadow list, ORION's governance veto, the 50-day check and
//! LUNA's fear reading are advisory notes on the card, as on the U.S. desk:
//! the 51-stock backtest looked far better (31% a year from 2023) but is
//! inflated by survivorship, since today's index members were picked after
//! they rose.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Serialize;

use crate::india::instruments::find_nse_instrument;
use crate::india::market_feed::UpstoxLiveMarketFeed;
use crate::india::momentum::{select_targets, MomentumConfig, MomentumDecision};
use crate::us::desk::{tone, Headline};

pub const INDIA_VIX_KEY: &str = "NSE_INDEX|India VIX";

/// NIFTY 50 members (October 2026) and their Upstox keys, for ATLAS's shadow list.
pub const NIFTY50: &[(&str, &str)] = &[
    ("ADANIENT", "NSE_EQ|INE423A01024"),
    ("ADANIPORTS", "NSE_EQ|INE742F01042"),
    ("APOLLOHOSP", "NSE_EQ|INE437A01024"),
    ("ASIANPAINT", "NSE_EQ|INE021A01026"),
    ("AXISBANK", "NSE_EQ|INE238A01034"),
    ("BAJAJ-AUTO", "NSE_EQ|INE917I01010"),
    ("BAJAJFINSV", "NSE_EQ|INE918I01026"),
    ("BAJFINANCE", "NSE_EQ|INE296A01032"),
    ("BEL", "NSE_EQ|INE263A01024"),
    ("BHARTIARTL", "NSE_EQ|INE397D01024"),
    ("CIPLA", "NSE_EQ|INE059A01026"),
    ("COALINDIA", "NSE_EQ|INE522F01014"),
    ("DRREDDY", "NSE_EQ|INE089A01031"),
    ("EICHERMOT", "NSE_EQ|INE066A01021"),
    ("ETERNAL", "NSE_EQ|INE758T01015"),
    ("GRASIM", "NSE_EQ|INE047A01021"),
    ("HCLTECH", "NSE_EQ|INE860A01027"),
    ("HDFCBANK", "NSE_EQ|INE040A01034"),
    ("HDFCLIFE", "NSE_EQ|INE795G01014"),
    ("HEROMOTOCO", "NSE_EQ|INE158A01026"),
    ("HINDALCO", "NSE_EQ|INE038A01020"),
    ("HINDUNILVR", "NSE_EQ|INE030A01027"),
    ("ICICIBANK", "NSE_EQ|INE090A01021"),
    ("INDUSINDBK", "NSE_EQ|INE095A01012"),
    ("INFY", "NSE_EQ|INE009A01021"),
    ("ITC", "NSE_EQ|INE154A01025"),
    ("JIOFIN", "NSE_EQ|INE758E01017"),
    ("JSWSTEEL", "NSE_EQ|INE019A01038"),
    ("KOTAKBANK", "NSE_EQ|INE237A01036"),
    ("LICI", "NSE_EQ|INE0J1Y01017"),
    ("LT", "NSE_EQ|INE018A01030"),
    ("M&M", "NSE_EQ|INE101A01026"),
    ("MARUTI", "NSE_EQ|INE585B01010"),
    ("NESTLEIND", "NSE_EQ|INE239A01024"),
    ("NTPC", "NSE_EQ|INE733E01010"),
    ("ONGC", "NSE_EQ|INE213A01029"),
    ("POWERGRID", "NSE_EQ|INE752E01010"),
    ("RELIANCE", "NSE_EQ|INE002A01018"),
    ("SBILIFE", "NSE_EQ|INE123W01016"),
    ("SBIN", "NSE_EQ|INE062A01020"),
    ("SHRIRAMFIN", "NSE_EQ|INE721A01047"),
    ("SUNPHARMA", "NSE_EQ|INE044A01036"),
    ("TATACONSUM", "NSE_EQ|INE192A01025"),
    ("TATASTEEL", "NSE_EQ|INE081A01020"),
    ("TCS", "NSE_EQ|INE467B01029"),
    ("TECHM", "NSE_EQ|INE669C01036"),
    ("TITAN", "NSE_EQ|INE280A01028"),
    ("TMPV", "NSE_EQ|INE155A01022"),
    ("TRENT", "NSE_EQ|INE849A01020"),
    ("ULTRACEMCO", "NSE_EQ|INE481G01011"),
    ("WIPRO", "NSE_EQ|INE075A01022"),
];

// ---------------------------------------------------------------- ORION's feed

/// Google News RSS for India (no key needed).
pub struct GoogleNews {
    http: reqwest::Client,
}

impl Default for GoogleNews {
    fn default() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .user_agent("polymarket-agent/0.1")
                .build()
                .unwrap_or_default(),
        }
    }
}

impl GoogleNews {
    /// The last day's headlines for a company, e.g. "ICICI Bank".
    pub async fn headlines(&self, company: &str) -> Result<Vec<Headline>> {
        let query = format!("\"{company}\" share when:1d");
        let resp = self
            .http
            .get("https://news.google.com/rss/search")
            .query(&[
                ("q", query.as_str()),
                ("hl", "en-IN"),
                ("gl", "IN"),
                ("ceid", "IN:en"),
            ])
            .send()
            .await
            .context("Google News request failed")?;
        if !resp.status().is_success() {
            bail!("Google News returned {}", resp.status());
        }
        Ok(parse_rss(&resp.text().await?))
    }
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let rest = &s[start..];
    Some(&rest[..rest.find(close)?])
}

fn unescape(s: &str) -> String {
    s.replace("<![CDATA[", "")
        .replace("]]>", "")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .trim()
        .to_string()
}

/// Items of an RSS document as headlines. Undated items are dropped.
pub fn parse_rss(xml: &str) -> Vec<Headline> {
    xml.split("<item>")
        .skip(1)
        .filter_map(|item| {
            let title = unescape(between(item, "<title>", "</title>")?);
            let at = DateTime::parse_from_rfc2822(between(item, "<pubDate>", "</pubDate>")?.trim())
                .ok()?
                .with_timezone(&Utc);
            let source = between(item, "<source", "</source>")
                .and_then(|s| s.split_once('>').map(|(_, name)| unescape(name)))
                .unwrap_or_default();
            Some(Headline {
                at,
                headline: title,
                summary: String::new(),
                source,
            })
        })
        .collect()
}

/// Indian-market event tags.
const EVENTS: &[(&str, &[&str])] = &[
    (
        "results",
        &[
            "results",
            " q1 ",
            " q2 ",
            " q3 ",
            " q4 ",
            "quarterly",
            "net profit",
            "earnings",
        ],
    ),
    (
        "governance",
        &[
            "pledge",
            "sebi",
            "fraud",
            "probe",
            "raid",
            "auditor",
            "resigns",
            "default",
            "insolvency",
            "ed searches",
        ],
    ),
    (
        "deal",
        &[
            "merger",
            "demerger",
            "acquisition",
            "acquire",
            "stake sale",
            "block deal",
            " ofs",
            " qip",
        ],
    ),
    (
        "corporate_action",
        &["dividend", "bonus", "stock split", "buyback", "record date"],
    ),
    (
        "rating",
        &["upgrade", "downgrade", "target price", "rating"],
    ),
];

pub fn tag_events(text: &str) -> Vec<&'static str> {
    let t = format!(" {} ", text.to_ascii_lowercase());
    EVENTS
        .iter()
        .filter(|(_, phrases)| phrases.iter().any(|p| t.contains(p)))
        .map(|(name, _)| *name)
        .collect()
}

/// What ORION, LUNA and the shadow ATLAS read for one rebalance. Every source
/// is best effort: a failure leaves that part of the card empty.
pub struct Extras {
    pub news: HashMap<String, Vec<Headline>>,
    pub india_vix: Option<Decimal>,
    pub shadow_top: Vec<(String, Decimal)>,
}

pub async fn gather(
    feed: &UpstoxLiveMarketFeed,
    decision: &MomentumDecision,
    holdings: &HashMap<String, i64>,
    config: &MomentumConfig,
    nifty: &[Decimal],
    today_ist: chrono::NaiveDate,
) -> Extras {
    let google = GoogleNews::default();
    let mut news = HashMap::new();
    let symbols = decision.targets.iter().chain(holdings.keys());
    for s in symbols {
        if news.contains_key(s) {
            continue;
        }
        let company = find_nse_instrument(s)
            .map(|i| {
                i.name
                    .trim_end_matches(" Ltd")
                    .trim_end_matches(" Limited")
                    .to_string()
            })
            .unwrap_or_else(|| s.clone());
        match google.headlines(&company).await {
            Ok(h) => {
                news.insert(s.clone(), h);
            }
            Err(e) => tracing::warn!(symbol = %s, error = %e, "ORION could not read Google News"),
        }
    }
    let india_vix = feed
        .fetch_daily_closes(INDIA_VIX_KEY, 5, today_ist)
        .await
        .ok()
        .and_then(|v| v.last().copied());

    let days = config.lookback_days.max(config.regime_sma_days) + 10;
    let mut closes = HashMap::new();
    for (symbol, key) in NIFTY50 {
        if let Ok(series) = feed.fetch_daily_closes(key, days, today_ist).await {
            closes.insert(symbol.to_string(), series);
        }
    }
    // The shadow list ignores the regime: it is what the wider universe ranks
    // highest, for comparison, not an order.
    let shadow = select_targets(&closes, nifty, config);
    let shadow_top = shadow
        .ranked
        .into_iter()
        .filter(|(_, m)| *m > Decimal::ZERO)
        .take(config.top_n)
        .collect();
    Extras {
        news,
        india_vix,
        shadow_top,
    }
}

// ---------------------------------------------------------------- the card

#[derive(Debug, Clone, Serialize)]
pub struct IndiaVega {
    pub nifty_close: Option<Decimal>,
    pub nifty_sma200: Option<Decimal>,
    pub regime_on: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndiaLuna {
    pub india_vix: Option<Decimal>,
    /// Watchlist share above its 50-day average, percent.
    pub breadth_above_50dma_pct: Option<Decimal>,
    pub mood: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndiaAtlas {
    /// The watchlist ranked by momentum (percent), best first.
    pub ranked: Vec<(String, Decimal)>,
    /// What a 51-stock universe would hold (advisory).
    pub shadow_top: Vec<(String, Decimal)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Pick {
    pub symbol: String,
    pub momentum_pct: Option<Decimal>,
    /// BUY, SELL, HOLD, or SKIP (a target TITAN could not afford).
    pub action: String,
    pub qty: i64,
    pub price: Option<Decimal>,
    pub above_50dma: Option<bool>,
    pub headlines_24h: usize,
    pub events: Vec<String>,
    pub latest: Option<String>,
    pub tone: Decimal,
    pub titan: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndiaNova {
    /// CASH (regime off), REBALANCE, or NO_CHANGE.
    pub action: String,
    pub reasons: Vec<String>,
    /// What the advisory agents flagged; none of these changed an order.
    pub advisory: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndiaCard {
    pub at: DateTime<Utc>,
    pub market: String,
    pub strategy: String,
    pub atlas: IndiaAtlas,
    pub vega: IndiaVega,
    pub luna: IndiaLuna,
    pub picks: Vec<Pick>,
    pub nova: IndiaNova,
}

/// Everything the desk read for one rebalance.
pub struct DeskInputs<'a> {
    pub decision: &'a MomentumDecision,
    pub holdings: &'a HashMap<String, i64>,
    pub sells: &'a [(String, i64)],
    pub buys: &'a [(String, i64)],
    pub prices: &'a HashMap<String, Decimal>,
    pub closes: &'a HashMap<String, Vec<Decimal>>,
    pub news: &'a HashMap<String, Vec<Headline>>,
    pub india_vix: Option<Decimal>,
    pub shadow_top: Vec<(String, Decimal)>,
    pub slot_inr: Decimal,
    pub now: DateTime<Utc>,
}

fn above_sma(series: &[Decimal], n: usize) -> Option<bool> {
    (series.len() >= n).then(|| {
        let sma = series[series.len() - n..].iter().copied().sum::<Decimal>() / Decimal::from(n);
        *series.last().expect("non-empty") > sma
    })
}

pub fn assemble(i: &DeskInputs) -> IndiaCard {
    let d = i.decision;
    let pct = |m: Decimal| (m * dec!(100)).round_dp(2);
    let momentum: HashMap<&str, Decimal> = d.ranked.iter().map(|(s, m)| (s.as_str(), *m)).collect();

    // Every symbol the month touches: targets and current holdings.
    let mut symbols: Vec<String> = d.targets.clone();
    for s in i.holdings.keys() {
        if !symbols.contains(s) {
            symbols.push(s.clone());
        }
    }
    let mut advisory = Vec::new();
    let mut picks = Vec::new();
    for s in &symbols {
        let buy = i.buys.iter().find(|(b, _)| b == s).map(|(_, q)| *q);
        let sell = i.sells.iter().find(|(b, _)| b == s).map(|(_, q)| *q);
        let held = i.holdings.get(s).copied().unwrap_or(0);
        let price = i.prices.get(s).copied();
        let (action, qty, titan) = match (buy, sell) {
            (Some(q), _) => (
                "BUY",
                q,
                format!("{q} shares fit the Rs {} slot", i.slot_inr.round_dp(0)),
            ),
            (_, Some(q)) => ("SELL", q, "closing: no longer a target".to_string()),
            _ if held > 0 => ("HOLD", held, "kept: still a target".to_string()),
            _ => (
                "SKIP",
                0,
                match price {
                    Some(p) => {
                        format!("one share at Rs {p} does not fit the cash left after charges")
                    }
                    None => "no live price".to_string(),
                },
            ),
        };
        let news = i.news.get(s).map(Vec::as_slice).unwrap_or(&[]);
        let recent: Vec<Headline> = news
            .iter()
            .filter(|h| i.now.signed_duration_since(h.at) <= chrono::Duration::hours(24))
            .cloned()
            .collect();
        let mut events: Vec<String> = Vec::new();
        for h in &recent {
            for t in tag_events(&h.headline) {
                if !events.iter().any(|e| e == t) {
                    events.push(t.to_string());
                }
            }
        }
        let above = i.closes.get(s).and_then(|c| above_sma(c, 50));
        if action == "BUY" {
            if events.iter().any(|e| e == "governance") {
                advisory.push(format!(
                    "ORION: governance news on {s} (pledge/SEBI/fraud/default) - would skip"
                ));
            }
            if above == Some(false) {
                advisory.push(format!("VEGA: {s} is below its 50-day average"));
            }
        }
        picks.push(Pick {
            symbol: s.clone(),
            momentum_pct: momentum.get(s.as_str()).map(|m| pct(*m)),
            action: action.to_string(),
            qty,
            price,
            above_50dma: above,
            headlines_24h: recent.len(),
            latest: recent
                .iter()
                .max_by_key(|h| h.at)
                .map(|h| h.headline.clone()),
            tone: tone(&recent),
            events,
            titan,
        });
    }

    let above: Vec<bool> = i.closes.values().filter_map(|c| above_sma(c, 50)).collect();
    let breadth = (!above.is_empty()).then(|| {
        (Decimal::from(above.iter().filter(|b| **b).count() * 100) / Decimal::from(above.len()))
            .round_dp(1)
    });
    let mood = match (i.india_vix, breadth) {
        (Some(v), _) if v >= dec!(25) => "fearful",
        (Some(v), Some(b)) if v <= dec!(15) && b >= dec!(60) => "calm and broad",
        (_, Some(b)) if b < dec!(40) => "narrow",
        _ => "neutral",
    }
    .to_string();
    if i.india_vix.is_some_and(|v| v >= dec!(25)) && !i.buys.is_empty() {
        advisory
            .push("LUNA: India VIX >= 25, a fearful market - would buy at half size".to_string());
    }
    let shadow_syms: Vec<&String> = i.shadow_top.iter().map(|(s, _)| s).collect();
    if d.regime_ok && !shadow_syms.is_empty() && shadow_syms.iter().any(|s| !d.targets.contains(s))
    {
        advisory.push(format!(
            "ATLAS: a 51-stock universe would hold {:?} (not validated: survivorship-biased backtest)",
            shadow_syms
        ));
    }

    let (action, reasons) = if !d.regime_ok {
        let why = match (d.nifty_close, d.nifty_sma) {
            (Some(c), Some(s)) => format!(
                "VEGA: NIFTY {c} is below its 200-day average {}",
                s.round_dp(1)
            ),
            _ => "VEGA: NIFTY history unavailable, so no regime".to_string(),
        };
        (
            "CASH",
            vec![why, "hold cash until next month's check".to_string()],
        )
    } else if i.sells.is_empty() && i.buys.is_empty() {
        (
            "NO_CHANGE",
            vec![format!("book already holds the targets {:?}", d.targets)],
        )
    } else {
        (
            "REBALANCE",
            vec![format!(
                "VEGA: NIFTY above its 200-day average; ATLAS targets {:?}; TITAN sized {} buys and {} sells",
                d.targets,
                i.buys.len(),
                i.sells.len()
            )],
        )
    };

    IndiaCard {
        at: i.now,
        market: "IN".to_string(),
        strategy: "momentum".to_string(),
        atlas: IndiaAtlas {
            ranked: d.ranked.iter().map(|(s, m)| (s.clone(), pct(*m))).collect(),
            shadow_top: i
                .shadow_top
                .iter()
                .map(|(s, m)| (s.clone(), pct(*m)))
                .collect(),
        },
        vega: IndiaVega {
            nifty_close: d.nifty_close,
            nifty_sma200: d.nifty_sma.map(|s| s.round_dp(2)),
            regime_on: d.regime_ok,
        },
        luna: IndiaLuna {
            india_vix: i.india_vix,
            breadth_above_50dma_pct: breadth,
            mood,
        },
        picks,
        nova: IndiaNova {
            action: action.to_string(),
            reasons,
            advisory,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<rss><channel><title>x</title>
<item><title>ICICI Bank Q2 results: net profit rises 12% - Business Standard</title>
<pubDate>Fri, 30 Oct 2026 04:00:00 GMT</pubDate><source url="https://bs.com">Business Standard</source></item>
<item><title>SEBI probe into promoter pledge &amp; disclosures</title>
<pubDate>Fri, 30 Oct 2026 05:00:00 GMT</pubDate><source url="x">NDTV Profit</source></item>
<item><title>No date here</title></item>
</channel></rss>"#;

    fn decision(regime: bool, targets: &[&str]) -> MomentumDecision {
        MomentumDecision {
            regime_ok: regime,
            nifty_close: Some(dec!(22936.75)),
            nifty_sma: Some(dec!(24050.0132)),
            ranked: vec![
                ("ICICIBANK".into(), dec!(0.0305)),
                ("LT".into(), dec!(0.0114)),
            ],
            targets: targets.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn rss_items_parse_with_source_and_entities() {
        let items = parse_rss(RSS);
        assert_eq!(items.len(), 2, "undated items are dropped");
        assert!(items[0].headline.starts_with("ICICI Bank Q2 results"));
        assert_eq!(items[0].source, "Business Standard");
        assert!(items[1].headline.contains("pledge & disclosures"));
    }

    #[test]
    fn indian_events_are_tagged() {
        assert_eq!(
            tag_events("ICICI Bank Q2 results: net profit rises"),
            vec!["results"]
        );
        assert!(tag_events("SEBI probe into promoter pledge").contains(&"governance"));
        assert!(
            tag_events("Board approves bonus issue, record date set").contains(&"corporate_action")
        );
    }

    #[test]
    fn regime_off_is_a_cash_card_and_advisories_do_not_change_it() {
        let d = decision(false, &[]);
        let empty = HashMap::new();
        let card = assemble(&DeskInputs {
            decision: &d,
            holdings: &HashMap::new(),
            sells: &[],
            buys: &[],
            prices: &empty,
            closes: &HashMap::new(),
            news: &HashMap::new(),
            india_vix: Some(dec!(15.12)),
            shadow_top: vec![("TRENT".into(), dec!(0.4))],
            slot_inr: dec!(6250),
            now: Utc::now(),
        });
        assert_eq!(card.nova.action, "CASH");
        assert!(card.nova.reasons[0].contains("below its 200-day average"));
        assert!(card.picks.is_empty());
        assert!(
            card.nova.advisory.is_empty(),
            "no shadow note while in cash"
        );
    }

    #[test]
    fn a_buy_with_governance_news_is_flagged_but_still_bought() {
        let d = decision(true, &["ICICIBANK", "LT"]);
        let prices = HashMap::from([
            ("ICICIBANK".to_string(), dec!(1400)),
            ("LT".to_string(), dec!(9000)),
        ]);
        let mut news = HashMap::new();
        news.insert(
            "ICICIBANK".to_string(),
            parse_rss(RSS)
                .into_iter()
                .map(|mut h| {
                    h.at = Utc::now();
                    h
                })
                .collect(),
        );
        let card = assemble(&DeskInputs {
            decision: &d,
            holdings: &HashMap::new(),
            sells: &[],
            buys: &[("ICICIBANK".to_string(), 4)],
            prices: &prices,
            closes: &HashMap::new(),
            news: &news,
            india_vix: Some(dec!(26)),
            shadow_top: vec![("TRENT".into(), dec!(0.4))],
            slot_inr: dec!(6250),
            now: Utc::now(),
        });
        assert_eq!(card.nova.action, "REBALANCE");
        let icici = card.picks.iter().find(|p| p.symbol == "ICICIBANK").unwrap();
        assert_eq!((icici.action.as_str(), icici.qty), ("BUY", 4));
        assert!(icici.events.contains(&"governance".to_string()));
        let lt = card.picks.iter().find(|p| p.symbol == "LT").unwrap();
        assert_eq!(lt.action, "SKIP", "a target TITAN could not afford");
        let notes = card.nova.advisory.join(" | ");
        assert!(notes.contains("ORION: governance news on ICICIBANK"));
        assert!(notes.contains("LUNA: India VIX >= 25"));
        assert!(notes.contains("TRENT"));
    }
}
