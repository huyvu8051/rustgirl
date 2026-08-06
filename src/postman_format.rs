//! Import/export for Postman's real JSON formats: Collection v2.1
//! (`https://schema.getpostman.com/json/collection/v2.1.0/collection.json`)
//! and Environment v1. Implemented by walking `serde_json::Value` directly
//! rather than deriving `Deserialize` onto dedicated structs — several parts
//! of the schema are inherently dynamic (an `item` array entry is a folder
//! or a request depending on which sibling key is present; an `auth` object
//! keys its param array by the auth type name itself), so one uniform
//! `Value`-walking style throughout is simpler than mixing derived and
//! hand-rolled parsing.
//!
//! Scripts are JavaScript in real Postman collections; since this app keeps
//! Lua (see the roadmap), an imported script's original JS lines are
//! embedded as a Lua comment block in the corresponding
//! `pre_request_script`/`post_response_script` field — visible immediately
//! in the existing script editor, inert (can't misfire as Lua), and
//! recoverable on export if the user hasn't since rewritten it.

use crate::model::{
    AuthConfig, AuthKind, BodyMode, Collection, Environment, Folder, FormField, FormFieldType,
    KeyValue, Method, RequestBody, RequestItem,
};
use serde_json::Value;

const JS_IMPORT_PREAMBLE: &str =
    "-- Imported from Postman as JavaScript — needs manual porting to Lua:\n";

// ---------------------------------------------------------------------------
// Collection import
// ---------------------------------------------------------------------------

