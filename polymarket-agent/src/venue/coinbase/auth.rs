//! Coinbase Developer Platform request signing.
//!
//! CDP does not use a static header pair the way Alpaca does. Every request
//! carries a short-lived ES256 JWT whose `uri` claim names the exact method,
//! host and path being called, so a token captured from one request cannot be
//! replayed against another endpoint. It expires in two minutes.
//!
//! That per-request binding is why this is built by hand rather than with
//! `jsonwebtoken`: CDP also requires a `nonce` in the *header*, and
//! `jsonwebtoken::Header` has no room for a non-standard field. Signing is
//! `p256` (NIST P-256) — not alloy's `k256`, which is a different curve and
//! would produce signatures Coinbase rejects without saying why.

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;
use p256::SecretKey;

/// How long a CDP token is valid. Coinbase's documented maximum is two
/// minutes; tokens are minted per request, so this is a ceiling rather than a
/// lifetime anything depends on.
const TOKEN_TTL_SECONDS: u64 = 120;

/// Signs CDP requests.
///
/// Holds the parsed key rather than the PEM so the secret is decoded once at
/// startup — a malformed key then fails at construction, where an operator is
/// looking, instead of on the first order.
pub struct CdpSigner {
    /// The API key name, e.g.
    /// `organizations/{org_id}/apiKeys/{key_id}`. Public.
    key_name: String,
    signing_key: SigningKey,
}

/// Hand-written: the key must never reach a log line or a panic message.
impl std::fmt::Debug for CdpSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CdpSigner")
            .field("key_name", &self.key_name)
            .field("signing_key", &"<redacted>")
            .finish()
    }
}

impl CdpSigner {
    /// `private_key_pem` is the EC key Coinbase issues alongside the key name.
    ///
    /// Accepts both encodings Coinbase has handed out: PKCS#8
    /// (`BEGIN PRIVATE KEY`) and SEC1 (`BEGIN EC PRIVATE KEY`). Downloading a
    /// key and finding the agent rejects it for a header nobody documents is
    /// a bad first five minutes.
    pub fn new(key_name: impl Into<String>, private_key_pem: &str) -> Result<Self> {
        let key_name = key_name.into();
        if key_name.trim().is_empty() {
            bail!("Coinbase API key name must not be empty");
        }

        // JSON-escaped newlines are how the key arrives when it is pasted
        // out of the downloaded JSON and into an env var. Without this the
        // PEM parse fails with "PEM error", which says nothing useful.
        let pem = private_key_pem.replace("\\n", "\n");
        let pem = pem.trim();

        let secret = SecretKey::from_pkcs8_pem(pem)
            .or_else(|_| SecretKey::from_sec1_pem(pem))
            .context(
                "Coinbase private key is not a valid P-256 PEM (expected \
                 'BEGIN PRIVATE KEY' or 'BEGIN EC PRIVATE KEY')",
            )?;

        Ok(Self {
            key_name,
            signing_key: SigningKey::from(secret),
        })
    }

