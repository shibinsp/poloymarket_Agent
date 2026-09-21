//! Authenticated HTTP for Coinbase Advanced Trade.
//!
//! One host, unlike Alpaca's split of trading and market data. The base URL is
//! a constructor argument so tests can point it at a mock server — and
//! because the JWT's `uri` claim names the host, the signer has to be told
//! the same one the request goes to, which is easy to get wrong when they are
//! derived separately.

use std::fmt;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::{Method, RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;
use serde::Serialize;

use super::auth::CdpSigner;

pub struct CoinbaseRest {
    http: reqwest::Client,
    base_url: String,
    /// Host as it appears in the JWT `uri` claim: no scheme, no trailing
    /// slash, port included when there is one. Derived from `base_url` once
    /// rather than at each call, so the two cannot drift.
    host: String,
    signer: CdpSigner,
}

impl fmt::Debug for CoinbaseRest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoinbaseRest")
            .field("base_url", &self.base_url)
            .field("signer", &self.signer)
            .finish()
    }
}

impl CoinbaseRest {
    pub fn new(base_url: impl Into<String>, signer: CdpSigner, timeout: Duration) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let host = host_of(&base_url)
            .with_context(|| format!("Coinbase base_url has no host: {base_url}"))?;

        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .context("Failed to build the Coinbase HTTP client")?;

        Ok(Self {
            http,
            base_url,
            host,
            signer,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// `GET` returning a decoded body.
    ///
    /// `path` must carry no query string: it goes into the JWT's `uri` claim,
    /// which Coinbase computes without the query, and a mismatch is a 401
    /// that names nothing.
    pub async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let url = format!("{}{path}", self.base_url);
        let request = self.authed(self.http.get(&url), "GET", path)?.query(query);
        let body = self.send(Method::GET, request, &url).await?;
        decode(&body, Method::GET, &url)
    }

    pub async fn post<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        payload: &B,
    ) -> Result<T> {
        let url = format!("{}{path}", self.base_url);
        let request = self
            .authed(self.http.post(&url), "POST", path)?
            .json(payload);
        let body = self.send(Method::POST, request, &url).await?;
        decode(&body, Method::POST, &url)
    }

    fn authed(&self, builder: RequestBuilder, method: &str, path: &str) -> Result<RequestBuilder> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_secs();
        let token = self.signer.token(method, &self.host, path, now)?;
        Ok(builder.bearer_auth(token))
    }

    /// Execute, and turn any non-2xx into an error carrying the status and
    /// body — without those a rejected order is undebuggable.
    async fn send(&self, method: Method, request: RequestBuilder, url: &str) -> Result<String> {
        let response = request
            .send()
            .await
            .with_context(|| format!("Coinbase {method} {url} request failed"))?;

        let status = response.status();
        // A read that fails mid-body is not an empty body. Collapsing the two
        // turns a connection reset on a 200 into "returned an unexpected body:"
        // with nothing after the colon, and erases the reason from an HTTP
        // error — the opposite of why this function carries the body at all.
        let body = response.text().await.with_context(|| {
            format!(
                "Coinbase {method} {url} answered HTTP {} but the body could not be read",
                status.as_u16()
            )
        })?;

        if !status.is_success() {
            bail!(
                "Coinbase {method} {url} failed: HTTP {}: {}",
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

/// Host and port, without the scheme — the form the JWT `uri` claim wants.
fn host_of(base_url: &str) -> Option<String> {
    let after_scheme = base_url.split("://").nth(1).unwrap_or(base_url);
    let host = after_scheme.split('/').next()?;
    (!host.is_empty()).then(|| host.to_string())
}

fn decode<T: DeserializeOwned>(body: &str, method: Method, url: &str) -> Result<T> {
    serde_json::from_str(body).with_context(|| {
        format!(
            "Coinbase {method} {url} returned an unexpected body: {}",
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

    #[test]
    fn the_host_drops_the_scheme_and_path() {
        assert_eq!(
            host_of("https://api.coinbase.com").as_deref(),
            Some("api.coinbase.com")
        );
        assert_eq!(
            host_of("https://api.coinbase.com/api/v3").as_deref(),
            Some("api.coinbase.com")
        );
    }

    /// Mock servers are `http://127.0.0.1:PORT`. The port is part of the host
    /// for the `uri` claim, and dropping it would make every test token fail
    /// to match in a way that only shows up against the real API.
    #[test]
    fn the_host_keeps_the_port() {
        assert_eq!(
            host_of("http://127.0.0.1:9911").as_deref(),
            Some("127.0.0.1:9911")
        );
    }

    #[test]
    fn a_url_with_no_host_is_rejected() {
        assert_eq!(host_of(""), None);
        assert_eq!(host_of("https://"), None);
    }

    #[test]
    fn a_long_body_is_truncated_on_a_character_boundary() {
        // Multi-byte characters straddling the cut would panic on a slice.
        let body = "é".repeat(400);
        let out = snippet(&body);
        assert!(out.ends_with('…'));
        assert!(out.len() <= 303);
    }
}
