use crate::model::{BodyMode, Environment, FormFieldType, RequestItem};
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub duration_ms: u128,
    pub size_bytes: usize,
}

/// A snapshot of exactly what went out on the wire: method, fully-resolved URL
/// (including query string), every header actually sent, and the raw body —
/// all *after* `{{variable}}` substitution, so it reflects reality rather than
/// what the editor fields say.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

pub async fn send_request(
    client: reqwest::Client,
    item: RequestItem,
    env: Option<Environment>,
) -> RequestOutcome {
    let resolve = |s: &str| -> String {
        match &env {
            Some(e) => e.resolve(s),
            None => s.to_string(),
        }
    };

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
    for h in item.headers.iter().filter(|h| h.enabled && !h.key.is_empty()) {
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
                                let part = reqwest::multipart::Part::bytes(bytes).file_name(file_name);
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
    let duration_ms = start.elapsed().as_millis();

    match result {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let status_text = resp
                .status()
                .canonical_reason()
                .unwrap_or("")
                .to_string();
            let headers: Vec<(String, String)> = resp
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();
            match resp.text().await {
                Ok(body) => {
                    let size_bytes = body.len();
                    RequestOutcome::Success {
                        request: snapshot,
                        response: HttpResponse {
                            status,
                            status_text,
                            headers,
                            body,
                            duration_ms,
                            size_bytes,
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
        Err(e) => RequestOutcome::Error {
            request: Some(snapshot),
            message: format!("Request failed: {e}"),
            duration_ms: Some(duration_ms),
        },
    }
}
