//! Postman's "Code" snippet panel — generates curl/JavaScript/Python/Rust
//! source that reproduces the current request. Deliberately dependency-free
//! string templating (`format!`/`String` building), not a template engine.
//!
//! Two design decisions worth calling out (see the Phase 10 plan for the
//! full reasoning):
//! - Snippets show the request's raw, unresolved text (`{{variable}}`
//!   placeholders intact) — matches Postman's own behavior, and is simply
//!   what's already on `RequestItem` with no extra plumbing.
//! - Only *static* auth (Basic/Bearer/ApiKey/OAuth2-with-a-token) is
//!   embedded as a real, usable value. Signature-based auth (OAuth1,
//!   AwsSigV4) signs over the *final resolved* URL/body — computing one now
//!   would embed a signature that's valid for the wrong (unresolved) text,
//!   silently wrong once real values are substituted. Live-round-trip auth
//!   (Digest, OAuth2 needing a token fetch) can't be precomputed at all.
//!   Both get an honest comment instead of a fabricated value — except
//!   where the target language has genuine native support (curl's
//!   `--digest`, Python's `requests.auth.HTTPDigestAuth`), which the
//!   generator uses for real. No crypto is duplicated from `src/auth.rs`;
//!   this module never calls `prepare_auth`/`oauth1_header`/
//!   `aws_sigv4_headers` at all.

use crate::model::{AuthConfig, AuthKind, BodyMode, FormFieldType, KeyValue, RequestItem};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CodeGenTarget {
    Curl,
    JavaScriptFetch,
    Python,
    Rust,
}

impl CodeGenTarget {
    pub const ALL: [CodeGenTarget; 4] = [
        CodeGenTarget::Curl,
        CodeGenTarget::JavaScriptFetch,
        CodeGenTarget::Python,
        CodeGenTarget::Rust,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            CodeGenTarget::Curl => "curl",
            CodeGenTarget::JavaScriptFetch => "JavaScript (fetch)",
            CodeGenTarget::Python => "Python (requests)",
            CodeGenTarget::Rust => "Rust (reqwest)",
        }
    }
}

pub fn generate_snippet(request: &RequestItem, auth: &AuthConfig, target: CodeGenTarget) -> String {
    match target {
        CodeGenTarget::Curl => curl_snippet(request, auth),
        CodeGenTarget::JavaScriptFetch => js_snippet(request, auth),
        CodeGenTarget::Python => python_snippet(request, auth),
        CodeGenTarget::Rust => rust_snippet(request, auth),
    }
}

/// What a request's auth contributes to a generated snippet — resolved
/// once per target function rather than duplicated per body-mode arm.
enum AuthContribution {
    /// A plain header, the fallback shape for anything without a nicer
    /// target-native idiom.
    Header(String, String),
    /// An API-key-in-query contribution — just another query param, no
    /// target has a query-building auth helper.
    Query(String, String),
    /// Username/password for Basic — its own variant (not folded into
    /// `Header`) so each target can use its nicer native idiom (curl `-u`,
    /// Python `HTTPBasicAuth`, Rust `.basic_auth()`) instead of hand-building
    /// a base64 header.
    BasicCreds(String, String),
    /// Same idea for Digest, since curl (`--digest -u`) and Python
    /// (`HTTPDigestAuth`) have genuine native support for it.
    DigestCreds(String, String),
    /// A bearer token — its own variant (not folded into `Header`) so Rust
    /// can use `.bearer_auth()` instead of a hand-built header. Produced by
    /// both `AuthKind::Bearer` and `AuthKind::OAuth2` (once it already has
    /// an access token) — from a generated snippet's point of view they're
    /// the same thing.
    BearerToken(String),
    /// A signature/live-round-trip auth kind that can't be safely
    /// precomputed — explained in a comment instead of faked.
    Note(&'static str),
    None,
}

fn param(auth: &AuthConfig, key: &str) -> String {
    auth.params
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| kv.value.clone())
        .unwrap_or_default()
}