/// Returns the imported collection plus any non-fatal warnings (unsupported
/// auth type, a script that needed the JS-as-comment treatment, ...).
pub fn import_collection(json: &str) -> Result<(Collection, Vec<String>), String> {
    let root: Value = serde_json::from_str(json).map_err(|e| format!("invalid JSON: {e}"))?;
    let mut warnings = Vec::new();

    let name = root
        .get("info")
        .and_then(|i| i.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("Imported Collection")
        .to_string();
    let mut collection = Collection::new(name);

    collection.auth = match root.get("auth") {
        Some(a) => import_auth(a, &mut warnings),
        None => AuthConfig::default(),
    };
    collection.variables = root
        .get("variable")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(import_kv_generic).collect())
        .unwrap_or_default();
    collection.description = root
        .get("info")
        .and_then(|i| i.get("description"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let label = format!("Collection {:?}", collection.name);
    import_events_into(
        &root,
        &mut collection.pre_request_script,
        &mut collection.post_response_script,
        &mut warnings,
        &label,
    );

    let items = root
        .get("item")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (folders, requests) = import_items(&items, &mut warnings);
    collection.folders = folders;
    collection.requests = requests;

    Ok((collection, warnings))
}

fn import_items(items: &[Value], warnings: &mut Vec<String>) -> (Vec<Folder>, Vec<RequestItem>) {
    let mut folders = Vec::new();
    let mut requests = Vec::new();
    for item in items {
        if item.get("item").is_some() {
            folders.push(import_folder(item, warnings));
        } else if item.get("request").is_some() {
            requests.push(import_request(item, warnings));
        } else {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("<unnamed>");
            warnings.push(format!(
                "Skipped item {name:?}: neither a folder nor a request"
            ));
        }
    }
    (folders, requests)
}

fn import_folder(item: &Value, warnings: &mut Vec<String>) -> Folder {
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Folder")
        .to_string();
    let mut folder = Folder::new(name);

    folder.auth = match item.get("auth") {
        Some(a) => import_auth(a, warnings),
        None => AuthConfig::default(),
    };
    folder.description = item
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let label = format!("Folder {:?}", folder.name);
    import_events_into(
        item,
        &mut folder.pre_request_script,
        &mut folder.post_response_script,
        warnings,
        &label,
    );

    let items = item
        .get("item")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (subfolders, requests) = import_items(&items, warnings);
    folder.folders = subfolders;
    folder.requests = requests;
    folder
}

/// Imports every `event` entry in `item.get("event")` into `pre_request_script`/
/// `post_response_script` — shared by the collection root, folders, and
/// requests, all of which carry these two fields now.
fn import_events_into(
    item: &Value,
    pre_request_script: &mut String,
    post_response_script: &mut String,
    warnings: &mut Vec<String>,
    label: &str,
) {
    let Some(events) = item.get("event").and_then(Value::as_array) else {
        return;
    };
    for event in events {
        import_event_into(
            event,
            pre_request_script,
            post_response_script,
            warnings,
            label,
        );
    }
}

fn import_request(item: &Value, warnings: &mut Vec<String>) -> RequestItem {
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Request")
        .to_string();
    let request = item.get("request").cloned().unwrap_or(Value::Null);
    let mut req = RequestItem::new(name.clone());

    req.method = import_method(
        request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET"),
        warnings,
        &name,
    );

    let (raw_url, path_params) = import_url(request.get("url"));
    req.url = raw_url;
    req.path_params = path_params;

    req.headers = request
        .get("header")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(import_kv_generic).collect())
        .unwrap_or_default();

    req.body = import_body(request.get("body"), warnings, &name);

    // Absent `auth` on the request means "inherit from parent", matching
    // real Postman's own export convention — not the same as an explicit
    // `{"type":"noauth"}`.
    req.auth = match request.get("auth") {
        Some(a) => import_auth(a, warnings),
        None => AuthConfig::default(),
    };
    req.description = request
        .get("description")
        .and_then(Value::as_str)
        .or_else(|| item.get("description").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string();

    let label = format!("Request {name:?}");
    import_events_into(
        item,
        &mut req.pre_request_script,
        &mut req.post_response_script,
        warnings,
        &label,
    );

    req
}

fn import_method(raw: &str, warnings: &mut Vec<String>, req_name: &str) -> Method {
    match raw.to_ascii_uppercase().as_str() {
        "GET" => Method::Get,
        "POST" => Method::Post,
        "PUT" => Method::Put,
        "PATCH" => Method::Patch,
        "DELETE" => Method::Delete,
        "HEAD" => Method::Head,
        "OPTIONS" => Method::Options,
        other => {
            warnings.push(format!(
                "Request {req_name:?}: unsupported method {other:?}, imported as GET"
            ));
            Method::Get
        }
    }
}

/// The schema allows `url` to be either a bare string or an object with a
/// `raw` field (used when the request also has `variable` entries for
/// `:pathParam`-style segments — the same concept as our `path_params`).
fn import_url(value: Option<&Value>) -> (String, Vec<KeyValue>) {
    match value {
        Some(Value::String(s)) => (s.clone(), Vec::new()),
        Some(obj @ Value::Object(_)) => {
            let raw = obj
                .get("raw")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    let host = join_str_array(obj.get("host"), ".");
                    let path = join_str_array(obj.get("path"), "/");
                    if path.is_empty() {
                        host
                    } else {
                        format!("{host}/{path}")
                    }
                });
            let path_params = obj
                .get("variable")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(import_kv_generic).collect())
                .unwrap_or_default();
            (raw, path_params)
        }
        _ => (String::new(), Vec::new()),
    }
}

fn join_str_array(value: Option<&Value>, sep: &str) -> String {
    value
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(sep)
        })
        .unwrap_or_default()
}

