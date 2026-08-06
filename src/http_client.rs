use crate::auth::{self, PreparedAuth};
use crate::model::{AuthConfig, BodyMode, FormFieldType, KeyValue, RequestItem, Settings};
use reqwest_cookie_store::CookieStoreMutex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

/// Builds the shared `reqwest::Client` from `Settings` — all of a
/// `ClientBuilder`'s config (cookies/proxy/TLS) is baked in at `.build()`
/// time, so this is called once at startup and again whenever Settings is
/// saved (replacing `App.client`). `cookie_jar` is the same `Arc` across
/// rebuilds, so existing cookies survive a Settings change.
///
/// Never fails outright — a malformed proxy URL or unreadable cert file is
/// just skipped (the caller surfaces that as a warning), rather than
/// leaving the user with no client at all.
pub fn build_client(settings: &Settings, cookie_jar: Arc<CookieStoreMutex>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder().cookie_provider(cookie_jar);

    if settings.proxy.enabled {
        if !settings.proxy.http_proxy.is_empty()
            && let Ok(proxy) = reqwest::Proxy::http(&settings.proxy.http_proxy)
        {
            builder = builder.proxy(apply_no_proxy(proxy, &settings.proxy.no_proxy));
        }
        if !settings.proxy.https_proxy.is_empty()
            && let Ok(proxy) = reqwest::Proxy::https(&settings.proxy.https_proxy)
        {
            builder = builder.proxy(apply_no_proxy(proxy, &settings.proxy.no_proxy));
        }
    } else {
        // Explicitly disables even system/env-var proxy auto-detection,
        // not just "don't add one of our own".
        builder = builder.no_proxy();
    }

    if settings.tls.accept_invalid_certs {
        builder = builder.tls_danger_accept_invalid_certs(true);
    }
    if let Some(path) = settings
        .tls
        .custom_ca_cert_path
        .as_ref()
        .filter(|p| !p.is_empty())
        && let Ok(bytes) = std::fs::read(path)
        && let Ok(cert) = reqwest::Certificate::from_pem(&bytes)
    {
        builder = builder.tls_certs_merge(Some(cert));
    }
    if let Some(path) = settings
        .tls
        .client_cert_path
        .as_ref()
        .filter(|p| !p.is_empty())
        && let Ok(bytes) = std::fs::read(path)
        && let Ok(identity) = reqwest::Identity::from_pem(&bytes)
    {
        builder = builder.identity(identity);
    }

    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

fn apply_no_proxy(proxy: reqwest::Proxy, no_proxy_list: &str) -> reqwest::Proxy {
    if no_proxy_list.trim().is_empty() {
        return proxy;
    }
    proxy.no_proxy(reqwest::NoProxy::from_string(no_proxy_list))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub duration_ms: u128,
    pub size_bytes: usize,
    /// The original response bytes, before the lossy-UTF8 conversion that
    /// produces `body`. Transient (`#[serde(skip)]`, defaults to empty on
    /// deserialize) — saving every response's raw bytes into every
    /// `HistoryEntry`/`SavedExample` JSON file forever would be a real
    /// storage-bloat regression for binary responses, and "Save Response to
    /// File" only ever needs the *live* response, never a persisted-forever
    /// copy. `body`/`size_bytes` stay byte-identical to before this field
    /// existed for every existing consumer.
    #[serde(skip)]
    pub raw_bytes: Vec<u8>,
}

/// A snapshot of exactly what went out on the wire: method, fully-resolved URL
/// (including query string), every header actually sent, and the raw body —
/// all *after* `{{variable}}` substitution, so it reflects reality rather than
/// what the editor fields say.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SentRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

pub enum RequestOutcome {
    Success {
        request: SentRequest,
        response: HttpResponse,
    },
    Error {
        request: Option<SentRequest>,
        message: String,
        /// How long the request was in flight before failing. `None` when
        /// the failure happened before anything was sent on the wire (e.g.
        /// an empty URL, a missing file, or a request that failed to build).
        duration_ms: Option<u128>,
    },
}

fn snapshot_of(built: &reqwest::Request) -> SentRequest {
    let method = built.method().to_string();
    let url = built.url().to_string();
    let headers = built
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("<binary>").to_string()))
        .collect();
    let body = built
        .body()
        .and_then(|b| b.as_bytes())
        .map(|bytes| String::from_utf8_lossy(bytes).to_string());
    SentRequest {
        method,
        url,
        headers,
        body,
    }
}

