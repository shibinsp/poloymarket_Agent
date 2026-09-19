//! Thin authenticated HTTP layer for Alpaca's two APIs.
//!
//! Alpaca splits trading (`/v2/account`, `/v2/orders`, …) and market data
//! (`/v2/stocks/*`, `/v1beta3/crypto/*`) across separate hosts that share one
//! key pair. Both hosts — and the paper/live choice — are constructor
//! arguments so tests can point them at a mock server.

use std::fmt;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::{Method, RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Alpaca's key header. Documented as `APCA-API-KEY-ID`.
const KEY_HEADER: &str = "APCA-API-KEY-ID";
/// Alpaca's secret header. Documented as `APCA-API-SECRET-KEY`.
const SECRET_HEADER: &str = "APCA-API-SECRET-KEY";

/// Which Alpaca host a request belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    /// Trading: account, orders, positions, assets, clock.
    Trading,
    /// Market data: quotes, bars, orderbooks.
    Data,
}

/// Authenticated Alpaca HTTP client.
pub struct AlpacaRest {
    http: reqwest::Client,
    trading_base_url: String,
    data_base_url: String,
    key_id: String,
    secret_key: String,
}

/// Hand-written so credentials can never reach a log line or panic message.
impl fmt::Debug for AlpacaRest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AlpacaRest")
            .field("trading_base_url", &self.trading_base_url)
            .field("data_base_url", &self.data_base_url)
            .field("key_id", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .finish()
    }
}

impl AlpacaRest {
    pub fn new(
        trading_base_url: impl Into<String>,
        data_base_url: impl Into<String>,
        key_id: impl Into<String>,
        secret_key: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self> {
        let key_id = key_id.into();
        let secret_key = secret_key.into();
        if key_id.trim().is_empty() || secret_key.trim().is_empty() {
            bail!("Alpaca API key id and secret key must both be non-empty");
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .context("Failed to build the Alpaca HTTP client")?;
        Ok(Self {
            http,
            trading_base_url: trading_base_url.into().trim_end_matches('/').to_string(),
            data_base_url: data_base_url.into().trim_end_matches('/').to_string(),
            key_id,
            secret_key,
        })
    }

    pub fn trading_base_url(&self) -> &str {
        &self.trading_base_url
    }

    pub fn data_base_url(&self) -> &str {
        &self.data_base_url
    }

    fn url(&self, api: Api, path: &str) -> String {
        let base = match api {
            Api::Trading => &self.trading_base_url,
            Api::Data => &self.data_base_url,
        };
        format!("{base}{path}")
    }

    fn authed(&self, builder: RequestBuilder) -> RequestBuilder {
        builder
            .header(KEY_HEADER, &self.key_id)
            .header(SECRET_HEADER, &self.secret_key)
    }

    /// `GET` returning a decoded body.
    pub async fn get<T: DeserializeOwned>(
        &self,
        api: Api,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let url = self.url(api, path);
        let request = self.authed(self.http.get(&url)).query(query);
        let body = self.send(Method::GET, request, &url).await?;
        decode(&body, Method::GET, &url)
    }

    /// `POST` with a JSON body, returning a decoded body.
    pub async fn post<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        api: Api,
        path: &str,
        payload: &B,
    ) -> Result<T> {
        let url = self.url(api, path);
        let request = self.authed(self.http.post(&url)).json(payload);
        let body = self.send(Method::POST, request, &url).await?;
        decode(&body, Method::POST, &url)
    }

    /// `DELETE` returning the raw body, which is empty for `204 No Content`
    /// (a single cancel) and a JSON array for `207 Multi-Status` (cancel all).
    pub async fn delete_raw(
        &self,
        api: Api,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<String> {
        let url = self.url(api, path);
        let request = self.authed(self.http.delete(&url)).query(query);
        self.send(Method::DELETE, request, &url).await
    }

    /// Execute and turn any non-2xx into an error that carries the status code
    /// and the response body — without those a failed order is undebuggable.
    async fn send(&self, method: Method, request: RequestBuilder, url: &str) -> Result<String> {
        let response = request
            .send()
            .await
            .with_context(|| format!("Alpaca {method} {url} request failed"))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            bail!(
                "Alpaca {method} {url} failed: HTTP {}: {}",
                status.as_u16(),
                snippet(&body)
            );
        }
        if status == StatusCode::NO_CONTENT {
            return Ok(String::new());
        }
        Ok(body)
    }
}

fn decode<T: DeserializeOwned>(body: &str, method: Method, url: &str) -> Result<T> {
    serde_json::from_str(body).with_context(|| {
        format!(
            "Alpaca {method} {url} returned a body this client cannot parse: {}",
            snippet(body)
        )
    })
}

/// Keep error messages bounded — an Alpaca 500 can return an HTML page.
fn snippet(body: &str) -> String {
    const MAX: usize = 512;
    if body.len() <= MAX {
        return body.to_string();
    }
    let mut end = MAX;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} bytes total)", &body[..end], body.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_credentials_are_rejected_at_construction() {
        let err =
            AlpacaRest::new("https://t", "https://d", "", "s", Duration::from_secs(5)).unwrap_err();
        assert!(err.to_string().contains("non-empty"), "{err}");
    }

    #[test]
    fn debug_never_prints_credentials() {
        let rest = AlpacaRest::new(
            "https://paper-api.alpaca.markets/",
            "https://data.alpaca.markets",
            "AKTOPSECRETKEYID",
            "topsecretsecretkey",
            Duration::from_secs(5),
        )
        .unwrap();
        let rendered = format!("{rest:?}");
        assert!(!rendered.contains("AKTOPSECRETKEYID"), "{rendered}");
        assert!(!rendered.contains("topsecretsecretkey"), "{rendered}");
        // Trailing slashes are normalised away so paths don't double up.
        assert_eq!(rest.trading_base_url(), "https://paper-api.alpaca.markets");
    }

    #[test]
    fn snippet_truncates_on_a_char_boundary() {
        let body = "é".repeat(1000);
        let out = snippet(&body);
        assert!(out.contains("bytes total"));
        // Would have panicked on a mid-character slice.
        assert!(out.len() < body.len());
    }
}