fn auth_contribution(auth: &AuthConfig) -> AuthContribution {
    match auth.kind {
        AuthKind::Inherit | AuthKind::None => AuthContribution::None,
        AuthKind::Basic => {
            AuthContribution::BasicCreds(param(auth, "username"), param(auth, "password"))
        }
        AuthKind::Bearer => AuthContribution::BearerToken(param(auth, "token")),
        AuthKind::ApiKey => {
            let key = param(auth, "key");
            let value = param(auth, "value");
            if param(auth, "in") == "query" {
                AuthContribution::Query(key, value)
            } else {
                AuthContribution::Header(key, value)
            }
        }
        AuthKind::Digest => {
            AuthContribution::DigestCreds(param(auth, "username"), param(auth, "password"))
        }
        AuthKind::OAuth1 => AuthContribution::Note(
            "OAuth 1.0a signs over the final, resolved request — a signature computed here (before variables are substituted) would be invalid. Sign this request from RustGirl directly, or with your own OAuth1 library once variables are resolved.",
        ),
        AuthKind::OAuth2 => {
            let token = param(auth, "accessToken");
            if token.is_empty() {
                AuthContribution::Note(
                    "No OAuth2 access token is set — configure a Client Credentials token URL in the Auth tab, or paste a token there directly.",
                )
            } else {
                AuthContribution::BearerToken(token)
            }
        }
        AuthKind::AwsSigV4 => AuthContribution::Note(
            "AWS Signature v4 signs over the final, resolved request and the current timestamp — a signature computed here would be stale/invalid almost immediately. Send this request from RustGirl directly, or sign it with an AWS SDK once variables are resolved.",
        ),
    }
}

fn enabled_pairs(items: &[KeyValue]) -> Vec<(&str, &str)> {
    items
        .iter()
        .filter(|kv| kv.enabled && !kv.key.is_empty())
        .map(|kv| (kv.key.as_str(), kv.value.as_str()))
        .collect()
}

// ---------- curl ----------

fn curl_snippet(request: &RequestItem, auth: &AuthConfig) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut flags: Vec<String> = vec![format!("-X {}", request.method.as_str())];

    let mut query = enabled_pairs(&request.params);
    let contribution = auth_contribution(auth);

    for (k, v) in enabled_pairs(&request.headers) {
        flags.push(format!("-H {}", shell_words::quote(&format!("{k}: {v}"))));
    }

    match &contribution {
        AuthContribution::BasicCreds(user, pass) => {
            flags.push(format!(
                "-u {}",
                shell_words::quote(&format!("{user}:{pass}"))
            ));
        }
        AuthContribution::DigestCreds(user, pass) => {
            flags.push("--digest".to_string());
            flags.push(format!(
                "-u {}",
                shell_words::quote(&format!("{user}:{pass}"))
            ));
        }
        AuthContribution::Header(k, v) => {
            flags.push(format!("-H {}", shell_words::quote(&format!("{k}: {v}"))));
        }
        AuthContribution::BearerToken(token) => {
            flags.push(format!(
                "-H {}",
                shell_words::quote(&format!("Authorization: Bearer {token}"))
            ));
        }
        AuthContribution::Query(k, v) => query.push((k.as_str(), v.as_str())),
        AuthContribution::Note(note) => lines.push(format!("# {note}")),
        AuthContribution::None => {}
    }
    let url = url_with_query(&request.url, &query);

    match request.body.mode {
        BodyMode::None => {}
        BodyMode::Raw => {
            flags.push(format!("-d {}", shell_words::quote(&request.body.raw)));
        }
        BodyMode::Json => {
            flags.push("-H 'Content-Type: application/json'".to_string());
            flags.push(format!("-d {}", shell_words::quote(&request.body.raw)));
        }
        BodyMode::Form => {
            for (k, v) in enabled_pairs(&request.body.form) {
                flags.push(format!(
                    "--data-urlencode {}",
                    shell_words::quote(&format!("{k}={v}"))
                ));
            }
        }
        BodyMode::Multipart => {
            for field in request
                .body
                .multipart
                .iter()
                .filter(|f| f.enabled && !f.key.is_empty())
            {
                let value = match field.field_type {
                    FormFieldType::Text => field.value.clone(),
                    FormFieldType::File => format!("@{}", field.file_path.as_deref().unwrap_or("")),
                };
                flags.push(format!(
                    "-F {}",
                    shell_words::quote(&format!("{}={value}", field.key))
                ));
            }
        }
        BodyMode::Binary => {
            if let Some(path) = request
                .body
                .binary_file_path
                .as_ref()
                .filter(|p| !p.is_empty())
            {
                flags.push(format!(
                    "--data-binary {}",
                    shell_words::quote(&format!("@{path}"))
                ));
            }
        }
    }

    let flags_joined = flags.join(" \\\n  ");
    lines.push(format!(
        "curl {flags_joined} \\\n  {}",
        shell_words::quote(&url)
    ));
    lines.join("\n")
}

