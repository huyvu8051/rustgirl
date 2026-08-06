//! Turns an `AuthConfig` into whatever needs to happen to the outgoing
//! request — a couple of static headers for most kinds, a query param for
//! `ApiKey`-in-query, a stateful challenge-response for `Digest`, or a token
//! fetched first for `OAuth2`. `prepare_auth` dispatches by `AuthKind` and
//! returns a [`PreparedAuth`] describing which of those shapes applies;
//! `http_client::send_request` is what actually acts on it, since only it
//! has the live `reqwest::Client`/`RequestBuilder`.
//!
//! `AuthConfig::params` keys, matching Phase 1's doc comment on `AuthConfig`
//! (chosen to mirror Postman's own v2.1 auth JSON so import/export stays a
//! direct field copy):
//! - `Basic`/`Digest`: `username`, `password`
//! - `Bearer`: `token`
//! - `ApiKey`: `key`, `value`, `in` (`"header"` or `"query"`)
//! - `OAuth1`: `consumerKey`, `consumerSecret`, `token`, `tokenSecret`
//!   (only the `HMAC-SHA1` signature method is supported — the overwhelming
//!   majority of real OAuth1 usage — not `PLAINTEXT`/`RSA-SHA1`)
//! - `OAuth2`: `accessToken` (used directly as a Bearer token if non-empty —
//!   covers "I already have a token, just paste it"), else `tokenUrl`/
//!   `clientId`/`clientSecret`/`scope` for a Client Credentials grant.
//!   Authorization Code (needs a system browser + local redirect listener)
//!   is a deferred follow-up, not implemented here.
//! - `AwsSigV4`: `accessKey`, `secretKey`, `region` (default `us-east-1`),
//!   `service` (default `execute-api`), `sessionToken`

use crate::model::{AuthConfig, AuthKind, Method};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use hmac::{Hmac, KeyInit, Mac};
use md5::Digest as _;
use sha2::Sha256;

type HmacSha1 = Hmac<sha1::Sha1>;
type HmacSha256 = Hmac<Sha256>;

/// What `http_client::send_request` needs to do to apply auth — the shape
/// varies enough by kind (static headers vs. a fetched token vs. a
/// challenge-response dance) that one enum covering all of them is simpler
/// than a uniform "apply" function each kind would have to shoehorn into.
pub enum PreparedAuth {
    Headers(Vec<(String, String)>),
    Query(Vec<(String, String)>),
    /// No header is added yet — send once, and if the response is 401 with
    /// a `WWW-Authenticate: Digest ...` challenge, compute the response and
    /// resend (RFC 2617).
    DigestChallenge {
        username: String,
        password: String,
    },
    /// `params["accessToken"]` was empty, so a token has to be fetched
    /// (Client Credentials grant) before the real request can go out.
    OAuth2ClientCredentials {
        token_url: String,
        client_id: String,
        client_secret: String,
        scope: Option<String>,
    },
    None,
}

fn param(auth: &AuthConfig, key: &str) -> String {
    auth.params
        .iter()
        .find(|kv| kv.enabled && kv.key == key)
        .map(|kv| kv.value.clone())
        .unwrap_or_default()
}

fn non_empty_or(value: String, default: &str) -> String {
    if value.is_empty() {
        default.to_string()
    } else {
        value
    }
}

