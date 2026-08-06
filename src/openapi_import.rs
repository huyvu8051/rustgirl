//! One-way, best-effort OpenAPI 3.0 import — not a full JSON-Schema-aware
//! parser. Builds one `RequestItem` per operation, grouped into a folder per
//! first `tag` ("Ungrouped" if untagged); parameters become disabled
//! Params/Headers/path-param rows for the user to fill in real values into;
//! a JSON/form/multipart `requestBody` sets the matching body mode with an
//! empty body rather than attempting to synthesize an example from the
//! schema. There's no export path — real Postman treats OpenAPI import the
//! same way.
//!
//! Accepts either JSON or YAML text (tries JSON first, falls back to YAML —
//! `serde_yaml` can deserialize directly into a `serde_json::Value`, so the
//! rest of this module only ever deals with one `Value` type regardless of
//! which the input was).

use crate::model::{BodyMode, Collection, Folder, KeyValue, Method, RequestItem};
use serde_json::Value;

const HTTP_METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];

pub fn import_openapi(text: &str) -> Result<(Collection, Vec<String>), String> {
    let root: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(json_err) => serde_yaml::from_str::<Value>(text).map_err(|yaml_err| {
            format!("could not parse as JSON ({json_err}) or YAML ({yaml_err})")
        })?,
    };
    let mut warnings = Vec::new();

    let title = root
        .get("info")
        .and_then(|i| i.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("Imported API")
        .to_string();
    let base_url = root
        .get("servers")
        .and_then(Value::as_array)
        .and_then(|servers| servers.first())
        .and_then(|server| server.get("url"))
        .and_then(Value::as_str)
        .unwrap_or("{{baseUrl}}")
        .to_string();

    let mut collection = Collection::new(title);
    let Some(paths) = root.get("paths").and_then(Value::as_object) else {
        return Err("OpenAPI spec has no \"paths\" object".to_string());
    };

    // Grouped by first tag, preserving first-seen order (small N, so a
    // linear scan for "have we seen this tag" is simpler than pulling in an
    // ordered-map crate for it).
    let mut tag_order: Vec<String> = Vec::new();
    let mut tag_requests: Vec<(String, Vec<RequestItem>)> = Vec::new();

    for (path, path_item) in paths {
        let Some(path_item_obj) = path_item.as_object() else {
            continue;
        };
        for method_str in HTTP_METHODS {
            let Some(operation) = path_item_obj.get(method_str) else {
                continue;
            };

            let name = operation
                .get("summary")
                .and_then(Value::as_str)
                .or_else(|| operation.get("operationId").and_then(Value::as_str))
                .map(str::to_string)
                .unwrap_or_else(|| format!("{} {path}", method_str.to_uppercase()));

            let mut req = RequestItem::new(name.clone());
            req.method = import_openapi_method(method_str);
            req.url = format!("{base_url}{}", convert_openapi_path(path));

            if let Some(params) = operation.get("parameters").and_then(Value::as_array) {
                for param in params {
                    apply_openapi_parameter(param, &mut req);
                }
            }
            if let Some(request_body) = operation.get("requestBody") {
                apply_openapi_request_body(request_body, &mut req, &mut warnings, &name);
            }

            let tag = operation
                .get("tags")
                .and_then(Value::as_array)
                .and_then(|tags| tags.first())
                .and_then(Value::as_str)
                .unwrap_or("Ungrouped")
                .to_string();
            match tag_order.iter().position(|t| t == &tag) {
                Some(idx) => tag_requests[idx].1.push(req),
                None => {
                    tag_order.push(tag.clone());
                    tag_requests.push((tag, vec![req]));
                }
            }
        }
    }

    for (tag, requests) in tag_requests {
        let mut folder = Folder::new(tag);
        folder.requests = requests;
        collection.folders.push(folder);
    }
    if collection.folders.is_empty() {
        warnings.push("No operations found under \"paths\"".to_string());
    }

    Ok((collection, warnings))
}

fn import_openapi_method(m: &str) -> Method {
    match m {
        "get" => Method::Get,
        "post" => Method::Post,
        "put" => Method::Put,
        "patch" => Method::Patch,
        "delete" => Method::Delete,
        "head" => Method::Head,
        _ => Method::Options,
    }
}

/// `/users/{id}` -> `/users/:id`, matching this app's own path-param syntax.
fn convert_openapi_path(path: &str) -> String {
    let mut out = String::new();
    let mut chars = path.chars();
    while let Some(c) = chars.next() {
        if c != '{' {
            out.push(c);
            continue;
        }
        out.push(':');
        for c2 in chars.by_ref() {
            if c2 == '}' {
                break;
            }
            out.push(c2);
        }
    }
    out
}