/// Appends enabled query params to `url` for targets (curl, Rust) that
/// don't build the query separately — a plain `?k=v&k2=v2` string, params
/// left unencoded (matching how this codebase's own `curl_import.rs` leaves
/// a parsed URL's query string untouched rather than auto-splitting it).
fn url_with_query(url: &str, query: &[(&str, &str)]) -> String {
    if query.is_empty() {
        return url.to_string();
    }
    let sep = if url.contains('?') { "&" } else { "?" };
    let qs: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("{url}{sep}{}", qs.join("&"))
}

// ---------- JavaScript (fetch) ----------

fn js_snippet(request: &RequestItem, auth: &AuthConfig) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut query: Vec<(String, String)> = enabled_pairs(&request.params)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    let mut headers: Vec<(String, String)> = enabled_pairs(&request.headers)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let mut body_expr: Option<String> = None;
    let mut extra_setup: Vec<String> = Vec::new();

    match auth_contribution(auth) {
        AuthContribution::BasicCreds(user, pass) => {
            // No native `fetch` Basic-auth helper — a plain header is the
            // real, correct shape here (base64 computed at request time by
            // `btoa`, so it stays correct even if `user`/`pass` are edited).
            extra_setup.push(format!("const basicAuth = btoa(`{user}:{pass}`);"));
            headers.push((
                "Authorization".to_string(),
                "`Basic ${basicAuth}`".to_string(),
            ));
        }
        AuthContribution::DigestCreds(_, _) => {
            lines.push("// Digest auth has no native `fetch` support — this request needs a live challenge-response.".to_string());
            lines.push(
                "// Send it from RustGirl directly, or use a library with Digest support."
                    .to_string(),
            );
        }
        AuthContribution::Header(k, v) => headers.push((k, v)),
        AuthContribution::BearerToken(token) => {
            headers.push(("Authorization".to_string(), format!("Bearer {token}")))
        }
        AuthContribution::Query(k, v) => query.push((k, v)),
        AuthContribution::Note(note) => lines.push(format!("// {note}")),
        AuthContribution::None => {}
    }
    let query_pairs: Vec<(&str, &str)> = query
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let url = url_with_query(&request.url, &query_pairs);

    match request.body.mode {
        BodyMode::None => {}
        BodyMode::Raw => body_expr = Some(js_string_literal(&request.body.raw)),
        BodyMode::Json => {
            headers.push(("Content-Type".to_string(), "application/json".to_string()));
            body_expr = Some(js_string_literal(&request.body.raw));
        }
        BodyMode::Form => {
            let pairs = enabled_pairs(&request.body.form);
            let entries: Vec<String> = pairs
                .iter()
                .map(|(k, v)| format!("  {}: {}", js_string_literal(k), js_string_literal(v)))
                .collect();
            extra_setup.push(format!(
                "const params = new URLSearchParams({{\n{}\n}});",
                entries.join(",\n")
            ));
            body_expr = Some("params".to_string());
        }
        BodyMode::Multipart => {
            extra_setup.push("const form = new FormData();".to_string());
            for field in request
                .body
                .multipart
                .iter()
                .filter(|f| f.enabled && !f.key.is_empty())
            {
                match field.field_type {
                    FormFieldType::Text => {
                        extra_setup.push(format!(
                            "form.append({}, {});",
                            js_string_literal(&field.key),
                            js_string_literal(&field.value)
                        ));
                    }
                    FormFieldType::File => {
                        extra_setup.push(format!(
                            "// form.append({}, /* a File object — pick one from an <input type=\"file\">, browsers can't read {} directly */);",
                            js_string_literal(&field.key),
                            field.file_path.as_deref().unwrap_or("(no path set)")
                        ));
                    }
                }
            }
            body_expr = Some("form".to_string());
        }
        BodyMode::Binary => {
            lines.push("// Binary body: browsers can't read a file path directly — supply a Blob/ArrayBuffer instead.".to_string());
        }
    }

    lines.extend(extra_setup);
    lines.push(format!("fetch({}, {{", js_string_literal(&url)));
    lines.push(format!(
        "  method: {},",
        js_string_literal(request.method.as_str())
    ));
    if !headers.is_empty() {
        lines.push("  headers: {".to_string());
        for (k, v) in &headers {
            // Basic auth's value is a template-literal expression, not a
            // plain string — emit it unquoted; everything else is a literal.
            let rendered = if v.starts_with('`') {
                v.clone()
            } else {
                js_string_literal(v)
            };
            lines.push(format!("    {}: {rendered},", js_string_literal(k)));
        }
        lines.push("  },".to_string());
    }
    if let Some(body) = body_expr {
        lines.push(format!("  body: {body},"));
    }
    lines.push("})".to_string());
    lines.push("  .then(response => response.text())".to_string());
    lines.push("  .then(text => console.log(text));".to_string());
    lines.join("\n")
}