/// `payload_for_signing`/`unsigned_payload` only matter for `AwsSigV4`
/// (its signature covers a hash of the body) — callers resolve the body
/// bytes themselves since only `http_client.rs` knows the final, resolved
/// body per `BodyMode`.
pub fn prepare_auth(
    auth: &AuthConfig,
    method: &Method,
    url: &str,
    query: &[(String, String)],
    payload_for_signing: &[u8],
    unsigned_payload: bool,
) -> PreparedAuth {
    match auth.kind {
        AuthKind::Inherit | AuthKind::None => PreparedAuth::None,
        AuthKind::Basic => {
            let encoded = BASE64.encode(format!(
                "{}:{}",
                param(auth, "username"),
                param(auth, "password")
            ));
            PreparedAuth::Headers(vec![(
                "Authorization".to_string(),
                format!("Basic {encoded}"),
            )])
        }
        AuthKind::Bearer => PreparedAuth::Headers(vec![(
            "Authorization".to_string(),
            format!("Bearer {}", param(auth, "token")),
        )]),
        AuthKind::ApiKey => {
            let key = param(auth, "key");
            let value = param(auth, "value");
            if param(auth, "in") == "query" {
                PreparedAuth::Query(vec![(key, value)])
            } else {
                PreparedAuth::Headers(vec![(key, value)])
            }
        }
        AuthKind::Digest => PreparedAuth::DigestChallenge {
            username: param(auth, "username"),
            password: param(auth, "password"),
        },
        AuthKind::OAuth1 => PreparedAuth::Headers(vec![(
            "Authorization".to_string(),
            oauth1_header(auth, method, url, query),
        )]),
        AuthKind::OAuth2 => {
            let access_token = param(auth, "accessToken");
            if !access_token.is_empty() {
                PreparedAuth::Headers(vec![(
                    "Authorization".to_string(),
                    format!("Bearer {access_token}"),
                )])
            } else {
                let scope = param(auth, "scope");
                PreparedAuth::OAuth2ClientCredentials {
                    token_url: param(auth, "tokenUrl"),
                    client_id: param(auth, "clientId"),
                    client_secret: param(auth, "clientSecret"),
                    scope: (!scope.is_empty()).then_some(scope),
                }
            }
        }
        AuthKind::AwsSigV4 => PreparedAuth::Headers(aws_sigv4_headers(
            auth,
            method,
            url,
            payload_for_signing,
            unsigned_payload,
        )),
    }
}

// ---------------------------------------------------------------------------
// Shared: percent-encoding (RFC 3986, unreserved chars only) — stricter than
// typical URL-encoding, used by both OAuth1's signature base string and AWS
// SigV4's canonical query string.
// ---------------------------------------------------------------------------

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// OAuth 1.0 (RFC 5849), HMAC-SHA1 only
// ---------------------------------------------------------------------------

