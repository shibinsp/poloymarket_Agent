//! Authenticated HTTP for Binance.US.
//!
//! The signature covers the exact query string sent, so this module builds
//! that string itself and passes it to `reqwest` as a pre-formed URL. Handing
//! `reqwest` a parameter list instead would let it re-encode or reorder them
//! after signing, which Binance answers with `-1022 Signature for this request
//! is not valid` — a message that names neither the parameter nor the byte.

use std::fmt;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;

use super::auth::{query_string, BinanceSigner};
use super::models::ApiError;

/// How much clock skew Binance will tolerate on a signed request.
///
/// Binance's own maximum is 60s. Five seconds is its documented default and a
/// tighter replay window; `-1021` then names a real clock problem rather than
/// a slow network.
const RECV_WINDOW_MS: u64 = 5_000;

pub struct BinanceRest {
    http: reqwest::Client,
    base_url: String,
    signer: BinanceSigner,
}

impl fmt::Debug for BinanceRest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BinanceRest")
            .field("base_url", &self.base_url)
            .field("signer", &self.signer)
            .finish()
    }
}

impl BinanceRest {
    pub fn new(
        base_url: impl Into<String>,
        signer: BinanceSigner,
        timeout: Duration,
    ) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        anyhow::ensure!(
            !base_url.is_empty(),
            "Binance.US base_url must not be empty"
        );

        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .context("Failed to build the Binance.US HTTP client")?;

        Ok(Self {
            http,
            base_url,
            signer,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// An unsigned market-data call.
    pub async fn public<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<T> {
        let url = self.url(path, &query_string(params));
        let response = self.send(&Method::GET, &url, false).await?;
        decode(&response, Method::GET, path)
    }

    /// A signed call. `now_ms` is threaded in so the timestamp is testable.
    pub async fn signed<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        params: &[(&str, String)],
        now_ms: u64,
    ) -> Result<T> {
        // Timestamp and recvWindow are part of the signed string, so they are
        // appended before signing rather than by the HTTP client after it.
        let mut all: Vec<(&str, String)> = params.to_vec();
        all.push(("recvWindow", RECV_WINDOW_MS.to_string()));
        all.push(("timestamp", now_ms.to_string()));

        let query = query_string(&all);
        let signature = self.signer.sign(&query)?;
        // Appended, not inserted: the signature covers everything before it.
        let url = self.url(path, &format!("{query}&signature={signature}"));

        let response = self.send(&method, &url, true).await?;
        decode(&response, method, path)
    }

    fn url(&self, path: &str, query: &str) -> String {
        if query.is_empty() {
            format!("{}{path}", self.base_url)
        } else {
            format!("{}{path}?{query}", self.base_url)
        }
    }

    /// Execute, and turn any non-2xx into an error carrying Binance's own
    /// code — without it a rejected order is undebuggable, and the code is the
    /// part that says whether to look at the order, the clock or the key.
    /// Builds the request itself rather than taking one already bound to a
    /// verb: passing both left two sources of truth for which method was sent
    /// and cloned it twice to say so.
    async fn send(&self, method: &Method, url: &str, authed: bool) -> Result<String> {
        let mut request = self.http.request(method.clone(), url);
        if authed {
            request = request.header("X-MBX-APIKEY", self.signer.api_key());
        }
        let response = request
            .send()
            .await
            // `without_url` first, and not merely for tidiness: reqwest's own
            // error carries the URL it was given, the URL carries the
            // signature, and `{e:#}` prints the whole chain. Redacting only
            // the context this code adds leaves the secret in the source
            // error one line below it.
            .map_err(|e| e.without_url())
            .with_context(|| format!("Binance.US {method} {} request failed", redact(url)))?;

        let status = response.status();
        // A read that fails mid-body is not an empty body; collapsing the two
        // erases the reason from the error.
        let body = response
            .text()
            .await
            .map_err(|e| e.without_url())
            .with_context(|| {
                format!(
                    "Binance.US {method} {} answered HTTP {} but the body could not be read",
                    redact(url),
                    status.as_u16()
                )
            })?;

        if !status.is_success() {
            if let Ok(api) = serde_json::from_str::<ApiError>(&body) {
                if api.code != 0 {
                    let hint = api.hint().map(|h| format!(" — {h}")).unwrap_or_default();
                    bail!(
                        "Binance.US {method} {} failed: {} ({}){hint}",
                        redact(url),
                        api.message(),
                        api.code
                    );
                }
            }
            bail!(
                "Binance.US {method} {} failed: HTTP {}: {}",
                redact(url),
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

/// Drop the query string: it carries the signature and the API key's own
/// parameters, and error messages end up in logs.
fn redact(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

fn decode<T: DeserializeOwned>(body: &str, method: Method, path: &str) -> Result<T> {
    serde_json::from_str(body).with_context(|| {
        format!(
            "Binance.US {method} {path} returned an unexpected body: {}",
            snippet(body)
        )
    })
}

/// Bodies go into error messages; a full one is unreadable in a log line.
fn snippet(body: &str) -> String {
    const MAX: usize = 300;
    if body.len() <= MAX {
        return body.to_string();
    }
    let mut cut = MAX;
    while !body.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &body[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The signature is in the query string, and error messages reach logs.
    #[test]
    fn a_signed_url_never_appears_whole_in_an_error() {
        let url = "https://api.binance.us/api/v3/account?timestamp=1&signature=deadbeef";
        assert_eq!(redact(url), "https://api.binance.us/api/v3/account");
        assert!(!redact(url).contains("signature"));
    }

    #[test]
    fn a_url_without_a_query_is_unchanged() {
        assert_eq!(
            redact("https://api.binance.us/api/v3/ping"),
            "https://api.binance.us/api/v3/ping"
        );
    }

    #[test]
    fn a_long_body_is_truncated_on_a_character_boundary() {
        let body = "é".repeat(400);
        let out = snippet(&body);
        assert!(out.ends_with('…'));
        assert!(out.len() <= 303);
    }
}