fn js_string_literal(s: &str) -> String {
    format!("{:?}", s) // Rust's Debug-escaped string quoting is a valid JS string literal for our purposes (same backslash/quote escaping rules)
}

// ---------- Python (requests) ----------

fn python_snippet(request: &RequestItem, auth: &AuthConfig) -> String {
    let mut lines: Vec<String> = vec!["import requests".to_string()];
    let mut auth_expr: Option<String> = None;
    let mut extra_headers: Vec<(String, String)> = Vec::new();
    let mut extra_query: Vec<(String, String)> = Vec::new();

    match auth_contribution(auth) {
        AuthContribution::BasicCreds(user, pass) => {
            lines.push("from requests.auth import HTTPBasicAuth".to_string());
            auth_expr = Some(format!(
                "HTTPBasicAuth({}, {})",
                py_str(&user),
                py_str(&pass)
            ));
        }
        AuthContribution::DigestCreds(user, pass) => {
            lines.push("from requests.auth import HTTPDigestAuth".to_string());
            auth_expr = Some(format!(
                "HTTPDigestAuth({}, {})",
                py_str(&user),
                py_str(&pass)
            ));
        }
        AuthContribution::Header(k, v) => extra_headers.push((k, v)),
        AuthContribution::BearerToken(token) => {
            extra_headers.push(("Authorization".to_string(), format!("Bearer {token}")))
        }
        AuthContribution::Query(k, v) => extra_query.push((k, v)),
        AuthContribution::Note(note) => lines.push(format!("# {note}")),
        AuthContribution::None => {}
    }
    lines.push(String::new());

    lines.push(format!("url = {}", py_str(&request.url)));

    let headers: Vec<(&str, &str)> = enabled_pairs(&request.headers);
    if !headers.is_empty() || !extra_headers.is_empty() {
        lines.push("headers = {".to_string());
        for (k, v) in &headers {
            lines.push(format!("    {}: {},", py_str(k), py_str(v)));
        }
        for (k, v) in &extra_headers {
            lines.push(format!("    {}: {},", py_str(k), py_str(v)));
        }
        lines.push("}".to_string());
    }

    let mut query = enabled_pairs(&request.params);
    let extra_query_refs: Vec<(&str, &str)> = extra_query
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    query.extend(extra_query_refs);
    if !query.is_empty() {
        lines.push("params = {".to_string());
        for (k, v) in &query {
            lines.push(format!("    {}: {},", py_str(k), py_str(v)));
        }
        lines.push("}".to_string());
    }

    let mut extra_kwargs: Vec<String> = Vec::new();
    match request.body.mode {
        BodyMode::None => {}
        BodyMode::Raw => {
            lines.push(format!("data = {}", py_triple_str(&request.body.raw)));
            extra_kwargs.push("data=data".to_string());
        }
        BodyMode::Json => {
            lines.push(format!("data = {}", py_triple_str(&request.body.raw)));
            extra_kwargs.push("data=data".to_string());
            lines.push("headers = headers if 'headers' in dir() else {}".to_string());
        }
        BodyMode::Form => {
            let pairs = enabled_pairs(&request.body.form);
            lines.push("data = {".to_string());
            for (k, v) in &pairs {
                lines.push(format!("    {}: {},", py_str(k), py_str(v)));
            }
            lines.push("}".to_string());
            extra_kwargs.push("data=data".to_string());
        }
        BodyMode::Multipart => {
            let fields: Vec<_> = request
                .body
                .multipart
                .iter()
                .filter(|f| f.enabled && !f.key.is_empty())
                .collect();
            let (files, texts): (
                Vec<&&crate::model::FormField>,
                Vec<&&crate::model::FormField>,
            ) = fields
                .iter()
                .partition(|f| f.field_type == FormFieldType::File);
            if !files.is_empty() {
                lines.push("files = {".to_string());
                for f in &files {
                    lines.push(format!(
                        "    {}: open({}, 'rb'),",
                        py_str(&f.key),
                        py_str(f.file_path.as_deref().unwrap_or(""))
                    ));
                }
                lines.push("}".to_string());
                extra_kwargs.push("files=files".to_string());
            }
            if !texts.is_empty() {
                lines.push("data = {".to_string());
                for f in &texts {
                    lines.push(format!("    {}: {},", py_str(&f.key), py_str(&f.value)));
                }
                lines.push("}".to_string());
                extra_kwargs.push("data=data".to_string());
            }
        }
        BodyMode::Binary => {
            if let Some(path) = request
                .body
                .binary_file_path
                .as_ref()
                .filter(|p| !p.is_empty())
            {
                lines.push(format!("data = open({}, 'rb').read()", py_str(path)));
                extra_kwargs.push("data=data".to_string());
            }
        }
    }

    let mut kwargs = vec!["url".to_string()];
    if !headers.is_empty() || !extra_headers.is_empty() {
        kwargs.push("headers=headers".to_string());
    }
    if !query.is_empty() {
        kwargs.push("params=params".to_string());
    }
    kwargs.extend(extra_kwargs);
    if let Some(auth) = auth_expr {
        kwargs.push(format!("auth={auth}"));
    }

    lines.push(String::new());
    lines.push(format!(
        "response = requests.{}({})",
        request.method.as_str().to_lowercase(),
        kwargs.join(", ")
    ));
    lines.push("print(response.status_code)".to_string());
    lines.push("print(response.text)".to_string());
    lines.join("\n")
}

