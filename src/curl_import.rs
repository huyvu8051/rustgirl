//! Parses a pasted curl command (e.g. copied from a browser's "Copy as
//! cURL", or hand-typed) into a `RequestItem`. Understands curl's own flags
//! well enough for the common case — not a shell interpreter, so variable
//! expansion, command substitution, and the like aren't supported.

use crate::model::{
    AuthConfig, AuthKind, BodyMode, FormField, FormFieldType, KeyValue, Method, RequestItem,
};

pub fn import_curl(command: &str) -> Result<RequestItem, String> {
    let normalized = normalize_line_continuations(command.trim());
    let mut tokens = shell_words::split(&normalized)
        .map_err(|e| format!("could not parse curl command: {e}"))?;
    if tokens.first().map(String::as_str) == Some("curl") {
        tokens.remove(0);
    }

    let mut method: Option<Method> = None;
    let mut url: Option<String> = None;
    let mut headers = Vec::new();
    let mut body_raw: Option<String> = None;
    let mut form_fields = Vec::new();
    let mut is_multipart = false;
    let mut auth = AuthConfig::default();

    let mut iter = tokens.into_iter();
    while let Some(tok) = iter.next() {
        match tok.as_str() {
            "-X" | "--request" => {
                let value = iter.next().ok_or("missing value after -X/--request")?;
                method = Some(parse_method(&value)?);
            }
            "-H" | "--header" => {
                let value = iter.next().ok_or("missing value after -H/--header")?;
                if let Some((key, val)) = value.split_once(':') {
                    headers.push(KeyValue {
                        key: key.trim().to_string(),
                        value: val.trim().to_string(),
                        enabled: true,
                    });
                }
            }
            "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-ascii"
            | "--data-urlencode" => {
                let value = iter.next().ok_or("missing value after -d/--data")?;
                // curl concatenates repeated -d flags with `&`, matching form-encoded semantics.
                body_raw = Some(match body_raw {
                    Some(existing) => format!("{existing}&{value}"),
                    None => value,
                });
                if method.is_none() {
                    method = Some(Method::Post);
                }
            }
            "-u" | "--user" => {
                let value = iter.next().ok_or("missing value after -u/--user")?;
                let (user, pass) = value.split_once(':').unwrap_or((value.as_str(), ""));
                auth = AuthConfig {
                    kind: AuthKind::Basic,
                    params: vec![
                        KeyValue {
                            key: "username".to_string(),
                            value: user.to_string(),
                            enabled: true,
                        },
                        KeyValue {
                            key: "password".to_string(),
                            value: pass.to_string(),
                            enabled: true,
                        },
                    ],
                };
            }
            "-F" | "--form" => {
                let value = iter.next().ok_or("missing value after -F/--form")?;
                is_multipart = true;
                if let Some((key, val)) = value.split_once('=') {
                    form_fields.push(if let Some(path) = val.strip_prefix('@') {
                        FormField {
                            key: key.to_string(),
                            value: String::new(),
                            enabled: true,
                            field_type: FormFieldType::File,
                            file_path: Some(path.to_string()),
                        }
                    } else {
                        FormField {
                            key: key.to_string(),
                            value: val.to_string(),
                            enabled: true,
                            field_type: FormFieldType::Text,
                            file_path: None,
                        }
                    });
                }
                if method.is_none() {
                    method = Some(Method::Post);
                }
            }
            "--url" => {
                url = Some(iter.next().ok_or("missing value after --url")?);
            }
            // Recognized flags with no modeled equivalent yet — accepted and ignored
            // rather than treated as errors, so the rest of the command still imports.
            "-k" | "--insecure" | "-s" | "--silent" | "-i" | "--include" | "-v" | "--verbose"
            | "-L" | "--location" | "--compressed" => {}
            other
                if !other.starts_with('-')
                // A bare positional argument is the URL — curl allows this
                // instead of (or alongside a redundant) `--url`.
                && url.is_none() =>
            {
                url = Some(other.to_string());
            }
            _ => {} // Unrecognized flag: ignore, don't fail the whole import over it.
        }
    }

    let url = url.ok_or("no URL found in curl command")?;
    let mut req = RequestItem::new(derive_request_name(&url));
    req.method = method.unwrap_or(Method::Get);
    req.headers = headers;
    req.auth = auth;
    req.url = url;

    if is_multipart {
        req.body.mode = BodyMode::Multipart;
        req.body.multipart = form_fields;
    } else if let Some(raw) = body_raw {
        let looks_json = matches!(raw.trim_start().as_bytes().first(), Some(b'{') | Some(b'['));
        req.body.mode = if looks_json {
            BodyMode::Json
        } else {
            BodyMode::Raw
        };
        req.body.raw = raw;
    }

    Ok(req)
}

