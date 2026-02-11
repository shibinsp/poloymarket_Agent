//! Sports data source.
//!
//! Fetches schedules, scores, and injury reports from ESPN's public API
//! to inform sports-related prediction markets.

use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use rust_decimal_macros::dec;
use serde::Deserialize;

use crate::data::{DataPoint, DataSource, MarketQuery};
use crate::market::models::MarketCategory;

/// Supported ESPN sport endpoints.
const SPORT_ENDPOINTS: &[(&str, &str)] = &[
    ("nfl", "football/nfl"),
    ("nba", "basketball/nba"),
    ("mlb", "baseball/mlb"),
    ("nhl", "hockey/nhl"),
    ("mma", "mma/ufc"),
    ("soccer", "soccer/usa.1"),
];

pub struct SportsSource {
    client: reqwest::Client,
}

impl SportsSource {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client");

        Self { client }
    }

    async fn fetch_scoreboard(&self, sport_path: &str) -> Result<EspnScoreboard> {
        let url = format!("https://site.api.espn.com/apis/site/v2/sports/{sport_path}/scoreboard");
        let response: EspnScoreboard = self
            .client
            .get(&url)
            .send()
            .await
            .context("ESPN scoreboard request failed")?
            .json()
            .await
            .context("Failed to parse ESPN scoreboard")?;
        Ok(response)
    }
}

#[async_trait]
impl DataSource for SportsSource {
    async fn fetch(&self, queries: &[MarketQuery]) -> Result<Vec<DataPoint>> {
        let mut points = Vec::new();

        for (sport_name, sport_path) in SPORT_ENDPOINTS {
            // Check if any queries relate to this sport
            let sport_lower = sport_name.to_lowercase();
            let has_relevant = queries.iter().any(|q| {
                let ql = q.question.to_lowercase();
                ql.contains(&sport_lower)
                    || ql.contains("game")
                    || ql.contains("win")
                    || ql.contains("score")
                    || ql.contains("championship")
                    || ql.contains("playoff")
                    || ql.contains("super bowl")
                    || ql.contains("world series")
            });

            if !has_relevant {
                continue;
            }

            match self.fetch_scoreboard(sport_path).await {
                Ok(scoreboard) => {
                    for event in &scoreboard.events {
                        let teams: Vec<serde_json::Value> = event
                            .competitions
                            .iter()
                            .flat_map(|c| &c.competitors)
                            .map(|comp| {
                                serde_json::json!({
                                    "name": comp.team.display_name,
                                    "abbreviation": comp.team.abbreviation,
                                    "score": comp.score,
                                    "home_away": comp.home_away,
                                    "winner": comp.winner,
                                })
                            })
                            .collect();

                        let status = event
                            .competitions
                            .first()
                            .map(|c| &c.status)
                            .map(|s| {
                                serde_json::json!({
                                    "type": s.type_detail.description,
                                    "completed": s.type_detail.completed,
                                })
                            })
                            .unwrap_or(serde_json::json!(null));

                        let payload = serde_json::json!({
                            "sport": sport_name,
                            "event_name": event.name,
                            "date": event.date,
                            "teams": teams,
                            "status": status,
                        });

                        // Match to relevant market queries by team names
                        let relevance: Vec<String> = queries
                            .iter()
                            .filter(|q| {
                                let ql = q.question.to_lowercase();
                                event.competitions.iter().any(|c| {
                                    c.competitors.iter().any(|comp| {
                                        ql.contains(&comp.team.display_name.to_lowercase())
                                            || ql.contains(
                                                &comp.team.abbreviation.to_lowercase(),
                                            )
                                    })
                                }) || ql.contains(&sport_lower)
                            })
                            .map(|q| q.condition_id.clone())
                            .collect();

                        points.push(DataPoint {
                            source: format!("espn_{sport_name}"),
                            category: MarketCategory::Sports,
                            timestamp: Utc::now(),
                            payload,
                            confidence: dec!(0.85),
                            relevance_to: relevance,
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!(sport = sport_name, error = %e, "Failed to fetch sport data");
                }
            }
        }

        Ok(points)
    }

    fn category(&self) -> MarketCategory {
        MarketCategory::Sports
    }

    fn freshness_window(&self) -> Duration {
        Duration::from_secs(300) // 5 minutes — scores change rapidly
    }

    fn name(&self) -> &str {
        "espn_sports"
    }
}

// --- ESPN API Response Types ---

#[derive(Debug, Deserialize)]
struct EspnScoreboard {
    events: Vec<EspnEvent>,
}

#[derive(Debug, Deserialize)]
struct EspnEvent {
    name: String,
    date: String,
    competitions: Vec<EspnCompetition>,
}

#[derive(Debug, Deserialize)]
struct EspnCompetition {
    competitors: Vec<EspnCompetitor>,
    status: EspnStatus,
}

#[derive(Debug, Deserialize)]
struct EspnCompetitor {
    team: EspnTeam,
    score: Option<String>,
    #[serde(rename = "homeAway")]
    home_away: String,
    winner: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EspnTeam {
    display_name: String,
    abbreviation: String,
}

#[derive(Debug, Deserialize)]
struct EspnStatus {
    #[serde(rename = "type")]
    type_detail: EspnStatusType,
}

#[derive(Debug, Deserialize)]
struct EspnStatusType {
    description: String,
    completed: bool,
}