fn oauth1_header(
    auth: &AuthConfig,
    method: &Method,
    url: &str,
    query: &[(String, String)],
) -> String {
    let consumer_key = param(auth, "consumerKey");
    let consumer_secret = param(auth, "consumerSecret");
    let token = param(auth, "token");
    let token_secret = param(auth, "tokenSecret");

    let mut oauth_params = vec![
        ("oauth_consumer_key".to_string(), consumer_key),
        (
            "oauth_nonce".to_string(),
            uuid::Uuid::new_v4().simple().to_string(),
        ),
        (
            "oauth_signature_method".to_string(),
            "HMAC-SHA1".to_string(),
        ),
        (
            "oauth_timestamp".to_string(),
            chrono::Utc::now().timestamp().to_string(),
        ),
        ("oauth_version".to_string(), "1.0".to_string()),
    ];
    if !token.is_empty() {
        oauth_params.push(("oauth_token".to_string(), token));
    }

    let base_url = url.split('?').next().unwrap_or(url);
    let mut all_params = oauth_params.clone();
    all_params.extend(query.iter().cloned());
    all_params.sort();
    let param_string = all_params
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    let base_string = format!(
        "{}&{}&{}",
        method.as_str(),
        percent_encode(base_url),
        percent_encode(&param_string)
    );
    let signing_key = format!(
        "{}&{}",
        percent_encode(&consumer_secret),
        percent_encode(&token_secret)
    );

    let mut mac =
        HmacSha1::new_from_slice(signing_key.as_bytes()).expect("HMAC accepts a key of any length");
    mac.update(base_string.as_bytes());
    let signature = BASE64.encode(mac.finalize().into_bytes());

    oauth_params.push(("oauth_signature".to_string(), signature));
    oauth_params.sort();
    let header_params = oauth_params
        .iter()
        .map(|(k, v)| format!(r#"{}="{}""#, k, percent_encode(v)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("OAuth {header_params}")
}

// ---------------------------------------------------------------------------
// AWS Signature v4
// ---------------------------------------------------------------------------

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn aws_sigv4_headers(
    auth: &AuthConfig,
    method: &Method,
    url: &str,
    payload: &[u8],
    unsigned_payload: bool,
) -> Vec<(String, String)> {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return Vec::new();
    };
    let host = parsed.host_str().unwrap_or("").to_string();
    let canonical_uri = {
        let path = parsed.path();
        if path.is_empty() {
            "/".to_string()
        } else {
            path.to_string()
        }
    };
    let canonical_query = {
        let mut pairs: Vec<(String, String)> = parsed
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        pairs.sort();
        pairs
            .iter()
            .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
            .collect::<Vec<_>>()
            .join("&")
    };

    let now = chrono::Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    let payload_hash = if unsigned_payload {
        "UNSIGNED-PAYLOAD".to_string()
    } else {
        hex::encode(Sha256::digest(payload))
    };

    let access_key = param(auth, "accessKey");
    let secret_key = param(auth, "secretKey");
    let region = non_empty_or(param(auth, "region"), "us-east-1");
    let service = non_empty_or(param(auth, "service"), "execute-api");
    let session_token = param(auth, "sessionToken");

    let authorization = aws_sigv4_authorization_header(
        &access_key,
        &secret_key,
        &region,
        &service,
        method.as_str(),
        &canonical_uri,
        &canonical_query,
        &host,
        &amz_date,
        &date_stamp,
        &payload_hash,
    );

    let mut headers = vec![
        ("Authorization".to_string(), authorization),
        ("X-Amz-Date".to_string(), amz_date),
    ];
    if !session_token.is_empty() {
        headers.push(("X-Amz-Security-Token".to_string(), session_token));
    }
    headers
}

/// The pure signing computation, parameterized over everything including
/// the timestamp — `aws_sigv4_headers` supplies `amz_date`/`date_stamp` from
/// the current time; kept separate so it can be tested against AWS's own
/// published "get-vanilla" test vector with fixed values.
#[allow(clippy::too_many_arguments)]
fn aws_sigv4_authorization_header(
    access_key: &str,
    secret_key: &str,
    region: &str,
    service: &str,
    method: &str,
    canonical_uri: &str,
    canonical_query: &str,
    host: &str,
    amz_date: &str,
    date_stamp: &str,
    payload_hash: &str,
) -> String {
    let canonical_headers = format!("host:{host}\nx-amz-date:{amz_date}\n");
    let signed_headers = "host;x-amz-date";
    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );
    let hashed_canonical_request = hex::encode(Sha256::digest(canonical_request.as_bytes()));

    let credential_scope = format!("{date_stamp}/{region}/{service}/aws4_request");
    let string_to_sign =
        format!("AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{hashed_canonical_request}");

    let k_date = hmac_sha256(
        format!("AWS4{secret_key}").as_bytes(),
        date_stamp.as_bytes(),
    );
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

    format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}"
    )
}

// ---------------------------------------------------------------------------
// Digest (RFC 2617) — two-round-trip challenge-response
// ---------------------------------------------------------------------------

/// Parsed from a `WWW-Authenticate: Digest ...` response header.
pub struct DigestChallenge {
    pub realm: String,
    pub nonce: String,
    /// The first offered `qop` value (almost always `"auth"`) — `None` means
    /// the server didn't send one, so the (older, pre-RFC-2617-`qop`)
    /// simpler `response` formula applies.
    pub qop: Option<String>,
    pub opaque: Option<String>,
    pub algorithm: String,
}

/// `None` if `www_authenticate` isn't a `Digest` challenge, or is missing
/// the two fields (`realm`, `nonce`) a response can't be computed without.
pub fn parse_digest_challenge(www_authenticate: &str) -> Option<DigestChallenge> {
    let rest = www_authenticate.trim();
    let rest = rest
        .strip_prefix("Digest")
        .or_else(|| rest.strip_prefix("digest"))?
        .trim_start();

    let mut realm = None;
    let mut nonce = None;
    let mut qop = None;
    let mut opaque = None;
    let mut algorithm = "MD5".to_string();

    for pair in split_digest_params(rest) {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        match key.trim() {
            "realm" => realm = Some(value.to_string()),
            "nonce" => nonce = Some(value.to_string()),
            // Prefer the first offered qop (in practice servers offer just
            // "auth", or "auth,auth-int" with "auth" first).
            "qop" => qop = value.split(',').next().map(|q| q.trim().to_string()),
            "opaque" => opaque = Some(value.to_string()),
            "algorithm" => algorithm = value.to_string(),
            _ => {}
        }
    }

    Some(DigestChallenge {
        realm: realm?,
        nonce: nonce?,
        qop,
        opaque,
        algorithm,
    })
}

