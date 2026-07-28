use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

impl Method {
    pub const ALL: [Method; 7] = [
        Method::Get,
        Method::Post,
        Method::Put,
        Method::Patch,
        Method::Delete,
        Method::Head,
        Method::Options,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
            Method::Head => "HEAD",
            Method::Options => "OPTIONS",
        }
    }

    pub fn to_reqwest(&self) -> reqwest::Method {
        match self {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Put => reqwest::Method::PUT,
            Method::Patch => reqwest::Method::PATCH,
            Method::Delete => reqwest::Method::DELETE,
            Method::Head => reqwest::Method::HEAD,
            Method::Options => reqwest::Method::OPTIONS,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct KeyValue {
    pub key: String,
    pub value: String,
    pub enabled: bool,
}

impl KeyValue {
    pub fn new() -> Self {
        Self {
            key: String::new(),
            value: String::new(),
            enabled: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyMode {
    None,
    Raw,
    Json,
    Form,
}

impl Default for BodyMode {
    fn default() -> Self {
        BodyMode::None
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct RequestBody {
    pub mode: BodyMode,
    pub raw: String,
    pub form: Vec<KeyValue>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RequestItem {
    pub id: Uuid,
    pub name: String,
    pub method: Method,
    pub url: String,
    pub params: Vec<KeyValue>,
    pub headers: Vec<KeyValue>,
    pub body: RequestBody,
    /// Values for `:name` path segments in `url` (e.g. `:tenantId` in
    /// `{{baseUrl}}/:tenantId/document-baskets`). Kept in sync with the URL
    /// text by the UI; absent in JSON saved before this feature existed.
    #[serde(default)]
    pub path_params: Vec<KeyValue>,
}

impl RequestItem {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            method: Method::Get,
            url: String::new(),
            params: vec![],
            headers: vec![],
            body: RequestBody::default(),
            path_params: vec![],
        }
    }
}

/// Names of `:name` path segments in `url` (the part before any `?query`),
/// in the order they appear. E.g. `{{baseUrl}}/:tenantId/document-baskets`
/// yields `["tenantId"]`.
pub fn extract_path_param_names(url: &str) -> Vec<String> {
    let path_part = url.split('?').next().unwrap_or(url);
    path_part
        .split('/')
        .filter_map(|segment| {
            let name = segment.strip_prefix(':')?;
            let is_valid_identifier =
                !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_');
            is_valid_identifier.then(|| name.to_string())
        })
        .collect()
}

/// Replaces each `:name` path segment with its resolved value, leaving the
/// segment untouched (so a broken substitution is visible instead of silently
/// producing an empty path component) if no non-empty value was provided.
pub fn substitute_path_params(url: &str, path_params: &[(String, String)]) -> String {
    let (path_part, query_part) = match url.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (url, None),
    };
    let new_path = path_part
        .split('/')
        .map(|segment| {
            segment
                .strip_prefix(':')
                .and_then(|name| path_params.iter().find(|(k, _)| k == name))
                .filter(|(_, v)| !v.is_empty())
                .map(|(_, v)| v.as_str())
                .unwrap_or(segment)
        })
        .collect::<Vec<_>>()
        .join("/");
    match query_part {
        Some(q) => format!("{new_path}?{q}"),
        None => new_path,
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Folder {
    pub id: Uuid,
    pub name: String,
    pub requests: Vec<RequestItem>,
}

impl Folder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            requests: vec![],
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Collection {
    pub id: Uuid,
    pub name: String,
    pub folders: Vec<Folder>,
    pub requests: Vec<RequestItem>,
}

impl Collection {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            folders: vec![],
            requests: vec![],
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Environment {
    pub id: Uuid,
    pub name: String,
    pub variables: Vec<KeyValue>,
}

impl Environment {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            variables: vec![],
        }
    }

    pub fn resolve(&self, text: &str) -> String {
        let mut out = text.to_string();
        for kv in &self.variables {
            if !kv.enabled || kv.key.is_empty() {
                continue;
            }
            let pattern = format!("{{{{{}}}}}", kv.key);
            out = out.replace(&pattern, &kv.value);
        }
        out
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HistoryEntry {
    pub id: Uuid,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub method: Method,
    pub url: String,
    pub status: Option<u16>,
    pub request: RequestItem,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AppData {
    pub collections: Vec<Collection>,
    pub environments: Vec<Environment>,
    pub history: Vec<HistoryEntry>,
}
