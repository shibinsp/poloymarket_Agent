//! Live U.S. news (5 Nov 2026): every Benzinga headline Alpaca streams, as it
//! is published, kept in memory for the agent (read at /api/us/news; no page).
//!
//! - Stream: `wss://stream.data.alpaca.markets/v1beta1/news`, all symbols.
//! - On each (re)connect the last 50 headlines are read over REST first, so the
//!   board is never empty after a restart.
//! - Each headline is tagged with ORION's event words (earnings, merger, halt...)
//!   and LUNA's tone, and marked when it names a stock the ORB watches.
//!
//! Display only: no strategy reads it. ORION still reads its own 24 hours of
//! headlines at entry time.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use crate::us::desk::{tag_events, tone, Headline};

const STREAM_URL: &str = "wss://stream.data.alpaca.markets/v1beta1/news";
const REST_URL: &str = "https://data.alpaca.markets/v1beta1/news";
/// Headlines kept in memory, newest first.
const KEEP: usize = 500;

#[derive(Debug, Clone, Serialize)]
pub struct NewsItem {
    pub id: i64,
    pub at: DateTime<Utc>,
    /// When the agent received it (equal to `at` for the REST backfill).
    pub received_at: DateTime<Utc>,
    pub headline: String,
    pub summary: String,
    pub source: String,
    pub url: String,
    pub symbols: Vec<String>,
    /// ORION's event tags: earnings, guidance, merger, offering, halt, regulatory, analyst.
    pub tags: Vec<String>,
    /// LUNA's headline tone, -1 (negative) .. +1 (positive).
    pub tone: Decimal,
    /// Names a stock on the ORB's watch list.
    pub watched: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NewsBoard {
    /// CONNECTING, LIVE, RECONNECTING, AUTH_REQUIRED.
    pub status: String,
    pub connected_since: Option<DateTime<Utc>>,
    pub last_message_at: Option<DateTime<Utc>>,
    /// Headlines received on the stream since the agent started.
    pub streamed: u64,
    pub items: VecDeque<NewsItem>,
}

pub type LiveNews = Arc<RwLock<NewsBoard>>;

/// One Alpaca news record, from the stream (`T` = "n") or the REST page.
#[derive(Debug, Deserialize)]
struct RawItem {
    #[serde(rename = "T", default)]
    kind: String,
    #[serde(default)]
    id: i64,
    created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    headline: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    symbols: Vec<String>,
}

#[derive(Deserialize)]
struct RestPage {
    #[serde(default)]
    news: Vec<RawItem>,
}

fn to_item(raw: RawItem, received_at: DateTime<Utc>, watch: &[String]) -> Option<NewsItem> {
    if raw.headline.trim().is_empty() {
        return None;
    }
    let at = raw.created_at.unwrap_or(received_at);
    let tags = tag_events(&format!("{} {}", raw.headline, raw.summary))
        .into_iter()
        .map(str::to_string)
        .collect();
    let tone = tone(&[Headline {
        at,
        headline: raw.headline.clone(),
        summary: String::new(),
        source: String::new(),
    }]);
    let watched = raw.symbols.iter().any(|s| watch.iter().any(|w| w == s));
    Some(NewsItem {
        id: raw.id,
        at,
        received_at,
        headline: raw.headline,
        summary: raw.summary,
        source: raw.source,
        url: raw.url.unwrap_or_default(),
        symbols: raw.symbols,
        tags,
        tone,
        watched,
    })
}

/// Adds a headline unless its id is already on the board; newest first.
fn insert(board: &mut NewsBoard, item: NewsItem) -> bool {
    if item.id != 0 && board.items.iter().any(|i| i.id == item.id) {
        return false;
    }
    let pos = board
        .items
        .iter()
        .position(|i| i.at <= item.at)
        .unwrap_or(board.items.len());
    board.items.insert(pos, item);
    board.items.truncate(KEEP);
    true
}

async fn backfill(
    http: &reqwest::Client,
    key: &str,
    secret: &str,
    watch: &[String],
    board: &LiveNews,
) -> anyhow::Result<usize> {
    let resp = http
        .get(REST_URL)
        .header("APCA-API-KEY-ID", key)
        .header("APCA-API-SECRET-KEY", secret)
        .query(&[("limit", "50"), ("sort", "desc")])
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("Alpaca news returned {}", resp.status());
    }
    let page: RestPage = resp.json().await?;
    let mut b = board.write().await;
    let mut added = 0;
    for raw in page.news {
        let at = raw.created_at.unwrap_or_else(Utc::now);
        if let Some(item) = to_item(raw, at, watch) {
            added += insert(&mut b, item) as usize;
        }
    }
    Ok(added)
}