/// Chrome/Firefox's "Copy as cURL" (and hand-wrapped shell commands) split
/// the command across lines with a trailing `\`. `shell_words` splits on
/// whitespace, not shell grammar, so these need joining first or the `\`
/// and newline would otherwise end up as stray token content.
fn normalize_line_continuations(command: &str) -> String {
    command.replace("\\\r\n", " ").replace("\\\n", " ")
}

fn parse_method(raw: &str) -> Result<Method, String> {
    match raw.to_ascii_uppercase().as_str() {
        "GET" => Ok(Method::Get),
        "POST" => Ok(Method::Post),
        "PUT" => Ok(Method::Put),
        "PATCH" => Ok(Method::Patch),
        "DELETE" => Ok(Method::Delete),
        "HEAD" => Ok(Method::Head),
        "OPTIONS" => Ok(Method::Options),
        other => Err(format!("unsupported HTTP method {other:?}")),
    }
}

/// The last non-empty path segment (ignoring the query string), or the full
/// URL if there isn't one — just a reasonable default name, not parsed for
/// correctness.
fn derive_request_name(url: &str) -> String {
    let without_query = url.split('?').next().unwrap_or(url);
    without_query
        .trim_end_matches('/')
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(url)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_get_with_bare_url() {
        let req = import_curl("curl https://example.com/users").unwrap();
        assert_eq!(req.method, Method::Get);
        assert_eq!(req.url, "https://example.com/users");
        assert!(req.headers.is_empty());
        assert_eq!(req.body.mode, BodyMode::None);
    }

    #[test]
    fn post_with_headers_and_json_data_infers_method_and_body_mode() {
        let cmd = r#"curl -X POST https://example.com/users -H 'Content-Type: application/json' -H 'Authorization: Bearer abc' -d '{"name":"Alice"}'"#;
        let req = import_curl(cmd).unwrap();
        assert_eq!(req.method, Method::Post);
        assert_eq!(req.headers.len(), 2);
        assert_eq!(req.headers[0].key, "Content-Type");
        assert_eq!(req.headers[1].value, "Bearer abc");
        assert_eq!(req.body.mode, BodyMode::Json);
        assert_eq!(req.body.raw, r#"{"name":"Alice"}"#);
    }

    #[test]
    fn data_without_explicit_method_defaults_to_post() {
        let req = import_curl("curl https://example.com/x -d 'a=1'").unwrap();
        assert_eq!(req.method, Method::Post);
        assert_eq!(req.body.mode, BodyMode::Raw);
        assert_eq!(req.body.raw, "a=1");
    }

    #[test]
    fn user_flag_sets_basic_auth() {
        let req = import_curl("curl -u alice:secret https://example.com/private").unwrap();
        assert_eq!(req.auth.kind, AuthKind::Basic);
        assert_eq!(req.auth.params[0].value, "alice");
        assert_eq!(req.auth.params[1].value, "secret");
    }

    #[test]
    fn form_flags_produce_multipart_body_with_file_and_text_fields() {
        let cmd =
            "curl -X POST https://example.com/upload -F 'file=@/tmp/photo.png' -F 'caption=Hello'";
        let req = import_curl(cmd).unwrap();
        assert_eq!(req.body.mode, BodyMode::Multipart);
        assert_eq!(req.body.multipart.len(), 2);
        assert_eq!(req.body.multipart[0].field_type, FormFieldType::File);
        assert_eq!(
            req.body.multipart[0].file_path.as_deref(),
            Some("/tmp/photo.png")
        );
        assert_eq!(req.body.multipart[1].field_type, FormFieldType::Text);
        assert_eq!(req.body.multipart[1].value, "Hello");
    }

    #[test]
    fn multiline_backslash_continuations_are_joined_before_tokenizing() {
        let cmd =
            "curl 'https://example.com/users' \\\n  -H 'Accept: application/json' \\\n  -X GET\n";
        let req = import_curl(cmd).unwrap();
        assert_eq!(req.url, "https://example.com/users");
        assert_eq!(req.headers[0].key, "Accept");
        assert_eq!(req.method, Method::Get);
    }

    #[test]
    fn missing_url_is_an_error() {
        assert!(import_curl("curl -X GET").is_err());
    }

    #[test]
    fn request_name_derived_from_last_path_segment() {
        let req = import_curl("curl https://example.com/api/v1/users?active=true").unwrap();
        assert_eq!(req.name, "users");
    }
}