/// `variable_scopes` is precedence-ordered — e.g. `[environment, collection,
/// globals]` — the first scope defining a given `{{key}}` wins. See
/// `model::resolve_variables`. `auth` is the already-*resolved* effective
/// config (Inherit walked up to whatever it resolves to) — this module has
/// no `Collection`/`Folder` to do that resolution itself, so the caller
/// does it first, same division of labor as `variable_scopes`.
pub async fn send_request(
    client: reqwest::Client,
    item: RequestItem,
    variable_scopes: Vec<Vec<KeyValue>>,
    auth: AuthConfig,
) -> RequestOutcome {
    let scope_refs: Vec<&[KeyValue]> = variable_scopes.iter().map(|v| v.as_slice()).collect();
    let resolve = |s: &str| -> String { crate::model::resolve_variables(s, &scope_refs) };

    let path_param_values: Vec<(String, String)> = item
        .path_params
        .iter()
        .map(|p| (p.key.clone(), resolve(&p.value)))
        .collect();
    let url = crate::model::substitute_path_params(&resolve(&item.url), &path_param_values);
    if url.trim().is_empty() {
        return RequestOutcome::Error {
            request: None,
            message: "URL is empty".to_string(),
            duration_ms: None,
        };
    }

    let mut req = client.request(item.method.to_reqwest(), &url);
    // A request-level override — `None` means "whatever the `Client`'s own
    // default already is" (no explicit timeout at all, today's behavior).
    if let Some(ms) = item.timeout_ms {
        req = req.timeout(std::time::Duration::from_millis(ms));
    }

    // Query params
    let query: Vec<(String, String)> = item
        .params
        .iter()
        .filter(|p| p.enabled && !p.key.is_empty())
        .map(|p| (resolve(&p.key), resolve(&p.value)))
        .collect();
    if !query.is_empty() {
        req = req.query(&query);
    }

    // Headers
    for h in item
        .headers
        .iter()
        .filter(|h| h.enabled && !h.key.is_empty())
    {
        req = req.header(resolve(&h.key), resolve(&h.value));
    }

    // Body
    match item.body.mode {
        BodyMode::None => {}
        BodyMode::Raw => {
            req = req.body(resolve(&item.body.raw));
        }
        BodyMode::Json => {
            let resolved = resolve(&item.body.raw);
            req = req
                .header("Content-Type", "application/json")
                .body(resolved);
        }
        BodyMode::Form => {
            let form: Vec<(String, String)> = item
                .body
                .form
                .iter()
                .filter(|f| f.enabled && !f.key.is_empty())
                .map(|f| (resolve(&f.key), resolve(&f.value)))
                .collect();
            req = req.form(&form);
        }
        BodyMode::Multipart => {
            let mut form = reqwest::multipart::Form::new();
            for field in item
                .body
                .multipart
                .iter()
                .filter(|f| f.enabled && !f.key.is_empty())
            {
                let key = resolve(&field.key);
                match field.field_type {
                    FormFieldType::Text => {
                        form = form.text(key, resolve(&field.value));
                    }
                    FormFieldType::File => {
                        let Some(path) = field.file_path.as_ref().filter(|p| !p.is_empty()) else {
                            continue;
                        };
                        match tokio::fs::read(path).await {
                            Ok(bytes) => {
                                let file_name = std::path::Path::new(path)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_else(|| "file".to_string());
                                let part =
                                    reqwest::multipart::Part::bytes(bytes).file_name(file_name);
                                form = form.part(key, part);
                            }
                            Err(e) => {
                                return RequestOutcome::Error {
                                    request: None,
                                    message: format!("Failed to read file {path}: {e}"),
                                    duration_ms: None,
                                };
                            }
                        }
                    }
                }
            }
            req = req.multipart(form);
        }
        BodyMode::Binary => {
            let Some(path) = item
                .body
                .binary_file_path
                .as_ref()
                .filter(|p| !p.is_empty())
            else {
                return RequestOutcome::Error {
                    request: None,
                    message: "No file selected for the binary body".to_string(),
                    duration_ms: None,
                };
            };
            match tokio::fs::read(path).await {
                Ok(bytes) => {
                    req = req.body(bytes);
                }
                Err(e) => {
                    return RequestOutcome::Error {
                        request: None,
                        message: format!("Failed to read file {path}: {e}"),
                        duration_ms: None,
                    };
                }
            }
        }
    }

    // Auth is applied here — after the body, not alongside the header loop
    // above — because `AwsSigV4` needs the final body bytes for its payload
    // hash; every other kind is indifferent to being applied at this point.
    let (payload_for_signing, unsigned_payload): (Vec<u8>, bool) = match item.body.mode {
        BodyMode::None => (Vec::new(), false),
        BodyMode::Raw | BodyMode::Json => (resolve(&item.body.raw).into_bytes(), false),
        // Reconstructing the exact on-wire bytes of a multipart/binary body
        // ahead of `reqwest` building the request isn't practical here;
        // `UNSIGNED-PAYLOAD` is a real, spec-valid SigV4 convention for
        // exactly this case (streaming/unknown-length payloads).
        BodyMode::Form | BodyMode::Multipart | BodyMode::Binary => (Vec::new(), true),
    };
    let prepared_auth = auth::prepare_auth(
        &auth,
        &item.method,
        &url,
        &query,
        &payload_for_signing,
        unsigned_payload,
    );

    let mut digest_creds: Option<(String, String)> = None;
    match &prepared_auth {
        PreparedAuth::Headers(headers) => {
            for (k, v) in headers {
                req = req.header(k, v);
            }
        }
        PreparedAuth::Query(pairs) => {
            req = req.query(pairs);
        }
        PreparedAuth::OAuth2ClientCredentials {
            token_url,
            client_id,
            client_secret,
            scope,
        } => {
            match auth::fetch_oauth2_client_credentials_token(
                &client,
                token_url,
                client_id,
                client_secret,
                scope.as_deref(),
            )
            .await
            {
                Ok(token) => {
                    req = req.header("Authorization", format!("Bearer {token}"));
                }
                Err(e) => {
                    return RequestOutcome::Error {
                        request: None,
                        message: format!("OAuth2 token request failed: {e}"),
                        duration_ms: None,
                    };
                }
            }
        }
        // No header yet — applied below, only if the server actually
        // challenges for it.
        PreparedAuth::DigestChallenge { username, password } => {
            digest_creds = Some((username.clone(), password.clone()));
        }
        PreparedAuth::None => {}
    }

    let built = match req.build() {
        Ok(built) => built,
        Err(e) => {
            return RequestOutcome::Error {
                request: None,
                message: format!("Failed to build request: {e}"),
                duration_ms: None,
            };
        }
    };
    let snapshot = snapshot_of(&built);

    let start = Instant::now();
    let result = client.execute(built).await;

    if let (Some((username, password)), Ok(resp)) = (&digest_creds, &result)
        && resp.status().as_u16() == 401
    {
        let challenge = resp
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .and_then(auth::parse_digest_challenge);
        if let Some(challenge) = challenge {
            let uri = digest_uri(&snapshot.url);
            let cnonce = uuid::Uuid::new_v4().simple().to_string();
            let digest_header = auth::build_digest_header(
                &challenge,
                username,
                password,
                &snapshot.method,
                &uri,
                &cnonce,
                "00000001",
            );
            return match resend_with_digest(&client, &snapshot, digest_header).await {
                Ok(resp2) => {
                    outcome_from_response(resp2, snapshot, start.elapsed().as_millis()).await
                }
                Err(e) => RequestOutcome::Error {
                    request: Some(snapshot),
                    message: format!("Digest retry failed: {e}"),
                    duration_ms: Some(start.elapsed().as_millis()),
                },
            };
        }
    }

    match result {
        Ok(resp) => outcome_from_response(resp, snapshot, start.elapsed().as_millis()).await,
        Err(e) => RequestOutcome::Error {
            request: Some(snapshot),
            message: format!("Request failed: {e}"),
            duration_ms: Some(start.elapsed().as_millis()),
        },
    }
}

