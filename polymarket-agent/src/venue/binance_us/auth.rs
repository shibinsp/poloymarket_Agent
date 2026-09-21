//! Binance.US request signing.
//!
//! Every private endpoint takes an HMAC-SHA256 of the **exact query string
//! sent**, appended as a `signature` parameter, with the API key in an
//! `X-MBX-APIKEY` header. That "exact" is the whole difficulty: sign
//! `symbol=BTCUSD&quantity=0.001` and then let the HTTP client re-encode the
//! parameters in a different order — or percent-encode a character the signer
//! left raw — and Binance answers `-1022 Signature for this request is not
//! valid`, which names nothing about which byte differed.
//!
//! So the query string is built once, here, and the caller sends that string
//! verbatim rather than handing a parameter list to `reqwest`.

use anyhow::{Context, Result};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Signs Binance.US requests.
pub struct BinanceSigner {
    api_key: String,
    secret_key: String,
}

/// Hand-written: the secret must never reach a log line or a panic message.
impl std::fmt::Debug for BinanceSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BinanceSigner")
            // Even the key half is an identifier an attacker would want, so
            // only its length is shown.
            .field("api_key_len", &self.api_key.len())
            .field("secret_key", &"<redacted>")
            .finish()
    }
}

impl BinanceSigner {
    pub fn new(api_key: impl Into<String>, secret_key: impl Into<String>) -> Result<Self> {
        let api_key = api_key.into();
        let secret_key = secret_key.into();
        anyhow::ensure!(
            !api_key.trim().is_empty(),
            "Binance.US API key must not be empty"
        );
        anyhow::ensure!(
            !secret_key.trim().is_empty(),
            "Binance.US secret key must not be empty"
        );
        Ok(Self {
            api_key,
            secret_key,
        })
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// The hex HMAC-SHA256 of `query_string` under the secret.
    ///
    /// Lower-case hex: Binance compares the string, and an upper-case digest
    /// fails with the same unhelpful `-1022` as a wrong secret.
    pub fn sign(&self, query_string: &str) -> Result<String> {
        let mut mac = HmacSha256::new_from_slice(self.secret_key.as_bytes())
            .context("Binance.US secret key could not be used as an HMAC key")?;
        mac.update(query_string.as_bytes());
        Ok(hex_lower(&mac.finalize().into_bytes()))
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Infallible against a String.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Build a query string, percent-encoding values the way the signature
/// assumes they were encoded.
///
/// Order is preserved rather than sorted: Binance signs the literal string, so
/// the only requirement is that what is signed is what is sent, and keeping
/// the caller's order makes the two trivially the same.
pub fn query_string(params: &[(&str, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// `application/x-www-form-urlencoded` encoding, which is what Binance's
/// signature examples assume.
///
/// `urlencoding::encode` escapes everything outside the unreserved set, which
/// is a superset of what is needed and always matches on the server side.
fn encode(value: &str) -> String {
    urlencoding::encode(value).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> BinanceSigner {
        BinanceSigner::new(
            "api-key",
            "NhqPtmdSJYdKjVHjA7PZj4Mge3R5YNiP1e3UZjInClVN65XAbvqqM6A7H5fATj0j",
        )
        .unwrap()
    }

    /// The vector from Binance's own signing documentation. If this drifts,
    /// every private call fails with `-1022` and nothing says which byte
    /// differed.
    #[test]
    fn the_signature_matches_binances_published_vector() {
        let query = "symbol=LTCBTC&side=BUY&type=LIMIT&timeInForce=GTC\
                     &quantity=1&price=0.1&recvWindow=5000&timestamp=1499827319559";
        assert_eq!(
            signer().sign(query).unwrap(),
            "c8db56825ae71d6d79447849e617115f4a920fa2acdcab2b053c4b2838bd6b71"
        );
    }

    #[test]
    fn the_digest_is_lower_case_hex_of_the_full_length() {
        let sig = signer().sign("a=1").unwrap();
        assert_eq!(sig.len(), 64, "SHA-256 is 32 bytes");
        assert_eq!(sig, sig.to_lowercase(), "Binance compares the string");
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_different_query_signs_differently() {
        let s = signer();
        assert_ne!(
            s.sign("symbol=BTCUSD&quantity=1").unwrap(),
            s.sign("symbol=BTCUSD&quantity=2").unwrap()
        );
    }

    /// Order is part of the signed string, so the builder must not reorder.
    /// Sorting here and sending the caller's order — or the reverse — is the
    /// `-1022` that looks like a wrong secret.
    #[test]
    fn the_query_string_keeps_the_order_it_was_given() {
        let params = [
            ("symbol", "BTCUSD".to_string()),
            ("side", "BUY".to_string()),
            ("timestamp", "1".to_string()),
        ];
        assert_eq!(query_string(&params), "symbol=BTCUSD&side=BUY&timestamp=1");
    }

    /// A client order id is a UUID, which is safe — but the encoding has to be
    /// applied *before* signing, or the signed string and the sent string
    /// differ for any value that needs escaping.
    #[test]
    fn values_are_percent_encoded_in_the_signed_string() {
        let params = [("newClientOrderId", "a b+c/d".to_string())];
        let qs = query_string(&params);
        assert!(
            !qs.contains(' '),
            "a raw space would not survive the wire: {qs}"
        );
        assert!(qs.contains("%2F") || qs.contains("%2f"), "{qs}");
        assert!(!qs.contains("+c/"), "{qs}");
    }

    #[test]
    fn empty_credentials_are_refused_at_construction() {
        assert!(BinanceSigner::new("  ", "secret").is_err());
        assert!(BinanceSigner::new("key", "").is_err());
    }

    #[test]
    fn the_debug_impl_leaks_neither_half_of_the_credential() {
        let rendered = format!("{:?}", signer());
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("NhqPtmdSJYdK"), "{rendered}");
        assert!(!rendered.contains("api-key"), "{rendered}");
    }
}