/// Handles the `{key, value, disabled?}` shape shared by headers, `variable`
/// entries, and `urlencoded` body fields. Not used for Environment `values`
/// entries, which use `enabled` instead of `disabled` — see
/// [`import_env_value`].
fn import_kv_generic(v: &Value) -> Option<KeyValue> {
    let obj = v.as_object()?;
    let key = obj.get("key").and_then(Value::as_str)?.to_string();
    let value = obj.get("value").map(value_to_string).unwrap_or_default();
    let enabled = !obj
        .get("disabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Some(KeyValue {
        key,
        value,
        enabled,
    })
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn import_body(value: Option<&Value>, warnings: &mut Vec<String>, req_name: &str) -> RequestBody {
    let mut body = RequestBody::default();
    let Some(value) = value else { return body };
    let Some(mode) = value.get("mode").and_then(Value::as_str) else {
        return body;
    };

    match mode {
        "raw" => {
            let language = value
                .get("options")
                .and_then(|o| o.get("raw"))
                .and_then(|r| r.get("language"))
                .and_then(Value::as_str);
            body.mode = if language == Some("json") {
                BodyMode::Json
            } else {
                BodyMode::Raw
            };
            body.raw = value
                .get("raw")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
        }
        "urlencoded" => {
            body.mode = BodyMode::Form;
            body.form = value
                .get("urlencoded")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(import_kv_generic).collect())
                .unwrap_or_default();
        }
        "formdata" => {
            body.mode = BodyMode::Multipart;
            body.multipart = value
                .get("formdata")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(import_form_field).collect())
                .unwrap_or_default();
        }
        "file" => {
            body.mode = BodyMode::Binary;
            body.binary_file_path = value
                .get("file")
                .and_then(|f| f.get("src"))
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        "graphql" => {
            warnings.push(format!(
                "Request {req_name:?}: GraphQL body imported as raw text (best effort)"
            ));
            body.mode = BodyMode::Raw;
            body.raw = value
                .get("graphql")
                .and_then(|g| g.get("query"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
        }
        other => {
            warnings.push(format!(
                "Request {req_name:?}: unsupported body mode {other:?}, ignored"
            ));
        }
    }
    body
}

fn import_form_field(v: &Value) -> Option<FormField> {
    let obj = v.as_object()?;
    let key = obj.get("key").and_then(Value::as_str)?.to_string();
    let enabled = !obj
        .get("disabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if obj.get("type").and_then(Value::as_str) == Some("file") {
        let file_path = obj.get("src").and_then(Value::as_str).map(str::to_string);
        Some(FormField {
            key,
            value: String::new(),
            enabled,
            field_type: FormFieldType::File,
            file_path,
        })
    } else {
        let value = obj.get("value").map(value_to_string).unwrap_or_default();
        Some(FormField {
            key,
            value,
            enabled,
            field_type: FormFieldType::Text,
            file_path: None,
        })
    }
}

/// Real Postman auth objects key their param array by the type name itself,
/// e.g. `{"type": "basic", "basic": [{"key": "username", ...}, ...]}`.
fn import_auth(value: &Value, warnings: &mut Vec<String>) -> AuthConfig {
    let Some(kind_str) = value.get("type").and_then(Value::as_str) else {
        return AuthConfig::default();
    };
    let kind = match kind_str {
        "noauth" => AuthKind::None,
        "basic" => AuthKind::Basic,
        "bearer" => AuthKind::Bearer,
        "apikey" => AuthKind::ApiKey,
        "digest" => AuthKind::Digest,
        "oauth1" => AuthKind::OAuth1,
        "oauth2" => AuthKind::OAuth2,
        "awsv4" => AuthKind::AwsSigV4,
        other => {
            warnings.push(format!("Unsupported auth type {other:?}, imported as None"));
            AuthKind::None
        }
    };
    let params = value
        .get(kind_str)
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(import_kv_generic).collect())
        .unwrap_or_default();
    AuthConfig { kind, params }
}