/// Shared by the normal path and the Digest-retry path so both read the
/// response (status/headers/body) the same way.
async fn outcome_from_response(
    resp: reqwest::Response,
    snapshot: SentRequest,
    duration_ms: u128,
) -> RequestOutcome {
    let status = resp.status().as_u16();
    let status_text = resp.status().canonical_reason().unwrap_or("").to_string();
    let headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    // Captured as raw bytes (not `resp.text()`) so "Save Response to File"
    // can write a byte-exact copy — `body` is still produced via
    // `from_utf8_lossy`, identically to before, so every existing consumer
    // (display, scripting, codegen) sees the exact same `String` it always
    // has for UTF8 content.
    match resp.bytes().await {
        Ok(raw_bytes) => {
            let body = String::from_utf8_lossy(&raw_bytes).to_string();
            let size_bytes = raw_bytes.len();
            RequestOutcome::Success {
                request: snapshot,
                response: HttpResponse {
                    status,
                    status_text,
                    headers,
                    body,
                    duration_ms,
                    size_bytes,
                    raw_bytes: raw_bytes.to_vec(),
                },
            }
        }
        Err(e) => RequestOutcome::Error {
            request: Some(snapshot),
            message: format!("Failed to read body: {e}"),
            duration_ms: Some(duration_ms),
        },
    }
}