/// Streams Alpaca news for the life of the agent, reconnecting on any drop.
pub fn spawn_news_stream(key: String, secret: String, watch: Vec<String>) -> (LiveNews, JoinHandle<()>) {
    let board: LiveNews = Arc::new(RwLock::new(NewsBoard {
        status: "CONNECTING".to_string(),
        ..Default::default()
    }));
    let out = board.clone();
    let handle = tokio::spawn(async move {
        let http = reqwest::Client::builder()
            .timeout(StdDuration::from_secs(15))
            .build()
            .expect("HTTP client");
        let mut backoff = 5u64;
        loop {
            match backfill(&http, &key, &secret, &watch, &board).await {
                Ok(n) => info!(added = n, "Live news: recent headlines loaded"),
                Err(e) => warn!(error = %e, "Live news: could not load recent headlines"),
            }
            match connect_async(STREAM_URL).await {
                Ok((ws, _)) => {
                    let (mut write, mut read) = ws.split();
                    let auth = serde_json::json!({ "action": "auth", "key": key, "secret": secret });
                    let sub = serde_json::json!({ "action": "subscribe", "news": ["*"] });
                    let mut ok = write.send(Message::Text(auth.to_string().into())).await.is_ok();
                    let mut subscribed = false;
                    while ok {
                        let Some(msg) = read.next().await else { break };
                        match msg {
                            Ok(Message::Text(txt)) => {
                                let now = Utc::now();
                                let Ok(records) = serde_json::from_str::<Vec<serde_json::Value>>(&txt) else {
                                    continue;
                                };
                                for rec in records {
                                    let kind = rec.get("T").and_then(|v| v.as_str()).unwrap_or("");
                                    let m = rec.get("msg").and_then(|v| v.as_str()).unwrap_or("");
                                    match kind {
                                        "success" if m == "authenticated" && !subscribed => {
                                            subscribed = true;
                                            ok = write.send(Message::Text(sub.to_string().into())).await.is_ok();
                                        }
                                        "subscription" => {
                                            let mut b = board.write().await;
                                            b.status = "LIVE".to_string();
                                            b.connected_since = Some(now);
                                            backoff = 5;
                                            info!("Live news stream subscribed to all symbols");
                                        }
                                        "error" => {
                                            warn!(response = %rec, "Live news stream error");
                                            if m.contains("auth") {
                                                board.write().await.status = "AUTH_REQUIRED".to_string();
                                            }
                                            ok = false;
                                        }
                                        "n" => {
                                            if let Ok(raw) = serde_json::from_value::<RawItem>(rec) {
                                                debug_assert_eq!(raw.kind, "n");
                                                if let Some(item) = to_item(raw, now, &watch) {
                                                    let mut b = board.write().await;
                                                    b.last_message_at = Some(now);
                                                    if insert(&mut b, item) {
                                                        b.streamed += 1;
                                                    }
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Ok(Message::Ping(p)) => {
                                ok = write.send(Message::Pong(p)).await.is_ok();
                            }
                            Ok(Message::Close(_)) | Err(_) => break,
                            _ => {}
                        }
                    }
                    warn!("Live news stream dropped; reconnecting");
                }
                Err(e) => warn!(error = %e, "Live news: could not connect to the stream"),
            }
            {
                let mut b = board.write().await;
                if b.status != "AUTH_REQUIRED" {
                    b.status = "RECONNECTING".to_string();
                }
                b.connected_since = None;
            }
            tokio::time::sleep(StdDuration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(300);
        }
    });
    (out, handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(id: i64, mins: i64, headline: &str, symbols: &[&str]) -> RawItem {
        RawItem {
            kind: "n".into(),
            id,
            created_at: Some(Utc::now() - chrono::Duration::minutes(mins)),
            headline: headline.into(),
            summary: String::new(),
            source: "benzinga".into(),
            url: None,
            symbols: symbols.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn a_stream_record_parses_and_is_tagged() {
        let txt = r#"[{"T":"n","id":24918784,"headline":"AMD Q3 Earnings Beat, Shares Jump","summary":"","author":"x","created_at":"2026-11-04T21:05:00Z","updated_at":"2026-11-04T21:05:00Z","url":"https://example.com/a","content":"","symbols":["AMD"],"source":"benzinga"}]"#;
        let recs: Vec<serde_json::Value> = serde_json::from_str(txt).unwrap();
        let r: RawItem = serde_json::from_value(recs[0].clone()).unwrap();
        let item = to_item(r, Utc::now(), &["AMD".to_string()]).unwrap();
        assert!(item.tags.contains(&"earnings".to_string()));
        assert!(item.tone > Decimal::ZERO);
        assert!(item.watched);
        assert_eq!(item.url, "https://example.com/a");
    }

    #[test]
    fn the_board_keeps_newest_first_and_drops_repeats() {
        let mut b = NewsBoard::default();
        let w: Vec<String> = vec![];
        assert!(insert(&mut b, to_item(raw(1, 10, "older", &[]), Utc::now(), &w).unwrap()));
        assert!(insert(&mut b, to_item(raw(2, 1, "newer", &[]), Utc::now(), &w).unwrap()));
        assert!(!insert(&mut b, to_item(raw(1, 10, "older", &[]), Utc::now(), &w).unwrap()));
        assert_eq!(b.items.iter().map(|i| i.id).collect::<Vec<_>>(), vec![2, 1]);
        assert!(to_item(raw(3, 0, "  ", &[]), Utc::now(), &w).is_none());
    }
}