fn apply_openapi_parameter(param: &Value, req: &mut RequestItem) {
    let Some(name) = param.get("name").and_then(Value::as_str) else {
        return;
    };
    let location = param.get("in").and_then(Value::as_str).unwrap_or("query");
    match location {
        "query" => req.params.push(KeyValue {
            key: name.to_string(),
            value: String::new(),
            enabled: false,
        }),
        "header" => req.headers.push(KeyValue {
            key: name.to_string(),
            value: String::new(),
            enabled: false,
        }),
        "path" => req.path_params.push(KeyValue {
            key: name.to_string(),
            value: String::new(),
            enabled: true,
        }),
        // "cookie" and anything else aren't modeled by this importer.
        _ => {}
    }
}

fn apply_openapi_request_body(
    request_body: &Value,
    req: &mut RequestItem,
    warnings: &mut Vec<String>,
    req_name: &str,
) {
    let Some(content) = request_body.get("content").and_then(Value::as_object) else {
        return;
    };
    if content.contains_key("application/json") {
        req.body.mode = BodyMode::Json;
        req.headers.push(KeyValue {
            key: "Content-Type".to_string(),
            value: "application/json".to_string(),
            enabled: true,
        });
    } else if content.contains_key("application/x-www-form-urlencoded") {
        req.body.mode = BodyMode::Form;
    } else if content.contains_key("multipart/form-data") {
        req.body.mode = BodyMode::Multipart;
    } else if let Some(first_type) = content.keys().next() {
        warnings.push(format!("Request {req_name:?}: unsupported request body content type {first_type:?}, body left empty"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_spec_json() -> String {
        serde_json::json!({
            "openapi": "3.0.0",
            "info": { "title": "Pet Store" },
            "servers": [{ "url": "https://api.example.com/v1" }],
            "paths": {
                "/pets": {
                    "get": {
                        "summary": "List pets",
                        "tags": ["pets"],
                        "parameters": [
                            { "name": "limit", "in": "query", "schema": { "type": "integer" } },
                        ],
                    },
                    "post": {
                        "summary": "Create a pet",
                        "tags": ["pets"],
                        "requestBody": {
                            "content": { "application/json": { "schema": { "type": "object" } } },
                        },
                    },
                },
                "/pets/{petId}": {
                    "get": {
                        "summary": "Get a pet",
                        "tags": ["pets"],
                        "parameters": [
                            { "name": "petId", "in": "path", "required": true, "schema": { "type": "string" } },
                        ],
                    },
                },
            },
        })
        .to_string()
    }

    #[test]
    fn imports_operations_grouped_by_tag_with_path_params_and_body() {
        let (collection, warnings) = import_openapi(&fixture_spec_json()).unwrap();

        assert_eq!(collection.name, "Pet Store");
        assert_eq!(collection.folders.len(), 1);
        let pets = &collection.folders[0];
        assert_eq!(pets.name, "pets");
        assert_eq!(pets.requests.len(), 3);

        let list = pets
            .requests
            .iter()
            .find(|r| r.name == "List pets")
            .unwrap();
        assert_eq!(list.method, Method::Get);
        assert_eq!(list.url, "https://api.example.com/v1/pets");
        assert_eq!(list.params[0].key, "limit");
        assert!(!list.params[0].enabled);

        let create = pets
            .requests
            .iter()
            .find(|r| r.name == "Create a pet")
            .unwrap();
        assert_eq!(create.method, Method::Post);
        assert_eq!(create.body.mode, BodyMode::Json);
        assert!(create.headers.iter().any(|h| h.key == "Content-Type"));

        let get_one = pets
            .requests
            .iter()
            .find(|r| r.name == "Get a pet")
            .unwrap();
        assert_eq!(get_one.url, "https://api.example.com/v1/pets/:petId");
        assert_eq!(get_one.path_params[0].key, "petId");
        assert!(get_one.path_params[0].enabled);

        assert!(warnings.is_empty());
    }

    #[test]
    fn untagged_operations_land_in_ungrouped_folder() {
        let json = serde_json::json!({
            "info": { "title": "X" },
            "paths": { "/ping": { "get": { "summary": "Ping" } } },
        })
        .to_string();
        let (collection, _warnings) = import_openapi(&json).unwrap();
        assert_eq!(collection.folders.len(), 1);
        assert_eq!(collection.folders[0].name, "Ungrouped");
        assert_eq!(collection.folders[0].requests[0].url, "{{baseUrl}}/ping");
    }

    #[test]
    fn yaml_input_parses_the_same_as_equivalent_json() {
        let yaml = "info:\n  title: From YAML\npaths:\n  /ok:\n    get:\n      summary: Ok\n";
        let (collection, _warnings) = import_openapi(yaml).unwrap();
        assert_eq!(collection.name, "From YAML");
        assert_eq!(collection.folders[0].requests[0].name, "Ok");
    }

    #[test]
    fn missing_paths_is_an_error() {
        let json = serde_json::json!({ "info": { "title": "X" } }).to_string();
        assert!(import_openapi(&json).is_err());
    }
}
