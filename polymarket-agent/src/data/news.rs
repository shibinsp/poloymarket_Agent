//! General news and event data source.
//!
//! Fetches headlines from public news APIs to inform
//! political and general prediction markets.

use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use rust_decimal_macros::dec;
use serde::Deserialize;

use crate::data::{DataPoint, DataSource, MarketQuery};
use crate::market::models::MarketCategory;

pub struct NewsSource {
    client: reqwest::Client,
}

impl NewsSource {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client");

        Self { client }
    }

    /// Fetch news from a public RSS-to-JSON proxy or news API.
    /// Uses Google News RSS via a public endpoint.
    async fn fetch_headlines(&self, search_terms: &[String]) -> Result<Vec<NewsArticle>> {
        let mut articles = Vec::new();

        for term in search_terms {
            let encoded = urlencoding::encode(term);
            let url = format!(
                "https://news.google.com/rss/search?q={encoded}&hl=en-US&gl=US&ceid=US:en"
            );

            match self.client.get(&url).send().await {
                Ok(response) => {
                    let body = response.text().await.unwrap_or_default();
                    // Parse RSS XML — extract title and link from <item> elements
                    let parsed = parse_rss_items(&body);
                    articles.extend(parsed.into_iter().map(|item| NewsArticle {
                        title: item.0,
                        link: item.1,
                        search_term: term.clone(),
                    }));
                }
                Err(e) => {
                    tracing::warn!(term, error = %e, "Failed to fetch news for term");
                }
            }
        }

        Ok(articles)
    }
}

#[async_trait]
impl DataSource for NewsSource {
    async fn fetch(&self, queries: &[MarketQuery]) -> Result<Vec<DataPoint>> {
        // Extract key search terms from market questions
        let search_terms: Vec<String> = queries
            .iter()
            .map(|q| extract_search_term(&q.question))
            .collect();

        if search_terms.is_empty() {
            return Ok(Vec::new());
        }

        let articles = self.fetch_headlines(&search_terms).await?;
        let mut points = Vec::new();

        for article in &articles {
            let payload = serde_json::json!({
                "title": article.title,
                "link": article.link,
                "search_term": article.search_term,
            });

            // Match back to relevant market queries
            let relevance: Vec<String> = queries
                .iter()
                .filter(|q| {
                    let ql = q.question.to_lowercase();
                    let al = article.title.to_lowercase();
                    // Check for keyword overlap
                    ql.split_whitespace()
                        .filter(|w| w.len() > 3)
                        .any(|word| al.contains(word))
                })
                .map(|q| q.condition_id.clone())
                .collect();

            points.push(DataPoint {
                source: "google_news".to_string(),
                category: MarketCategory::Politics,
                timestamp: Utc::now(),
                payload,
                confidence: dec!(0.5), // News headlines have lower signal quality
                relevance_to: relevance,
            });
        }

        Ok(points)
    }

    fn category(&self) -> MarketCategory {
        MarketCategory::Politics
    }

    fn freshness_window(&self) -> Duration {
        Duration::from_secs(600) // 10 minutes
    }

    fn name(&self) -> &str {
        "google_news"
    }
}

/// Extract a simplified search term from a market question.
fn extract_search_term(question: &str) -> String {
    // Remove common question prefixes
    let q = question
        .replace("Will ", "")
        .replace("will ", "")
        .replace("Is ", "")
        .replace("is ", "")
        .replace("?", "");

    // Take first 5 meaningful words
    q.split_whitespace()
        .filter(|w| !["the", "a", "an", "be", "by", "in", "on", "at", "to", "of", "or", "and"].contains(w))
        .take(5)
        .collect::<Vec<&str>>()
        .join(" ")
}

/// Minimal RSS XML parser — extracts (title, link) pairs from <item> elements.
fn parse_rss_items(xml: &str) -> Vec<(String, String)> {
    let mut items = Vec::new();

    for item_block in xml.split("<item>").skip(1) {
        let title = extract_xml_tag(item_block, "title").unwrap_or_default();
        let link = extract_xml_tag(item_block, "link").unwrap_or_default();
        if !title.is_empty() {
            items.push((title, link));
        }
        // Limit to 10 articles per search
        if items.len() >= 10 {
            break;
        }
    }

    items
}

fn extract_xml_tag(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text.find(&close)?;
    if start < end {
        Some(text[start..end].trim().to_string())
    } else {
        None
    }
}

struct NewsArticle {
    title: String,
    link: String,
    search_term: String,
}