/// The request-URI (path + query, no scheme/host) RFC 2617 signs — parsed
/// from the snapshot's fully-resolved URL rather than the pre-`req.query()`
/// one, so it reflects exactly what went out on the wire.
fn digest_uri(url: &str) -> String {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return url.to_string();
    };
    match parsed.query() {
        Some(q) => format!("{}?{q}", parsed.path()),
        None => parsed.path().to_string(),
    }
}

/// Resends the exact method/url/headers/body captured in `snapshot`, with
/// `snapshot`'s own `Authorization` (if any) replaced by `digest_header`.
/// Replays the already-captured snapshot rather than rebuilding the request
/// from scratch, so a second async multipart/file read isn't needed.
///
/// Known limitation: `snapshot.body` is a lossy-UTF8 `String` (`snapshot_of`
/// converts raw bytes via `from_utf8_lossy`), so this retry is byte-exact
/// for `Raw`/`Json`/`Form` bodies but not guaranteed for `Multipart`/
/// `Binary` ones — a rare combination with Digest auth in practice.
async fn resend_with_digest(
    client: &reqwest::Client,
    snapshot: &SentRequest,
    digest_header: String,
) -> reqwest::Result<reqwest::Response> {
    let method =
        reqwest::Method::from_bytes(snapshot.method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let mut builder = client.request(method, &snapshot.url);
    for (k, v) in &snapshot.headers {
        if k.eq_ignore_ascii_case("authorization") {
            continue; // replaced by `digest_header` below
        }
        builder = builder.header(k, v);
    }
    builder = builder.header("Authorization", digest_header);
    if let Some(body) = &snapshot.body {
        builder = builder.body(body.clone());
    }
    builder.send().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProxyConfig, TlsConfig};

    fn jar() -> Arc<CookieStoreMutex> {
        Arc::new(CookieStoreMutex::default())
    }

    /// `build_client` should never panic and should always produce *a*
    /// client, across the input combinations users can actually configure
    /// through the Settings panel — it can't assert deep proxy/TLS
    /// *behavior* without a live target (see the manual-verification note
    /// in the roadmap), just that nothing here crashes or silently returns
    /// no client at all.
    #[test]
    fn build_client_never_panics_across_settings_combinations() {
        let combinations = [
            Settings::default(),
            Settings {
                proxy: ProxyConfig {
                    enabled: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            Settings {
                proxy: ProxyConfig {
                    enabled: true,
                    http_proxy: "http://127.0.0.1:8080".to_string(),
                    https_proxy: "http://127.0.0.1:8080".to_string(),
                    no_proxy: "localhost,example.com".to_string(),
                },
                ..Default::default()
            },
            // A malformed proxy URL is skipped, not a hard error.
            Settings {
                proxy: ProxyConfig {
                    enabled: true,
                    http_proxy: "not a url".to_string(),
                    ..Default::default()
                },
                ..Default::default()
            },
            Settings {
                tls: TlsConfig {
                    accept_invalid_certs: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            // A nonexistent cert path is skipped, not a hard error.
            Settings {
                tls: TlsConfig {
                    custom_ca_cert_path: Some("/nonexistent/ca.pem".to_string()),
                    client_cert_path: Some("/nonexistent/client.pem".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
        ];

        for settings in combinations {
            let _client = build_client(&settings, jar());
        }
    }

    /// Phase 13d: `body`/`size_bytes` must stay exactly what they always
    /// were (a lossy-UTF8 `String` and its byte length) now that they're
    /// derived from `resp.bytes()` instead of `resp.text()` — and the new
    /// `raw_bytes` field should be a byte-exact match of that same body for
    /// ordinary UTF8 content, which "Save Response to File" depends on.
    /// Needs real network access — same manual-verification convention as
    /// every prior phase's real-server tests (Phase 5's `pm.sendRequest`,
    /// Phase 9's Runner, Phase 10's curl execution).
    #[test]
    #[ignore]
    fn send_request_captures_raw_bytes_matching_the_lossy_body_for_utf8_content() {
        let client = reqwest::Client::new();
        let mut item = RequestItem::new("example");
        item.url = "https://example.com/".to_string();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let outcome = rt.block_on(send_request(
            client,
            item,
            Vec::new(),
            AuthConfig::default(),
        ));
        match outcome {
            RequestOutcome::Success { response, .. } => {
                assert_eq!(response.size_bytes, response.raw_bytes.len());
                assert_eq!(response.body.as_bytes(), response.raw_bytes.as_slice());
                assert!(!response.raw_bytes.is_empty());
            }
            RequestOutcome::Error { message, .. } => panic!("request failed: {message}"),
        }
    }
}