/// Splits `k1=v1, k2="v2,with,commas", k3=v3` on top-level commas, treating
/// commas inside `"..."` as part of the value rather than a separator.
fn split_digest_params(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                current.push(c);
            }
            ',' if !in_quotes => {
                parts.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

/// Builds the `Authorization: Digest ...` header for a resend, per RFC 2617.
/// `cnonce`/`nc` are supplied by the caller (`http_client.rs` generates a
/// fresh `cnonce` and always uses `nc = "00000001"`, since this app only
/// ever makes a single retry attempt per challenge) rather than generated
/// in here, so this stays a pure, directly-testable function.
#[allow(clippy::too_many_arguments)]
pub fn build_digest_header(
    challenge: &DigestChallenge,
    username: &str,
    password: &str,
    method: &str,
    uri: &str,
    cnonce: &str,
    nc: &str,
) -> String {
    let ha1 = hex::encode(md5::Md5::digest(format!(
        "{username}:{}:{password}",
        challenge.realm
    )));
    let ha2 = hex::encode(md5::Md5::digest(format!("{method}:{uri}")));

    let response = match &challenge.qop {
        Some(qop) => hex::encode(md5::Md5::digest(format!(
            "{ha1}:{}:{nc}:{cnonce}:{qop}:{ha2}",
            challenge.nonce
        ))),
        None => hex::encode(md5::Md5::digest(format!("{ha1}:{}:{ha2}", challenge.nonce))),
    };

    let mut parts = vec![
        format!(r#"username="{username}""#),
        format!(r#"realm="{}""#, challenge.realm),
        format!(r#"nonce="{}""#, challenge.nonce),
        format!(r#"uri="{uri}""#),
        format!(r#"response="{response}""#),
    ];
    if let Some(opaque) = &challenge.opaque {
        parts.push(format!(r#"opaque="{opaque}""#));
    }
    if let Some(qop) = &challenge.qop {
        parts.push(format!("qop={qop}"));
        parts.push(format!("nc={nc}"));
        parts.push(format!(r#"cnonce="{cnonce}""#));
    }
    if challenge.algorithm != "MD5" {
        parts.push(format!("algorithm={}", challenge.algorithm));
    }
    format!("Digest {}", parts.join(", "))
}

// ---------------------------------------------------------------------------
// OAuth 2.0 Client Credentials grant
// ---------------------------------------------------------------------------

/// POSTs `grant_type=client_credentials` (form-encoded) to `token_url` and
/// pulls `access_token` out of the JSON response. Authorization Code (needs
/// a system browser + local redirect listener) is a deferred follow-up.
pub async fn fetch_oauth2_client_credentials_token(
    client: &reqwest::Client,
    token_url: &str,
    client_id: &str,
    client_secret: &str,
    scope: Option<&str>,
) -> Result<String, String> {
    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "client_credentials"),
        ("client_id", client_id),
        ("client_secret", client_secret),
    ];
    if let Some(scope) = scope {
        form.push(("scope", scope));
    }

    let response = client
        .post(token_url)
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("token request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("token endpoint returned {status}: {body}"));
    }

    let json: serde_json::Value = response
        .text()
        .await
        .map_err(|e| format!("failed to read token response: {e}"))
        .and_then(|text| {
            serde_json::from_str(&text)
                .map_err(|e| format!("token response wasn't valid JSON: {e}"))
        })?;
    json.get("access_token")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "token response had no \"access_token\" field".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::KeyValue;

    fn config(kind: AuthKind, pairs: &[(&str, &str)]) -> AuthConfig {
        AuthConfig {
            kind,
            params: pairs
                .iter()
                .map(|(k, v)| KeyValue {
                    key: k.to_string(),
                    value: v.to_string(),
                    enabled: true,
                })
                .collect(),
        }
    }

    #[test]
    fn basic_encodes_username_password_as_base64() {
        let auth = config(
            AuthKind::Basic,
            &[("username", "Aladdin"), ("password", "open sesame")],
        );
        let PreparedAuth::Headers(headers) =
            prepare_auth(&auth, &Method::Get, "http://x", &[], &[], false)
        else {
            panic!("expected headers");
        };
        // A well-known example from RFC 7617.
        assert_eq!(
            headers,
            vec![(
                "Authorization".to_string(),
                "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==".to_string()
            )]
        );
    }

    #[test]
    fn bearer_sets_authorization_header() {
        let auth = config(AuthKind::Bearer, &[("token", "abc123")]);
        let PreparedAuth::Headers(headers) =
            prepare_auth(&auth, &Method::Get, "http://x", &[], &[], false)
        else {
            panic!("expected headers");
        };
        assert_eq!(
            headers,
            vec![("Authorization".to_string(), "Bearer abc123".to_string())]
        );
    }

    #[test]
    fn apikey_dispatches_to_header_or_query_by_location() {
        let header_auth = config(
            AuthKind::ApiKey,
            &[("key", "X-Api-Key"), ("value", "secret"), ("in", "header")],
        );
        assert!(matches!(
            prepare_auth(&header_auth, &Method::Get, "http://x", &[], &[], false),
            PreparedAuth::Headers(_)
        ));

        let query_auth = config(
            AuthKind::ApiKey,
            &[("key", "api_key"), ("value", "secret"), ("in", "query")],
        );
        let PreparedAuth::Query(q) =
            prepare_auth(&query_auth, &Method::Get, "http://x", &[], &[], false)
        else {
            panic!("expected query");
        };
        assert_eq!(q, vec![("api_key".to_string(), "secret".to_string())]);
    }

    #[test]
    fn inherit_and_none_apply_nothing() {
        let inherit = AuthConfig::default();
        assert!(matches!(
            prepare_auth(&inherit, &Method::Get, "http://x", &[], &[], false),
            PreparedAuth::None
        ));
        let none = config(AuthKind::None, &[]);
        assert!(matches!(
            prepare_auth(&none, &Method::Get, "http://x", &[], &[], false),
            PreparedAuth::None
        ));
    }

    #[test]
    fn oauth2_uses_pasted_access_token_directly_when_present() {
        let auth = config(AuthKind::OAuth2, &[("accessToken", "tok123")]);
        let PreparedAuth::Headers(headers) =
            prepare_auth(&auth, &Method::Get, "http://x", &[], &[], false)
        else {
            panic!("expected headers");
        };
        assert_eq!(
            headers,
            vec![("Authorization".to_string(), "Bearer tok123".to_string())]
        );
    }

    #[test]
    fn oauth2_falls_back_to_client_credentials_when_no_token_pasted() {
        let auth = config(
            AuthKind::OAuth2,
            &[
                ("tokenUrl", "https://auth.example.com/token"),
                ("clientId", "id"),
                ("clientSecret", "secret"),
            ],
        );
        let prepared = prepare_auth(&auth, &Method::Get, "http://x", &[], &[], false);
        assert!(matches!(
            prepared,
            PreparedAuth::OAuth2ClientCredentials { .. }
        ));
    }

    #[test]
    fn digest_defers_to_a_challenge_response() {
        let auth = config(AuthKind::Digest, &[("username", "u"), ("password", "p")]);
        assert!(matches!(
            prepare_auth(&auth, &Method::Get, "http://x", &[], &[], false),
            PreparedAuth::DigestChallenge { .. }
        ));
    }

    #[test]
    fn oauth1_header_is_well_formed_and_signature_changes_with_secret() {
        let auth_a = config(
            AuthKind::OAuth1,
            &[("consumerKey", "ck"), ("consumerSecret", "cs1")],
        );
        let auth_b = config(
            AuthKind::OAuth1,
            &[("consumerKey", "ck"), ("consumerSecret", "cs2")],
        );
        let PreparedAuth::Headers(h_a) = prepare_auth(
            &auth_a,
            &Method::Get,
            "http://example.com/resource",
            &[],
            &[],
            false,
        ) else {
            panic!("expected headers");
        };
        let PreparedAuth::Headers(h_b) = prepare_auth(
            &auth_b,
            &Method::Get,
            "http://example.com/resource",
            &[],
            &[],
            false,
        ) else {
            panic!("expected headers");
        };
        assert!(h_a[0].1.starts_with("OAuth oauth_consumer_key=\"ck\""));
        assert!(h_a[0].1.contains("oauth_signature_method=\"HMAC-SHA1\""));
        assert!(h_a[0].1.contains("oauth_signature="));
        assert_ne!(
            h_a[0].1, h_b[0].1,
            "different consumer secrets must produce different signatures"
        );
    }

    #[test]
    fn percent_encode_only_leaves_unreserved_characters_untouched() {
        assert_eq!(percent_encode("abcXYZ019-._~"), "abcXYZ019-._~");
        assert_eq!(percent_encode("a b/c"), "a%20b%2Fc");
    }

    /// AWS's own published "get-vanilla" test vector (Signature Version 4
    /// Test Suite) — a fixed-input, byte-exact correctness check, not just a
    /// structural one.
    #[test]
    fn aws_sigv4_matches_the_official_get_vanilla_test_vector() {
        let authorization = aws_sigv4_authorization_header(
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            "service",
            "GET",
            "/",
            "",
            "example.amazonaws.com",
            "20150830T123600Z",
            "20150830",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        );
        assert_eq!(
            authorization,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    #[test]
    fn parse_digest_challenge_reads_a_real_world_header() {
        let header = r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#;
        let challenge = parse_digest_challenge(header).expect("should parse");
        assert_eq!(challenge.realm, "testrealm@host.com");
        assert_eq!(challenge.nonce, "dcd98b7102dd2f0e8b11d0f600bfb0c093");
        assert_eq!(challenge.qop.as_deref(), Some("auth"));
        assert_eq!(
            challenge.opaque.as_deref(),
            Some("5ccc069c403ebaf9f0171e9517f40e41")
        );
        assert_eq!(challenge.algorithm, "MD5");
    }

    #[test]
    fn parse_digest_challenge_rejects_non_digest_schemes() {
        assert!(parse_digest_challenge(r#"Basic realm="x""#).is_none());
    }

    /// RFC 2617 §3.5's own worked example — a byte-exact correctness check
    /// against the RFC's published `response` value, not just structural.
    #[test]
    fn build_digest_header_matches_rfc_2617_worked_example() {
        let challenge = DigestChallenge {
            realm: "testrealm@host.com".to_string(),
            nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093".to_string(),
            qop: Some("auth".to_string()),
            opaque: Some("5ccc069c403ebaf9f0171e9517f40e41".to_string()),
            algorithm: "MD5".to_string(),
        };
        let header = build_digest_header(
            &challenge,
            "Mufasa",
            "Circle Of Life",
            "GET",
            "/dir/index.html",
            "0a4f113b",
            "00000001",
        );
        assert!(header.contains(r#"response="6629fae49393a05397450978507c4ef1""#));
        assert!(header.contains(r#"username="Mufasa""#));
        assert!(header.contains(r#"cnonce="0a4f113b""#));
        assert!(header.contains("qop=auth"));
        assert!(header.contains("nc=00000001"));
    }

    #[test]
    fn build_digest_header_without_qop_uses_the_simpler_formula() {
        let challenge = DigestChallenge {
            realm: "r".to_string(),
            nonce: "n".to_string(),
            qop: None,
            opaque: None,
            algorithm: "MD5".to_string(),
        };
        let header = build_digest_header(&challenge, "u", "p", "GET", "/x", "cn", "00000001");
        assert!(!header.contains("qop="));
        assert!(!header.contains("cnonce="));
        assert!(header.contains("response="));
    }
}