fn import_event_into(
    event: &Value,
    pre_request_script: &mut String,
    post_response_script: &mut String,
    warnings: &mut Vec<String>,
    label: &str,
) {
    let Some(listen) = event.get("listen").and_then(Value::as_str) else {
        return;
    };
    let exec_lines: Vec<String> = event
        .get("script")
        .and_then(|s| s.get("exec"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    // Postman's own UI commonly leaves a placeholder event behind — an
    // `exec` array that exists but holds only blank/whitespace lines (often
    // exactly `[""]`) — for a script tab the user opened but never actually
    // wrote anything into. Checking `is_empty()` on the `Vec` alone doesn't
    // catch this (the array has 1 element, just an empty string), so a real
    // export commonly triggered a false "needs manual porting" warning over
    // a script that was never really there. Skip whenever every line is
    // blank once trimmed, not just when the array itself has zero entries.
    if exec_lines.iter().all(|line| line.trim().is_empty()) {
        return;
    }
    let commented = embed_js_as_lua_comment(&exec_lines);
    // Postman allows more than one event with the same `listen` type on a
    // single item (e.g. two separate "test" scripts on one request) — an
    // earlier version of this function assigned (`=`) instead of appending,
    // silently dropping every event but the last one. Appended here instead,
    // so a request with N same-type scripts imports all N, not just one.
    let target = match listen {
        "prerequest" => &mut *pre_request_script,
        "test" => &mut *post_response_script,
        other => {
            warnings.push(format!(
                "{label}: unsupported event listener {other:?}, ignored"
            ));
            return;
        }
    };
    if !target.is_empty() {
        target.push('\n');
    }
    target.push_str(&commented);
    warnings.push(format!(
        "{label}: {listen} script is JavaScript — embedded as a comment, needs manual porting to Lua"
    ));
}

fn embed_js_as_lua_comment(exec_lines: &[String]) -> String {
    let mut out = String::from(JS_IMPORT_PREAMBLE);
    for line in exec_lines {
        out.push_str("-- ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The inverse of [`embed_js_as_lua_comment`] — `None` if `script` doesn't
/// start with the exact preamble (i.e. the user wrote their own Lua, or
/// there was never an imported script here), in which case export simply
/// omits this script rather than guessing.
fn extract_js_from_lua_comment(script: &str) -> Option<Vec<String>> {
    let body = script.strip_prefix(JS_IMPORT_PREAMBLE)?;
    Some(
        body.lines()
            .map(|line| line.strip_prefix("-- ").unwrap_or(line).to_string())
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Collection export
// ---------------------------------------------------------------------------

pub fn export_collection(collection: &Collection) -> String {
    let mut root = serde_json::json!({
        "info": {
            "name": collection.name,
            "description": collection.description,
            "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json",
        },
        "item": export_items(&collection.folders, &collection.requests),
        "variable": export_kv_list(&collection.variables),
    });
    if collection.auth.kind != AuthKind::Inherit {
        root["auth"] = export_auth(&collection.auth);
    }
    let events = export_events(
        &collection.pre_request_script,
        &collection.post_response_script,
    );
    if !events.is_empty() {
        root["event"] = Value::Array(events);
    }
    serde_json::to_string_pretty(&root).unwrap_or_default()
}

fn export_items(folders: &[Folder], requests: &[RequestItem]) -> Vec<Value> {
    folders
        .iter()
        .map(export_folder)
        .chain(requests.iter().map(export_request))
        .collect()
}

fn export_folder(folder: &Folder) -> Value {
    let mut obj = serde_json::json!({
        "name": folder.name,
        "description": folder.description,
        "item": export_items(&folder.folders, &folder.requests),
    });
    if folder.auth.kind != AuthKind::Inherit {
        obj["auth"] = export_auth(&folder.auth);
    }
    let events = export_events(&folder.pre_request_script, &folder.post_response_script);
    if !events.is_empty() {
        obj["event"] = Value::Array(events);
    }
    obj
}

fn export_request(req: &RequestItem) -> Value {
    let mut request = serde_json::json!({
        "method": req.method.as_str(),
        "header": export_kv_list(&req.headers),
        "url": export_url(&req.url, &req.path_params),
        "description": req.description,
    });
    if let Some(body) = export_body(&req.body) {
        request["body"] = body;
    }
    // Absence means "inherit from parent" on import — see `import_request`.
    if req.auth.kind != AuthKind::Inherit {
        request["auth"] = export_auth(&req.auth);
    }

    let mut item = serde_json::json!({
        "name": req.name,
        "request": request,
    });
    let events = export_events(&req.pre_request_script, &req.post_response_script);
    if !events.is_empty() {
        item["event"] = Value::Array(events);
    }
    item
}

fn export_url(raw: &str, path_params: &[KeyValue]) -> Value {
    if path_params.is_empty() {
        Value::String(raw.to_string())
    } else {
        serde_json::json!({ "raw": raw, "variable": export_kv_list(path_params) })
    }
}

fn export_kv_list(items: &[KeyValue]) -> Vec<Value> {
    items
        .iter()
        .map(|kv| {
            let mut obj = serde_json::json!({ "key": kv.key, "value": kv.value });
            if !kv.enabled {
                obj["disabled"] = Value::Bool(true);
            }
            obj
        })
        .collect()
}

fn export_body(body: &RequestBody) -> Option<Value> {
    match body.mode {
        BodyMode::None => None,
        BodyMode::Raw => Some(serde_json::json!({ "mode": "raw", "raw": body.raw })),
        BodyMode::Json => Some(serde_json::json!({
            "mode": "raw",
            "raw": body.raw,
            "options": { "raw": { "language": "json" } },
        })),
        BodyMode::Form => Some(serde_json::json!({
            "mode": "urlencoded",
            "urlencoded": export_kv_list(&body.form),
        })),
        BodyMode::Multipart => Some(serde_json::json!({
            "mode": "formdata",
            "formdata": export_form_fields(&body.multipart),
        })),
        BodyMode::Binary => Some(serde_json::json!({
            "mode": "file",
            "file": { "src": body.binary_file_path.clone().unwrap_or_default() },
        })),
    }
}

fn export_form_fields(fields: &[FormField]) -> Vec<Value> {
    fields
        .iter()
        .map(|f| {
            let mut obj = serde_json::Map::new();
            obj.insert("key".to_string(), Value::String(f.key.clone()));
            if !f.enabled {
                obj.insert("disabled".to_string(), Value::Bool(true));
            }
            match f.field_type {
                FormFieldType::Text => {
                    obj.insert("type".to_string(), Value::String("text".to_string()));
                    obj.insert("value".to_string(), Value::String(f.value.clone()));
                }
                FormFieldType::File => {
                    obj.insert("type".to_string(), Value::String("file".to_string()));
                    obj.insert(
                        "src".to_string(),
                        Value::String(f.file_path.clone().unwrap_or_default()),
                    );
                }
            }
            Value::Object(obj)
        })
        .collect()
}

fn export_auth(auth: &AuthConfig) -> Value {
    let kind_str = match auth.kind {
        AuthKind::Inherit | AuthKind::None => "noauth",
        AuthKind::Basic => "basic",
        AuthKind::Bearer => "bearer",
        AuthKind::ApiKey => "apikey",
        AuthKind::Digest => "digest",
        AuthKind::OAuth1 => "oauth1",
        AuthKind::OAuth2 => "oauth2",
        AuthKind::AwsSigV4 => "awsv4",
    };
    let mut obj = serde_json::Map::new();
    obj.insert("type".to_string(), Value::String(kind_str.to_string()));
    if kind_str != "noauth" {
        let params: Vec<Value> = auth
            .params
            .iter()
            .map(|kv| serde_json::json!({ "key": kv.key, "value": kv.value, "type": "string" }))
            .collect();
        obj.insert(kind_str.to_string(), Value::Array(params));
    }
    Value::Object(obj)
}

fn export_events(pre_request_script: &str, post_response_script: &str) -> Vec<Value> {
    let mut events = Vec::new();
    if let Some(exec) = extract_js_from_lua_comment(pre_request_script) {
        events.push(serde_json::json!({
            "listen": "prerequest",
            "script": { "type": "text/javascript", "exec": exec },
        }));
    }
    if let Some(exec) = extract_js_from_lua_comment(post_response_script) {
        events.push(serde_json::json!({
            "listen": "test",
            "script": { "type": "text/javascript", "exec": exec },
        }));
    }
    events
}

// ---------------------------------------------------------------------------
// Environment v1 import/export
// ---------------------------------------------------------------------------

pub fn import_environment(json: &str) -> Result<Environment, String> {
    let root: Value = serde_json::from_str(json).map_err(|e| format!("invalid JSON: {e}"))?;
    let name = root
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Imported Environment")
        .to_string();
    let mut env = Environment::new(name);
    env.variables = root
        .get("values")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(import_env_value).collect())
        .unwrap_or_default();
    Ok(env)
}

/// Environment `values` entries use `enabled` directly (`true`/`false`),
/// the *opposite* polarity from the `disabled` flag used everywhere else in
/// the collection format ([`import_kv_generic`]) — a real schema quirk, not
/// a typo.
fn import_env_value(v: &Value) -> Option<KeyValue> {
    let obj = v.as_object()?;
    let key = obj.get("key").and_then(Value::as_str)?.to_string();
    let value = obj.get("value").map(value_to_string).unwrap_or_default();
    let enabled = obj.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    Some(KeyValue {
        key,
        value,
        enabled,
    })
}

pub fn export_environment(env: &Environment) -> String {
    let values: Vec<Value> = env
        .variables
        .iter()
        .map(|kv| serde_json::json!({ "key": kv.key, "value": kv.value, "enabled": kv.enabled, "type": "default" }))
        .collect();
    let root = serde_json::json!({
        "id": env.id.to_string(),
        "name": env.name,
        "values": values,
        "_postman_variable_scope": "environment",
    });
    serde_json::to_string_pretty(&root).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_collection_json() -> String {
        serde_json::json!({
            "info": {
                "name": "My API",
                "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json",
            },
            "auth": { "type": "bearer", "bearer": [{"key": "token", "value": "abc123", "type": "string"}] },
            "variable": [{"key": "baseUrl", "value": "https://api.example.com", "type": "string"}],
            "item": [
                {
                    "name": "List Users",
                    "request": {
                        "method": "GET",
                        "header": [{"key": "Accept", "value": "application/json"}],
                        "url": "{{baseUrl}}/users",
                        "auth": { "type": "basic", "basic": [
                            {"key": "username", "value": "alice", "type": "string"},
                            {"key": "password", "value": "secret", "type": "string"},
                        ]},
                    },
                    "event": [
                        {"listen": "test", "script": {"type": "text/javascript", "exec": [
                            "pm.test(\"Status is 200\", function () {",
                            "  pm.response.to.have.status(200);",
                            "});",
                        ]}},
                    ],
                },
                {
                    "name": "Auth",
                    "auth": { "type": "digest", "digest": [{"key": "username", "value": "bob", "type": "string"}] },
                    "item": [
                        {
                            "name": "Tokens",
                            "item": [
                                {
                                    "name": "Refresh Token",
                                    "request": {
                                        "method": "POST",
                                        "header": [],
                                        "url": "{{baseUrl}}/auth/:tenantId/refresh",
                                        "body": {"mode": "raw", "raw": "{}", "options": {"raw": {"language": "json"}}},
                                    },
                                },
                            ],
                        },
                    ],
                },
            ],
        })
        .to_string()
    }

    #[test]
    fn import_collection_maps_variables_auth_nesting_and_scripts() {
        let (collection, warnings) = import_collection(&fixture_collection_json()).unwrap();

        assert_eq!(collection.name, "My API");
        assert_eq!(collection.auth.kind, AuthKind::Bearer);
        assert_eq!(collection.auth.params[0].value, "abc123");
        assert_eq!(collection.variables[0].key, "baseUrl");

        assert_eq!(collection.requests.len(), 1);
        let list_users = &collection.requests[0];
        assert_eq!(list_users.name, "List Users");
        assert_eq!(list_users.method, Method::Get);
        assert_eq!(list_users.url, "{{baseUrl}}/users");
        assert_eq!(list_users.headers[0].key, "Accept");
        assert_eq!(list_users.auth.kind, AuthKind::Basic);
        assert_eq!(list_users.auth.params[0].value, "alice");
        assert!(
            list_users
                .post_response_script
                .starts_with(JS_IMPORT_PREAMBLE)
        );
        assert!(list_users.post_response_script.contains("pm.test"));
        assert!(warnings.iter().any(|w| w.contains("JavaScript")));

        assert_eq!(collection.folders.len(), 1);
        let auth_folder = &collection.folders[0];
        assert_eq!(auth_folder.name, "Auth");
        assert_eq!(auth_folder.auth.kind, AuthKind::Digest);
        assert_eq!(auth_folder.folders.len(), 1);
        let tokens_folder = &auth_folder.folders[0];
        assert_eq!(tokens_folder.name, "Tokens");
        assert_eq!(tokens_folder.requests.len(), 1);
        let refresh = &tokens_folder.requests[0];
        assert_eq!(refresh.name, "Refresh Token");
        assert_eq!(refresh.method, Method::Post);
        // No `auth` key on this request in the fixture -> Inherit.
        assert_eq!(refresh.auth.kind, AuthKind::Inherit);
        assert_eq!(refresh.body.mode, BodyMode::Json);
        assert_eq!(refresh.body.raw, "{}");
    }

    /// Regression test: Postman allows more than one event with the same
    /// `listen` type on a single item (real-world example: two separate
    /// `test` scripts on one request, each capturing a different field from
    /// the response). An earlier version of `import_event_into` assigned
    /// (`=`) instead of appending, so only the *last* same-type event
    /// survived import — silently dropping the first one's script.
    #[test]
    fn multiple_same_type_events_on_one_item_are_concatenated_not_overwritten() {
        let json = r#"{
            "info": {"name": "Two Test Scripts"},
            "item": [
                {
                    "name": "Verify",
                    "request": {"method": "GET", "url": "http://x"},
                    "event": [
                        {"listen": "test", "script": {"exec": ["-- first script"]}},
                        {"listen": "test", "script": {"exec": ["-- second script"]}}
                    ]
                }
            ]
        }"#;
        let (collection, _warnings) = import_collection(json).unwrap();
        let script = &collection.requests[0].post_response_script;
        assert!(
            script.contains("first script"),
            "the first same-type event's script must survive, not just the last: {script:?}"
        );
        assert!(
            script.contains("second script"),
            "the second same-type event's script must also be present: {script:?}"
        );
    }

    /// Regression test: a real Postman export commonly has a script tab the
    /// user opened but never wrote anything into — `event.script.exec` is
    /// `[""]` (an array with one blank string), not an empty array. That
    /// used to still count as "there's a script here" (the `Vec` itself
    /// isn't empty), producing a false "needs manual porting" warning and a
    /// content-free comment block for a script that never really existed.
    #[test]
    fn blank_placeholder_script_is_skipped_without_a_warning() {
        let json = r#"{
            "info": {"name": "Placeholder Scripts"},
            "item": [
                {
                    "name": "Get Thing",
                    "request": {"method": "GET", "url": "http://x"},
                    "event": [
                        {"listen": "prerequest", "script": {"exec": [""]}},
                        {"listen": "test", "script": {"exec": ["", "  "]}}
                    ]
                }
            ]
        }"#;
        let (collection, warnings) = import_collection(json).unwrap();
        assert_eq!(collection.requests[0].pre_request_script, "");
        assert_eq!(collection.requests[0].post_response_script, "");
        assert!(
            warnings.is_empty(),
            "a blank placeholder script shouldn't produce a warning: {warnings:?}"
        );
    }

    #[test]
    fn export_then_reimport_is_stable() {
        let (collection, _warnings) = import_collection(&fixture_collection_json()).unwrap();
        let exported = export_collection(&collection);
        let (reimported, _warnings2) = import_collection(&exported).unwrap();

        assert_eq!(reimported.name, collection.name);
        assert_eq!(reimported.auth.kind, collection.auth.kind);
        assert_eq!(reimported.variables, collection.variables);
        assert_eq!(reimported.requests.len(), collection.requests.len());
        assert_eq!(
            reimported.requests[0].auth.kind,
            collection.requests[0].auth.kind
        );
        // The JS script round-trips faithfully through the comment-embedding trick.
        assert_eq!(
            reimported.requests[0].post_response_script,
            collection.requests[0].post_response_script
        );
        assert_eq!(
            reimported.folders[0].auth.kind,
            collection.folders[0].auth.kind
        );
        assert_eq!(
            reimported.folders[0].folders[0].requests[0].body.mode,
            collection.folders[0].folders[0].requests[0].body.mode
        );
    }

    #[test]
    fn unsupported_auth_type_imports_as_none_with_warning() {
        let json = serde_json::json!({
            "info": {"name": "X", "schema": "s"},
            "item": [{
                "name": "R",
                "request": {"method": "GET", "url": "http://x", "auth": {"type": "hawk", "hawk": []}},
            }],
        })
        .to_string();
        let (collection, warnings) = import_collection(&json).unwrap();
        assert_eq!(collection.requests[0].auth.kind, AuthKind::None);
        assert!(warnings.iter().any(|w| w.contains("hawk")));
    }

    #[test]
    fn environment_v1_round_trips_with_correct_enabled_polarity() {
        let json = serde_json::json!({
            "id": "abc",
            "name": "Staging",
            "values": [
                {"key": "token", "value": "t1", "enabled": true, "type": "default"},
                {"key": "disabledOne", "value": "x", "enabled": false, "type": "default"},
            ],
        })
        .to_string();
        let env = import_environment(&json).unwrap();
        assert_eq!(env.name, "Staging");
        assert!(env.variables[0].enabled);
        assert!(!env.variables[1].enabled);

        let exported = export_environment(&env);
        let reimported = import_environment(&exported).unwrap();
        assert_eq!(reimported.variables, env.variables);
    }
}