    /// A bearer token valid for one request.
    ///
    /// `host` carries no scheme and `path` no query string — CDP builds the
    /// `uri` claim as `METHOD host/path`, and a mismatch is a 401 with no
    /// explanation of which part was wrong.
    pub fn token(&self, method: &str, host: &str, path: &str, now_unix: u64) -> Result<String> {
        let uri = format!("{} {host}{path}", method.to_uppercase());

        // The nonce exists to make otherwise-identical tokens distinct.
        // `uuid` is already a dependency and is exactly as unpredictable as
        // this needs to be.
        let nonce = uuid::Uuid::new_v4().simple().to_string();

        let header = serde_json::json!({
            "alg": "ES256",
            "kid": self.key_name,
            "nonce": nonce,
            "typ": "JWT",
        });
        let claims = serde_json::json!({
            "iss": "cdp",
            "sub": self.key_name,
            "nbf": now_unix,
            "exp": now_unix + TOKEN_TTL_SECONDS,
            "uri": uri,
        });

        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header)?),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?),
        );

        // JWS wants the raw r‖s pair, not the DER encoding `to_der` produces.
        // DER is the shape most ECDSA examples show, and Coinbase answers it
        // with the same opaque 401 as a wrong key.
        let signature: Signature = self.signing_key.sign(signing_input.as_bytes());
        let encoded = URL_SAFE_NO_PAD.encode(signature.to_bytes());

        Ok(format!("{signing_input}.{encoded}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::VerifyingKey;

    /// A throwaway P-256 key in PKCS#8 PEM. Generated for this test and used
    /// nowhere else.
    const PKCS8_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgevZzL1gdAFr88hb2\n\
OF/2NxApJCzGCEDdfSp6VQO30hyhRANCAAQRWz+jn65BtOMvdyHKcvjBeBSDZH2r\n\
1RTwjmYSi9R/zpBnuQ4EiMnCqfMPWiZqB4QdbAd0E7oH50VpuZ1P087G\n\
-----END PRIVATE KEY-----";

    fn signer() -> CdpSigner {
        CdpSigner::new("organizations/o/apiKeys/k", PKCS8_PEM).expect("valid key")
    }

    fn decode_part(part: &str) -> serde_json::Value {
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).expect("base64")).expect("json")
    }

    #[test]
    fn the_token_has_three_base64url_parts() {
        let token = signer()
            .token("GET", "api.coinbase.com", "/x", 1_700_000_000)
            .unwrap();
        assert_eq!(token.split('.').count(), 3);
        assert!(
            !token.contains('=') && !token.contains('+') && !token.contains('/'),
            "JWS uses base64url without padding: {token}"
        );
    }

    #[test]
    fn the_header_carries_the_key_name_and_a_nonce() {
        let token = signer()
            .token("GET", "api.coinbase.com", "/x", 1_700_000_000)
            .unwrap();
        let header = decode_part(token.split('.').next().unwrap());
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["typ"], "JWT");
        assert_eq!(header["kid"], "organizations/o/apiKeys/k");
        assert!(
            header["nonce"].as_str().is_some_and(|n| !n.is_empty()),
            "CDP requires a header nonce: {header}"
        );
    }

    /// The claim that makes a captured token useless elsewhere. If this is
    /// wrong the failure is a 401 that says nothing about which part of the
    /// URI did not match.
    #[test]
    fn the_uri_claim_binds_the_method_host_and_path() {
        let token = signer()
            .token(
                "get",
                "api.coinbase.com",
                "/api/v3/brokerage/accounts",
                1_700_000_000,
            )
            .unwrap();
        let claims = decode_part(token.split('.').nth(1).unwrap());
        assert_eq!(
            claims["uri"], "GET api.coinbase.com/api/v3/brokerage/accounts",
            "method is upper-cased and host and path are concatenated"
        );
        assert_eq!(claims["iss"], "cdp");
        assert_eq!(claims["sub"], "organizations/o/apiKeys/k");
    }

    #[test]
    fn a_different_path_produces_a_different_uri_claim() {
        let s = signer();
        let a = s.token("GET", "api.coinbase.com", "/accounts", 1).unwrap();
        let b = s.token("GET", "api.coinbase.com", "/orders", 1).unwrap();
        assert_ne!(
            decode_part(a.split('.').nth(1).unwrap())["uri"],
            decode_part(b.split('.').nth(1).unwrap())["uri"]
        );
    }

    #[test]
    fn the_token_expires_two_minutes_after_it_is_minted() {
        let token = signer().token("GET", "h", "/p", 1_700_000_000).unwrap();
        let claims = decode_part(token.split('.').nth(1).unwrap());
        assert_eq!(claims["nbf"], 1_700_000_000u64);
        assert_eq!(claims["exp"], 1_700_000_000u64 + 120);
    }

    /// Raw `r‖s`, not DER. DER is what most ECDSA examples produce and
    /// Coinbase rejects it with the same opaque 401 as a wrong key, so this
    /// pins the length: P-256 gives 32 bytes each.
    #[test]
    fn the_signature_is_sixty_four_raw_bytes_not_der() {
        let token = signer().token("GET", "h", "/p", 1).unwrap();
        let sig = URL_SAFE_NO_PAD
            .decode(token.split('.').nth(2).unwrap())
            .expect("base64");
        assert_eq!(sig.len(), 64, "r‖s for P-256");
        assert_ne!(sig[0], 0x30, "0x30 is a DER SEQUENCE tag");
    }

    #[test]
    fn the_signature_verifies_against_the_public_key() {
        let s = signer();
        let token = s
            .token("GET", "api.coinbase.com", "/x", 1_700_000_000)
            .unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let sig = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();

        let verifying = VerifyingKey::from(&s.signing_key);
        verifying
            .verify(signing_input.as_bytes(), &sig)
            .expect("the token must verify against its own key");
    }

    #[test]
    fn two_tokens_for_the_same_request_differ() {
        // The nonce's whole job. Identical tokens would be replayable within
        // the two-minute window.
        let s = signer();
        let a = s.token("GET", "h", "/p", 1).unwrap();
        let b = s.token("GET", "h", "/p", 1).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn a_key_with_escaped_newlines_still_parses() {
        // How the key arrives when pasted out of the downloaded JSON into an
        // env var. Rejecting it gives "PEM error" and no clue why.
        let escaped = PKCS8_PEM.replace('\n', "\\n");
        assert!(CdpSigner::new("organizations/o/apiKeys/k", &escaped).is_ok());
    }

    #[test]
    fn a_malformed_key_fails_at_construction_not_at_the_first_order() {
        let err = CdpSigner::new(
            "k",
            "-----BEGIN PRIVATE KEY-----\nnope\n-----END PRIVATE KEY-----",
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("P-256"),
            "the error should name what was expected: {err:#}"
        );
    }

    #[test]
    fn an_empty_key_name_is_refused() {
        assert!(CdpSigner::new("  ", PKCS8_PEM).is_err());
    }

    #[test]
    fn the_debug_impl_does_not_leak_the_key() {
        let rendered = format!("{:?}", signer());
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("MIGHAgEA"), "{rendered}");
    }
}