fn py_str(s: &str) -> String {
    format!("{:?}", s) // Python's escaping rules for a double-quoted string are close enough to Rust's Debug for our purposes (both backslash-escape `"`/`\`)
}

fn py_triple_str(s: &str) -> String {
    format!("\"\"\"{}\"\"\"", s.replace("\"\"\"", "\\\"\\\"\\\""))
}

// ---------- Rust (reqwest) ----------

fn rust_snippet(request: &RequestItem, auth: &AuthConfig) -> String {
    let mut lines: Vec<String> = vec!["let client = reqwest::Client::new();".to_string()];
    let mut auth_calls: Vec<String> = Vec::new();
    let mut extra_headers: Vec<(String, String)> = Vec::new();

    let mut extra_query: Vec<(String, String)> = Vec::new();
    match auth_contribution(auth) {
        AuthContribution::BasicCreds(user, pass) => {
            auth_calls.push(format!(
                ".basic_auth({}, Some({}))",
                rust_str(&user),
                rust_str(&pass)
            ));
        }
        AuthContribution::DigestCreds(_, _) => {
            lines.push("// reqwest has no native Digest support — this request needs a live challenge-response.".to_string());
            lines.push(
                "// Send it from RustGirl directly, or use a crate with Digest support."
                    .to_string(),
            );
        }
        AuthContribution::Header(k, v) => extra_headers.push((k, v)),
        AuthContribution::BearerToken(token) => {
            auth_calls.push(format!(".bearer_auth({})", rust_str(&token)))
        }
        AuthContribution::Query(k, v) => extra_query.push((k, v)),
        AuthContribution::Note(note) => lines.push(format!("// {note}")),
        AuthContribution::None => {}
    }

    let mut query = enabled_pairs(&request.params);
    let extra_query_refs: Vec<(&str, &str)> = extra_query
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    query.extend(extra_query_refs);
    let method_fn = request.method.as_str().to_lowercase();
    lines.push(format!(
        "let response = client\n    .{method_fn}({})",
        rust_str(&request.url)
    ));
    for (k, v) in enabled_pairs(&request.headers) {
        lines.push(format!("    .header({}, {})", rust_str(k), rust_str(v)));
    }
    for (k, v) in &extra_headers {
        lines.push(format!("    .header({}, {})", rust_str(k), rust_str(v)));
    }
    for call in &auth_calls {
        lines.push(format!("    {call}"));
    }
    if !query.is_empty() {
        let pairs: Vec<String> = query
            .iter()
            .map(|(k, v)| format!("({}, {})", rust_str(k), rust_str(v)))
            .collect();
        lines.push(format!("    .query(&[{}])", pairs.join(", ")));
    }

    match request.body.mode {
        BodyMode::None => {}
        BodyMode::Raw => lines.push(format!("    .body({})", rust_str(&request.body.raw))),
        BodyMode::Json => {
            lines.push("    .header(\"Content-Type\", \"application/json\")".to_string());
            lines.push(format!("    .body({})", rust_str(&request.body.raw)));
        }
        BodyMode::Form => {
            let pairs: Vec<String> = enabled_pairs(&request.body.form)
                .iter()
                .map(|(k, v)| format!("(\"{k}\", \"{v}\")"))
                .collect();
            lines.push(format!("    .form(&[{}])", pairs.join(", ")));
        }
        BodyMode::Multipart => {
            lines.push("    .multipart({".to_string());
            lines.push("        let mut form = reqwest::multipart::Form::new();".to_string());
            for field in request
                .body
                .multipart
                .iter()
                .filter(|f| f.enabled && !f.key.is_empty())
            {
                match field.field_type {
                    FormFieldType::Text => {
                        lines.push(format!(
                            "        form = form.text({}, {});",
                            rust_str(&field.key),
                            rust_str(&field.value)
                        ));
                    }
                    FormFieldType::File => {
                        lines.push(format!(
                            "        form = form.part({}, reqwest::multipart::Part::bytes(std::fs::read({})?));",
                            rust_str(&field.key),
                            rust_str(field.file_path.as_deref().unwrap_or(""))
                        ));
                    }
                }
            }
            lines.push("        form".to_string());
            lines.push("    })".to_string());
        }
        BodyMode::Binary => {
            if let Some(path) = request
                .body
                .binary_file_path
                .as_ref()
                .filter(|p| !p.is_empty())
            {
                lines.push(format!("    .body(std::fs::read({})?)", rust_str(path)));
            }
        }
    }
    lines.push("    .send()\n    .await?;".to_string());
    lines.push("println!(\"{}\", response.status());".to_string());
    lines.push("println!(\"{}\", response.text().await?);".to_string());
    lines.join("\n")
}

