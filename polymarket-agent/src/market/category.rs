//! Keyword-based market category detection.
//!
//! Infers a market's category from the question text using keyword matching.
//! This enables the portfolio concentration limit (`max_positions_per_category`)
//! which was previously broken because all markets were categorized as "Other".

use crate::market::models::MarketCategory;

/// Infer market category from the question text using keyword matching.
///
/// Falls back to `Other("unclassified")` if no keywords match.
pub fn infer_category(question: &str) -> MarketCategory {
    let q = question.to_lowercase();

    // Weather
    if contains_any(&q, &[
        "weather", "rain", "snow", "temperature", "storm", "hurricane",
        "tornado", "flood", "drought", "celsius", "fahrenheit", "forecast",
        "heatwave", "heat wave", "cold snap", "wildfire",
    ]) {
        return MarketCategory::Weather;
    }

    // Sports
    if contains_any(&q, &[
        "nfl", "nba", "nhl", "mlb", "mma", "ufc", "soccer", "football",
        "basketball", "baseball", "hockey", "tennis", "golf", "boxing",
        "championship", "super bowl", "world cup", "world series",
        "playoffs", "finals", "mvp", "draft", "premier league",
        "champions league", "match", "bout", "fight",
    ]) {
        return MarketCategory::Sports;
    }

    // Crypto
    if contains_any(&q, &[
        "bitcoin", "ethereum", "btc", "eth", "solana", "sol", "dogecoin",
        "doge", "crypto", "cryptocurrency", "blockchain", "token", "defi",
        "nft", "altcoin", "stablecoin", "ripple", "xrp", "cardano",
        "polkadot", "avalanche", "polygon", "matic", "binance", "coinbase",
        "mining", "halving",
    ]) {
        return MarketCategory::Crypto;
    }

    // Politics
    if contains_any(&q, &[
        "election", "vote", "ballot", "congress", "senate", "house",
        "president", "governor", "mayor", "democrat", "republican",
        "legislation", "bill", "law", "policy", "impeach", "cabinet",
        "supreme court", "parliament", "prime minister", "referendum",
        "midterm", "inaugurat",
    ]) {
        return MarketCategory::Politics;
    }

    MarketCategory::Other("unclassified".to_string())
}

/// Check if text contains any of the given keywords.
fn contains_any(text: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|kw| text.contains(kw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crypto_detection() {
        assert_eq!(
            infer_category("Will Bitcoin reach $100k by March 2026?"),
            MarketCategory::Crypto
        );
        assert_eq!(
            infer_category("Will ETH flip BTC in market cap?"),
            MarketCategory::Crypto
        );
    }

    #[test]
    fn test_sports_detection() {
        assert_eq!(
            infer_category("Who wins the Super Bowl?"),
            MarketCategory::Sports
        );
        assert_eq!(
            infer_category("Will the Lakers win the NBA championship?"),
            MarketCategory::Sports
        );
    }

    #[test]
    fn test_weather_detection() {
        assert_eq!(
            infer_category("Will it rain in NYC on Feb 20?"),
            MarketCategory::Weather
        );
        assert_eq!(
            infer_category("Will temperature exceed 100F in Phoenix?"),
            MarketCategory::Weather
        );
    }

    #[test]
    fn test_politics_detection() {
        assert_eq!(
            infer_category("Will the bill pass the Senate?"),
            MarketCategory::Politics
        );
        assert_eq!(
            infer_category("Who wins the 2028 presidential election?"),
            MarketCategory::Politics
        );
    }

    #[test]
    fn test_unclassified_fallback() {
        assert_eq!(
            infer_category("Will aliens be discovered by 2030?"),
            MarketCategory::Other("unclassified".to_string())
        );
    }
}