fn rust_str(s: &str) -> String {
    format!("{:?}", s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FormField, RequestBody};

    fn fixture(mode: BodyMode) -> RequestItem {
        let mut req = RequestItem::new("Test");
        req.url = "https://api.example.com/{{path}}".to_string();
        req.params.push(KeyValue {
            key: "q".to_string(),
            value: "1".to_string(),
            enabled: true,
        });
        req.headers.push(KeyValue {
            key: "X-Custom".to_string(),
            value: "abc".to_string(),
            enabled: true,
        });
        req.body = RequestBody {
            mode,
            ..Default::default()
        };
        match mode {
            BodyMode::Raw | BodyMode::Json => req.body.raw = "{\"id\": {{userId}}}".to_string(),
            BodyMode::Form => {
                req.body.form = vec![KeyValue {
                    key: "field".to_string(),
                    value: "value".to_string(),
                    enabled: true,
                }]
            }
            BodyMode::Multipart => {
                req.body.multipart = vec![
                    FormField {
                        key: "text".to_string(),
                        value: "hi".to_string(),
                        enabled: true,
                        field_type: FormFieldType::Text,
                        file_path: None,
                    },
                    FormField {
                        key: "file".to_string(),
                        value: String::new(),
                        enabled: true,
                        field_type: FormFieldType::File,
                        file_path: Some("/tmp/x.png".to_string()),
                    },
                ];
            }
            BodyMode::Binary => req.body.binary_file_path = Some("/tmp/payload.bin".to_string()),
            BodyMode::None => {}
        }
        req
    }

    #[test]
    fn curl_covers_every_body_mode() {
        for mode in [
            BodyMode::None,
            BodyMode::Raw,
            BodyMode::Json,
            BodyMode::Form,
            BodyMode::Multipart,
            BodyMode::Binary,
        ] {
            let snippet = curl_snippet(&fixture(mode), &AuthConfig::default());
            assert!(snippet.contains("-X GET"), "mode {mode:?}: {snippet}");
            assert!(
                snippet.contains("'q=1'") || snippet.contains("q=1"),
                "mode {mode:?} should include the query param: {snippet}"
            );
            match mode {
                BodyMode::None => {}
                BodyMode::Raw => assert!(snippet.contains("-d")),
                BodyMode::Json => {
                    assert!(snippet.contains("application/json"));
                    assert!(snippet.contains("-d"));
                }
                BodyMode::Form => assert!(snippet.contains("--data-urlencode")),
                BodyMode::Multipart => {
                    assert!(snippet.contains("-F"));
                    assert!(snippet.contains("@/tmp/x.png"));
                }
                BodyMode::Binary => assert!(
                    snippet.contains("--data-binary") && snippet.contains("@/tmp/payload.bin")
                ),
            }
        }
    }

    #[test]
    fn snippets_preserve_unresolved_variable_placeholders() {
        let req = fixture(BodyMode::Json);
        for target in CodeGenTarget::ALL {
            let snippet = generate_snippet(&req, &AuthConfig::default(), target);
            assert!(
                snippet.contains("{{path}}"),
                "{target:?} should keep the URL placeholder intact: {snippet}"
            );
            assert!(
                snippet.contains("{{userId}}"),
                "{target:?} should keep the body placeholder intact: {snippet}"
            );
        }
    }

    #[test]
    fn basic_auth_is_embedded_as_real_credentials_in_every_target() {
        let auth = AuthConfig {
            kind: AuthKind::Basic,
            params: vec![
                KeyValue {
                    key: "username".to_string(),
                    value: "alice".to_string(),
                    enabled: true,
                },
                KeyValue {
                    key: "password".to_string(),
                    value: "hunter2".to_string(),
                    enabled: true,
                },
            ],
        };
        let req = fixture(BodyMode::None);

        let curl = curl_snippet(&req, &auth);
        assert!(
            curl.contains("-u") && curl.contains("alice:hunter2"),
            "{curl}"
        );

        let py = python_snippet(&req, &auth);
        assert!(
            py.contains("HTTPBasicAuth") && py.contains("alice") && py.contains("hunter2"),
            "{py}"
        );

        let rs = rust_snippet(&req, &auth);
        assert!(
            rs.contains(".basic_auth(") && rs.contains("alice") && rs.contains("hunter2"),
            "{rs}"
        );

        let js = js_snippet(&req, &auth);
        assert!(js.contains("Authorization") && js.contains("btoa"), "{js}");
    }

    #[test]
    fn digest_auth_is_native_in_curl_and_python_but_a_note_elsewhere() {
        let auth = AuthConfig {
            kind: AuthKind::Digest,
            params: vec![
                KeyValue {
                    key: "username".to_string(),
                    value: "bob".to_string(),
                    enabled: true,
                },
                KeyValue {
                    key: "password".to_string(),
                    value: "secret".to_string(),
                    enabled: true,
                },
            ],
        };
        let req = fixture(BodyMode::None);

        let curl = curl_snippet(&req, &auth);
        assert!(
            curl.contains("--digest") && curl.contains("bob:secret"),
            "{curl}"
        );

        let py = python_snippet(&req, &auth);
        assert!(
            py.contains("HTTPDigestAuth") && py.contains("bob") && py.contains("secret"),
            "{py}"
        );

        let js = js_snippet(&req, &auth);
        assert!(
            js.to_lowercase().contains("digest") && !js.contains("bob"),
            "JS should note the limitation, not fabricate a header: {js}"
        );

        let rs = rust_snippet(&req, &auth);
        assert!(
            rs.to_lowercase().contains("digest") && !rs.contains("bob"),
            "Rust should note the limitation, not fabricate a header: {rs}"
        );
    }

    #[test]
    fn oauth1_and_awssigv4_never_fabricate_a_signature() {
        let oauth1 = AuthConfig {
            kind: AuthKind::OAuth1,
            params: vec![KeyValue {
                key: "consumerKey".to_string(),
                value: "ck".to_string(),
                enabled: true,
            }],
        };
        let aws = AuthConfig {
            kind: AuthKind::AwsSigV4,
            params: vec![KeyValue {
                key: "accessKey".to_string(),
                value: "AKIAEXAMPLE".to_string(),
                enabled: true,
            }],
        };
        let req = fixture(BodyMode::None);

        for auth in [&oauth1, &aws] {
            for target in CodeGenTarget::ALL {
                let snippet = generate_snippet(&req, auth, target);
                assert!(
                    !snippet.contains("Authorization: AWS4")
                        && !snippet.contains("oauth_signature")
                        && !snippet.to_lowercase().contains("authorization: oauth"),
                    "{target:?} should never embed a fabricated signature: {snippet}"
                );
            }
        }
    }

    #[test]
    fn bearer_and_api_key_are_embedded_in_every_target() {
        let bearer = AuthConfig {
            kind: AuthKind::Bearer,
            params: vec![KeyValue {
                key: "token".to_string(),
                value: "tok123".to_string(),
                enabled: true,
            }],
        };
        let req = fixture(BodyMode::None);
        for target in CodeGenTarget::ALL {
            let snippet = generate_snippet(&req, &bearer, target);
            assert!(
                snippet.contains("tok123"),
                "{target:?} should embed the bearer token: {snippet}"
            );
        }

        let api_key_query = AuthConfig {
            kind: AuthKind::ApiKey,
            params: vec![
                KeyValue {
                    key: "key".to_string(),
                    value: "apiKey".to_string(),
                    enabled: true,
                },
                KeyValue {
                    key: "value".to_string(),
                    value: "xyz".to_string(),
                    enabled: true,
                },
                KeyValue {
                    key: "in".to_string(),
                    value: "query".to_string(),
                    enabled: true,
                },
            ],
        };
        let curl = curl_snippet(&req, &api_key_query);
        assert!(
            curl.contains("apiKey=xyz"),
            "query-mode ApiKey should land in the URL: {curl}"
        );
    }

    /// Real, manual verification matching this phase's own roadmap wording
    /// ("generated cURL should actually execute and reproduce the same
    /// request") — not just structural assertions. `#[ignore]`d and run
    /// manually (needs network + a shell), same convention as
    /// `scripting::tests::pm_send_request_hits_a_real_server` and
    /// `app::tests::start_run_completes_a_real_request_without_panicking`.
    /// Generates a real curl command (query params, a header, Basic auth,
    /// a JSON body) and actually executes it via `std::process::Command`,
    /// asserting the real server saw everything the snippet claimed it
    /// would send.
    #[test]
    #[ignore]
    fn generated_curl_command_actually_executes_and_reproduces_the_request() {
        let mut req = RequestItem::new("Manual verification");
        req.method = crate::model::Method::Post;
        req.url = "https://httpbin.org/post".to_string();
        req.params.push(KeyValue {
            key: "q".to_string(),
            value: "1".to_string(),
            enabled: true,
        });
        req.headers.push(KeyValue {
            key: "X-Test-Header".to_string(),
            value: "codegen-check".to_string(),
            enabled: true,
        });
        req.body = crate::model::RequestBody {
            mode: BodyMode::Json,
            raw: "{\"hello\":\"world\"}".to_string(),
            ..Default::default()
        };
        let auth = AuthConfig {
            kind: AuthKind::Basic,
            params: vec![
                KeyValue {
                    key: "username".to_string(),
                    value: "alice".to_string(),
                    enabled: true,
                },
                KeyValue {
                    key: "password".to_string(),
                    value: "hunter2".to_string(),
                    enabled: true,
                },
            ],
        };

        let snippet = curl_snippet(&req, &auth);
        println!("Generated command:\n{snippet}");

        // The snippet is itself a valid multi-line shell command (curl with
        // `\`-continuations) — run it for real via `sh -c`.
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&snippet)
            .output()
            .expect("failed to execute curl");
        let body = String::from_utf8_lossy(&output.stdout);
        println!("Response body:\n{body}");

        assert!(
            output.status.success(),
            "curl exited with an error: {:?}",
            output.status
        );
        // httpbin.org/post echoes back everything it received — confirms
        // the generated command really sent what the snippet claimed.
        assert!(
            body.contains("\"q\": \"1\""),
            "query param missing from what the server actually received: {body}"
        );
        assert!(
            body.contains("\"X-Test-Header\": \"codegen-check\""),
            "header missing: {body}"
        );
        assert!(
            body.contains("\"hello\": \"world\""),
            "JSON body missing: {body}"
        );
        assert!(
            body.contains("\"Authorization\": \"Basic"),
            "Basic auth header missing: {body}"
        );
    }
}
