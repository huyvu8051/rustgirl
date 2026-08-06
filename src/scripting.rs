//! Postman-style pre-request / post-response Lua scripting.
//!
//! Both entry points run synchronously on the calling thread (the UI thread,
//! called from `app.rs` right before sending and right after receiving a
//! response) — `mlua::Lua` isn't `Send`, and there's no need for it to be
//! since script execution is fast and never crosses a thread boundary.
//! `Lua::new()` loads mlua's "safe" standard library subset (no `io`, no
//! `os.execute`/`os.remove`, no FFI), so a script can't touch the filesystem
//! or spawn processes.

use crate::http_client::{HttpResponse, RequestOutcome};
use crate::model::{AuthConfig, BodyMode, Environment, KeyValue, Method, RequestItem, TestResult};
use mlua::{Lua, LuaSerdeExt, Table, Value, Variadic};
use std::cell::RefCell;
use std::rc::Rc;

/// What a script produced: anything printed via `console.log`, any
/// `pm.test(name, fn)` results, and the error message if the script itself
/// raised (a syntax error or an uncaught Lua error).
#[derive(Default)]
pub struct ScriptRun {
    pub log: Vec<String>,
    pub tests: Vec<TestResult>,
    pub error: Option<String>,
}

/// Everything a script phase needs beyond the request/response themselves —
/// bundled into one struct rather than a growing positional-parameter list
/// as `pm.*` gained more scopes. `env`/`globals`/`collection_variables` are
/// read/write (a script can call `pm.environment.set(...)` etc.); `client`/
/// `runtime` are only used by `pm.sendRequest`.
pub struct ScriptContext<'a> {
    pub env: &'a mut Option<Environment>,
    pub globals: &'a mut Vec<KeyValue>,
    pub collection_variables: &'a mut Vec<KeyValue>,
    pub client: reqwest::Client,
    /// A `Handle` (not `Handle::current()`) specifically: `pm.sendRequest`
    /// needs to block the calling (UI) thread on an async call, and doing
    /// that via `Handle::current()` depends on this thread still having an
    /// active `rt.enter()` guard, which isn't guaranteed to outlive
    /// `App`'s constructor. Blocking on an owned `Handle` cloned from
    /// `App.rt` works regardless.
    pub runtime: tokio::runtime::Handle,
}

/// Runs `script` (a no-op if empty) against `item`, letting it rewrite the
/// URL/method/headers/(raw or JSON) body in place and read/write variables
/// through the `pm` table. `script` is an explicit parameter (rather than
/// always reading `item.pre_request_script`) so collection/folder-level
/// scripts can run the same way against the same `item` — see
/// `App`'s script-chain callers in `app.rs`, which pass the collection's/
/// each folder's own script text here in turn, then finally the request's
/// own `item.pre_request_script`.
pub fn run_pre_request(script: &str, item: &mut RequestItem, ctx: ScriptContext) -> ScriptRun {
    if script.trim().is_empty() {
        return ScriptRun::default();
    }

    let log = Rc::new(RefCell::new(Vec::new()));
    let tests = Rc::new(RefCell::new(Vec::new()));
    let shared_item = Rc::new(RefCell::new(item.clone()));
    let shared_env = Rc::new(RefCell::new(ctx.env.clone()));
    let shared_globals = Rc::new(RefCell::new(ctx.globals.clone()));
    let shared_collection_vars = Rc::new(RefCell::new(ctx.collection_variables.clone()));

    let result = (|| -> mlua::Result<()> {
        let lua = Lua::new();
        let pm = install_core(&lua, log.clone(), tests.clone())?;
        install_environment(&lua, shared_env.clone(), &pm)?;
        install_variable_scope(&lua, shared_globals.clone(), &pm, "globals")?;
        install_variable_scope(
            &lua,
            shared_collection_vars.clone(),
            &pm,
            "collectionVariables",
        )?;
        install_merged_variables(
            &lua,
            shared_env.clone(),
            shared_collection_vars.clone(),
            shared_globals.clone(),
            &pm,
        )?;
        install_send_request(&lua, ctx.client.clone(), ctx.runtime.clone(), &pm)?;
        pm.set("request", SharedRequest(shared_item.clone()))?;
        lua.globals().set("pm", pm)?;
        lua.load(script).set_name("pre-request script").exec()
    })();

    *item = shared_item.borrow().clone();
    *ctx.env = shared_env.borrow().clone();
    *ctx.globals = shared_globals.borrow().clone();
    *ctx.collection_variables = shared_collection_vars.borrow().clone();
    finish(result, log, tests)
}

/// Runs `script` (a no-op if empty), giving it read access to the response
/// (or the error message, if the request failed) via `pm.response`, read/
/// write access to variables, and `pm.test`. `script` is an explicit
/// parameter for the same reason as `run_pre_request`'s — see its doc
/// comment.
pub fn run_post_response(
    script: &str,
    ctx: ScriptContext,
    response: Option<&HttpResponse>,
    error: Option<&str>,
) -> ScriptRun {
    if script.trim().is_empty() {
        return ScriptRun::default();
    }

    let log = Rc::new(RefCell::new(Vec::new()));
    let tests = Rc::new(RefCell::new(Vec::new()));
    let shared_env = Rc::new(RefCell::new(ctx.env.clone()));
    let shared_globals = Rc::new(RefCell::new(ctx.globals.clone()));
    let shared_collection_vars = Rc::new(RefCell::new(ctx.collection_variables.clone()));

    let result = (|| -> mlua::Result<()> {
        let lua = Lua::new();
        let pm = install_core(&lua, log.clone(), tests.clone())?;
        install_environment(&lua, shared_env.clone(), &pm)?;
        install_variable_scope(&lua, shared_globals.clone(), &pm, "globals")?;
        install_variable_scope(
            &lua,
            shared_collection_vars.clone(),
            &pm,
            "collectionVariables",
        )?;
        install_merged_variables(
            &lua,
            shared_env.clone(),
            shared_collection_vars.clone(),
            shared_globals.clone(),
            &pm,
        )?;
        install_send_request(&lua, ctx.client.clone(), ctx.runtime.clone(), &pm)?;
        pm.set("response", SharedResponse::new(response, error))?;
        lua.globals().set("pm", pm)?;
        lua.load(script).set_name("post-response script").exec()
    })();

    *ctx.env = shared_env.borrow().clone();
    *ctx.globals = shared_globals.borrow().clone();
    *ctx.collection_variables = shared_collection_vars.borrow().clone();
    finish(result, log, tests)
}

fn finish(
    result: mlua::Result<()>,
    log: Rc<RefCell<Vec<String>>>,
    tests: Rc<RefCell<Vec<TestResult>>>,
) -> ScriptRun {
    ScriptRun {
        log: Rc::try_unwrap(log)
            .map(RefCell::into_inner)
            .unwrap_or_default(),
        tests: Rc::try_unwrap(tests)
            .map(RefCell::into_inner)
            .unwrap_or_default(),
        error: result.err().map(|e| e.to_string()),
    }
}

/// Installs `console.log` and `pm.test`, common to both script phases.
/// Returns the (still-empty otherwise) `pm` table for the caller to add to.
fn install_core(
    lua: &Lua,
    log: Rc<RefCell<Vec<String>>>,
    tests: Rc<RefCell<Vec<TestResult>>>,
) -> mlua::Result<Table<'_>> {
    let console = lua.create_table()?;
    console.set(
        "log",
        lua.create_function(move |_, args: Variadic<Value>| {
            let line = args.iter().map(lua_display).collect::<Vec<_>>().join("\t");
            log.borrow_mut().push(line);
            Ok(())
        })?,
    )?;
    lua.globals().set("console", console)?;

    let pm = lua.create_table()?;
    pm.set(
        "test",
        lua.create_function(move |_, (name, func): (String, mlua::Function)| {
            let (passed, error) = match func.call::<_, ()>(()) {
                Ok(()) => (true, None),
                Err(e) => (false, Some(e.to_string())),
            };
            tests.borrow_mut().push(TestResult {
                name,
                passed,
                error,
            });
            Ok(())
        })?,
    )?;
    Ok(pm)
}

fn lua_display(value: &Value) -> String {
    match value {
        Value::Nil => "nil".to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s
            .to_str()
            .map(str::to_string)
            .unwrap_or_else(|_| "<invalid utf8>".to_string()),
        Value::Table(_) => "<table>".to_string(),
        Value::Function(_) => "<function>".to_string(),
        _ => "<value>".to_string(),
    }
}

fn install_environment(
    lua: &Lua,
    env: Rc<RefCell<Option<Environment>>>,
    pm: &Table,
) -> mlua::Result<()> {
    let environment = lua.create_table()?;

    let get_env = env.clone();
    environment.set(
        "get",
        lua.create_function(move |_, key: String| {
            Ok(get_env
                .borrow()
                .as_ref()
                .and_then(|e| e.variables.iter().find(|kv| kv.enabled && kv.key == key))
                .map(|kv| kv.value.clone()))
        })?,
    )?;

    let set_env = env;
    environment.set(
        "set",
        lua.create_function(move |_, (key, value): (String, String)| {
            if let Some(env) = set_env.borrow_mut().as_mut() {
                match env.variables.iter_mut().find(|kv| kv.key == key) {
                    Some(kv) => {
                        kv.value = value;
                        kv.enabled = true;
                    }
                    None => env.variables.push(KeyValue {
                        key,
                        value,
                        enabled: true,
                    }),
                }
            }
            Ok(())
        })?,
    )?;

    pm.set("environment", environment)?;
    Ok(())
}

/// `pm.globals` and `pm.collectionVariables` are structurally identical to
/// each other (a plain `Vec<KeyValue>`, always present, unlike the "maybe no
/// active environment" `Option` above) — one installer, parameterized by
/// which `pm.<name>` table it becomes.
fn install_variable_scope(
    lua: &Lua,
    vars: Rc<RefCell<Vec<KeyValue>>>,
    pm: &Table,
    name: &str,
) -> mlua::Result<()> {
    let table = lua.create_table()?;

    let get_vars = vars.clone();
    table.set(
        "get",
        lua.create_function(move |_, key: String| {
            Ok(get_vars
                .borrow()
                .iter()
                .find(|kv| kv.enabled && kv.key == key)
                .map(|kv| kv.value.clone()))
        })?,
    )?;

    let set_vars = vars;
    table.set(
        "set",
        lua.create_function(move |_, (key, value): (String, String)| {
            let mut vars = set_vars.borrow_mut();
            match vars.iter_mut().find(|kv| kv.key == key) {
                Some(kv) => {
                    kv.value = value;
                    kv.enabled = true;
                }
                None => vars.push(KeyValue {
                    key,
                    value,
                    enabled: true,
                }),
            }
            Ok(())
        })?,
    )?;

    pm.set(name, table)?;
    Ok(())
}

/// `pm.variables.get(key)` — a merged, read-only view across environment >
/// collection > global, mirroring `model::resolve_variables`'s own
/// precedence. No `.set`: unlike the individual scopes above, there's no way
/// to tell which one a plain `pm.variables.set(...)` should write into
/// (Postman's own `pm.variables` has the same read-only limitation).
fn install_merged_variables(
    lua: &Lua,
    env: Rc<RefCell<Option<Environment>>>,
    collection_variables: Rc<RefCell<Vec<KeyValue>>>,
    globals: Rc<RefCell<Vec<KeyValue>>>,
    pm: &Table,
) -> mlua::Result<()> {
    let table = lua.create_table()?;
    table.set(
        "get",
        lua.create_function(move |_, key: String| {
            let from_env = env.borrow().as_ref().and_then(|e| {
                e.variables
                    .iter()
                    .find(|kv| kv.enabled && kv.key == key)
                    .map(|kv| kv.value.clone())
            });
            if from_env.is_some() {
                return Ok(from_env);
            }
            let from_collection = collection_variables
                .borrow()
                .iter()
                .find(|kv| kv.enabled && kv.key == key)
                .map(|kv| kv.value.clone());
            if from_collection.is_some() {
                return Ok(from_collection);
            }
            Ok(globals
                .borrow()
                .iter()
                .find(|kv| kv.enabled && kv.key == key)
                .map(|kv| kv.value.clone()))
        })?,
    )?;
    pm.set("variables", table)?;
    Ok(())
}

/// `pm.sendRequest(urlOrRequest, function(err, response) ... end)`. Blocks
/// the calling thread until the sub-request completes — this app's scripts
/// run synchronously on the UI thread already (see the module doc comment),
/// so there's no separate async context to hand the request off to, and
/// from the user's perspective the outer script doesn't proceed until the
/// callback fires either way (same as real Postman).
fn install_send_request(
    lua: &Lua,
    client: reqwest::Client,
    runtime: tokio::runtime::Handle,
    pm: &Table,
) -> mlua::Result<()> {
    pm.set(
        "sendRequest",
        lua.create_function(move |_, (request, callback): (Value, mlua::Function)| {
            let item = match request_item_from_lua(request) {
                Ok(item) => item,
                Err(e) => return callback.call::<_, ()>((e, Value::Nil)),
            };
            let client = client.clone();
            let outcome = runtime.block_on(crate::http_client::send_request(
                client,
                item,
                Vec::new(),
                AuthConfig::default(),
            ));
            match outcome {
                RequestOutcome::Success { response, .. } => {
                    callback.call::<_, ()>((Value::Nil, SharedResponse::new(Some(&response), None)))
                }
                RequestOutcome::Error { message, .. } => {
                    callback.call::<_, ()>((message, Value::Nil))
                }
            }
        })?,
    )?;
    Ok(())
}

/// Accepts either a bare URL string (→ `GET`) or a table `{url=, method=,
/// header={}, body=}`, matching Postman's real flexible `pm.sendRequest`
/// signature closely enough to be useful without replicating it exactly.
fn request_item_from_lua(value: Value) -> Result<RequestItem, String> {
    match value {
        Value::String(s) => {
            let mut item = RequestItem::new("pm.sendRequest");
            item.url = s.to_str().map_err(|e| e.to_string())?.to_string();
            Ok(item)
        }
        Value::Table(t) => {
            let mut item = RequestItem::new("pm.sendRequest");
            item.url = t
                .get::<_, Option<String>>("url")
                .ok()
                .flatten()
                .unwrap_or_default();
            if item.url.is_empty() {
                return Err("pm.sendRequest: no url given".to_string());
            }
            if let Some(method) = t.get::<_, Option<String>>("method").ok().flatten()
                && let Some(m) = Method::ALL
                    .into_iter()
                    .find(|m| m.as_str().eq_ignore_ascii_case(&method))
            {
                item.method = m;
            }
            if let Some(headers) = t.get::<_, Option<Table>>("header").ok().flatten() {
                for pair in headers.pairs::<String, String>() {
                    let (key, value) = pair.map_err(|e| e.to_string())?;
                    item.headers.push(KeyValue {
                        key,
                        value,
                        enabled: true,
                    });
                }
            }
            if let Some(body) = t.get::<_, Option<String>>("body").ok().flatten() {
                item.body.mode = BodyMode::Raw;
                item.body.raw = body;
            }
            Ok(item)
        }
        _ => Err("pm.sendRequest: expected a URL string or a request table".to_string()),
    }
}

/// Bound into Lua as `pm.request`; every read/write goes straight through to
/// the shared `RequestItem`, so `request.url = "..."` etc. take effect
/// immediately without any snapshot-then-reapply step.
#[derive(Clone)]
struct SharedRequest(Rc<RefCell<RequestItem>>);

impl mlua::UserData for SharedRequest {
    fn add_fields<'lua, F: mlua::UserDataFields<'lua, Self>>(fields: &mut F) {
        fields.add_field_method_get("url", |_, this| Ok(this.0.borrow().url.clone()));
        fields.add_field_method_set("url", |_, this, value: String| {
            this.0.borrow_mut().url = value;
            Ok(())
        });
        fields.add_field_method_get("method", |_, this| {
            Ok(this.0.borrow().method.as_str().to_string())
        });
        fields.add_field_method_set("method", |_, this, value: String| {
            if let Some(m) = Method::ALL
                .into_iter()
                .find(|m| m.as_str().eq_ignore_ascii_case(&value))
            {
                this.0.borrow_mut().method = m;
            }
            Ok(())
        });
        // Only Raw/JSON bodies are a single editable string; form/multipart/
        // binary bodies aren't meaningfully scriptable as text.
        fields.add_field_method_get("body", |_, this| {
            let item = this.0.borrow();
            Ok(matches!(item.body.mode, BodyMode::Raw | BodyMode::Json)
                .then(|| item.body.raw.clone()))
        });
        fields.add_field_method_set("body", |_, this, value: String| {
            let mut item = this.0.borrow_mut();
            if matches!(item.body.mode, BodyMode::Raw | BodyMode::Json) {
                item.body.raw = value;
            }
            Ok(())
        });
    }

    fn add_methods<'lua, M: mlua::UserDataMethods<'lua, Self>>(methods: &mut M) {
        methods.add_method("getHeader", |_, this, key: String| {
            Ok(this
                .0
                .borrow()
                .headers
                .iter()
                .find(|h| h.enabled && h.key.eq_ignore_ascii_case(&key))
                .map(|h| h.value.clone()))
        });
        methods.add_method("setHeader", |_, this, (key, value): (String, String)| {
            let mut item = this.0.borrow_mut();
            match item
                .headers
                .iter_mut()
                .find(|h| h.key.eq_ignore_ascii_case(&key))
            {
                Some(h) => {
                    h.value = value;
                    h.enabled = true;
                }
                None => item.headers.push(KeyValue {
                    key,
                    value,
                    enabled: true,
                }),
            }
            Ok(())
        });
    }
}

/// Bound into Lua as `pm.response` (both the response of the request this
/// script phase belongs to, and — via `pm.sendRequest`'s callback — the
/// response of an ad hoc sub-request). Read-only: nothing about a completed
/// response makes sense for a script to rewrite.
struct SharedResponse {
    status: Option<u16>,
    headers: Vec<(String, String)>,
    body: String,
    duration_ms: Option<u128>,
    error: Option<String>,
}

impl SharedResponse {
    fn new(response: Option<&HttpResponse>, error: Option<&str>) -> Self {
        match response {
            Some(r) => Self {
                status: Some(r.status),
                headers: r.headers.clone(),
                body: r.body.clone(),
                duration_ms: Some(r.duration_ms),
                error: None,
            },
            None => Self {
                status: None,
                headers: Vec::new(),
                body: String::new(),
                duration_ms: None,
                error: error.map(str::to_string),
            },
        }
    }
}

impl mlua::UserData for SharedResponse {
    fn add_fields<'lua, F: mlua::UserDataFields<'lua, Self>>(fields: &mut F) {
        fields.add_field_method_get("status", |_, this| Ok(this.status));
        fields.add_field_method_get("body", |_, this| Ok(this.body.clone()));
        fields.add_field_method_get("duration_ms", |_, this| {
            Ok(this.duration_ms.map(|d| d as i64))
        });
        fields.add_field_method_get("error", |_, this| Ok(this.error.clone()));
    }

    fn add_methods<'lua, M: mlua::UserDataMethods<'lua, Self>>(methods: &mut M) {
        methods.add_method("getHeader", |_, this, key: String| {
            Ok(this
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(&key))
                .map(|(_, v)| v.clone()))
        });
        // `getHeader` only ever returns the *first* match — not enough for a
        // header that can legitimately repeat (most commonly `Set-Cookie`:
        // a response setting several cookies sends one header line per
        // cookie, all sharing that same name). Returns a plain Lua array
        // (1-indexed table) of every value for `key`, in the order the
        // server sent them, empty if none matched.
        methods.add_method("getHeaders", |lua, this, key: String| {
            let table = lua.create_table()?;
            for (i, (_, v)) in this
                .headers
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case(&key))
                .enumerate()
            {
                table.set(i + 1, v.clone())?;
            }
            Ok(table)
        });
        // Convenience mirroring Postman's `pm.response.json()`.
        methods.add_method("json", |lua, this, ()| {
            let value: serde_json::Value =
                serde_json::from_str(&this.body).map_err(mlua::Error::external)?;
            lua.to_value(&value)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `ScriptContext` needs a real `tokio::runtime::Handle` even for
    /// scripts that never call `pm.sendRequest` — building a throwaway
    /// `Runtime` directly (rather than `Handle::current()`) works without
    /// needing `#[tokio::test]`, matching how `App` itself obtains one.
    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    #[test]
    fn pm_globals_get_set_round_trips() {
        let rt = test_runtime();
        let mut env = None;
        let mut globals = Vec::new();
        let mut collection_variables = Vec::new();
        let mut item = RequestItem::new("test");
        item.pre_request_script = r#"
            pm.globals.set("token", "abc123")
            console.log(pm.globals.get("token"))
        "#
        .to_string();

        let script = item.pre_request_script.clone();

        let run = run_pre_request(
            &script,
            &mut item,
            ScriptContext {
                env: &mut env,
                globals: &mut globals,
                collection_variables: &mut collection_variables,
                client: reqwest::Client::new(),
                runtime: rt.handle().clone(),
            },
        );

        assert!(run.error.is_none(), "{:?}", run.error);
        assert_eq!(globals.len(), 1);
        assert_eq!(globals[0].key, "token");
        assert_eq!(globals[0].value, "abc123");
        assert_eq!(run.log, vec!["abc123".to_string()]);
    }

    #[test]
    fn pm_collection_variables_get_set_round_trips() {
        let rt = test_runtime();
        let mut env = None;
        let mut globals = Vec::new();
        let mut collection_variables = vec![KeyValue {
            key: "existing".to_string(),
            value: "1".to_string(),
            enabled: true,
        }];
        let mut item = RequestItem::new("test");
        item.pre_request_script = r#"
            pm.collectionVariables.set("existing", "2")
            pm.collectionVariables.set("new", "3")
        "#
        .to_string();

        let script = item.pre_request_script.clone();

        let run = run_pre_request(
            &script,
            &mut item,
            ScriptContext {
                env: &mut env,
                globals: &mut globals,
                collection_variables: &mut collection_variables,
                client: reqwest::Client::new(),
                runtime: rt.handle().clone(),
            },
        );

        assert!(run.error.is_none(), "{:?}", run.error);
        assert_eq!(
            collection_variables
                .iter()
                .find(|kv| kv.key == "existing")
                .unwrap()
                .value,
            "2"
        );
        assert_eq!(
            collection_variables
                .iter()
                .find(|kv| kv.key == "new")
                .unwrap()
                .value,
            "3"
        );
    }

    /// Phase 13b: `App`'s script-chain callers (`send_current_request`,
    /// `poll_responses`, the Runner's `start_run`) run a collection's own
    /// pre-request script, then each folder's, then the request's own —
    /// each a separate `run_pre_request` call sharing the *same* `env`/
    /// `globals`/`collection_variables` locals across the chain. This is
    /// the concrete behavior that sharing makes possible: a collection-level
    /// `pm.environment.set(...)` must be visible by the time the
    /// request-level script runs.
    #[test]
    fn chained_pre_request_scripts_share_the_same_environment() {
        let rt = test_runtime();
        let mut env = Some(Environment::new("Env"));
        let mut globals = Vec::new();
        let mut collection_variables = Vec::new();
        let mut item = RequestItem::new("test");

        // First link in the chain: the collection's own pre-request script.
        let collection_script = r#"pm.environment.set("traceId", "abc123")"#.to_string();
        let run1 = run_pre_request(
            &collection_script,
            &mut item,
            ScriptContext {
                env: &mut env,
                globals: &mut globals,
                collection_variables: &mut collection_variables,
                client: reqwest::Client::new(),
                runtime: rt.handle().clone(),
            },
        );
        assert!(run1.error.is_none(), "{:?}", run1.error);

        // Second link: the request's own pre-request script, run against
        // the *same* `env` local — it should see what the collection's
        // script just set.
        let request_script = r#"console.log(pm.environment.get("traceId"))"#.to_string();
        let run2 = run_pre_request(
            &request_script,
            &mut item,
            ScriptContext {
                env: &mut env,
                globals: &mut globals,
                collection_variables: &mut collection_variables,
                client: reqwest::Client::new(),
                runtime: rt.handle().clone(),
            },
        );

        assert!(run2.error.is_none(), "{:?}", run2.error);
        assert_eq!(run2.log, vec!["abc123".to_string()]);
    }

    /// `pm.response:getHeaders(key)` returns *every* value for a
    /// case-insensitively-matching header name, not just the first —
    /// specifically added to port a real migrated Postman script that reads
    /// several `Set-Cookie` headers (one response can set more than one
    /// cookie, each its own header line sharing that same name) and picks
    /// out the one containing a particular cookie name.
    #[test]
    fn pm_response_get_headers_returns_every_matching_header_not_just_the_first() {
        let rt = test_runtime();
        let mut env = Some(Environment::new("Test"));
        let mut globals = Vec::new();
        let mut collection_variables = Vec::new();
        let response = HttpResponse {
            status: 200,
            status_text: "OK".to_string(),
            headers: vec![
                ("Set-Cookie".to_string(), "session=abc; Path=/".to_string()),
                (
                    "Set-Cookie".to_string(),
                    "fingerprint=xyz789; Path=/".to_string(),
                ),
                ("Content-Type".to_string(), "application/json".to_string()),
            ],
            body: "{}".to_string(),
            duration_ms: 1,
            size_bytes: 2,
            raw_bytes: Vec::new(),
        };
        let script = r#"
            local cookies = pm.response:getHeaders("Set-Cookie")
            for _, value in ipairs(cookies) do
                local fp = value:match("fingerprint=([^;]+)")
                if fp then
                    pm.environment.set("fingerprint", fp)
                end
            end
            console.log(#cookies)
        "#;

        let run = run_post_response(
            script,
            ScriptContext {
                env: &mut env,
                globals: &mut globals,
                collection_variables: &mut collection_variables,
                client: reqwest::Client::new(),
                runtime: rt.handle().clone(),
            },
            Some(&response),
            None,
        );

        assert!(run.error.is_none(), "{:?}", run.error);
        assert_eq!(run.log, vec!["2".to_string()]);
        assert_eq!(
            env.unwrap()
                .variables
                .iter()
                .find(|kv| kv.key == "fingerprint")
                .map(|kv| kv.value.as_str()),
            Some("xyz789")
        );
    }

    #[test]
    fn pm_variables_reads_merged_scopes_in_precedence_order() {
        let rt = test_runtime();
        // "shared" is set in all three scopes — environment should win.
        let mut env = Some(Environment {
            id: uuid::Uuid::new_v4(),
            name: "Env".to_string(),
            variables: vec![KeyValue {
                key: "shared".to_string(),
                value: "from-env".to_string(),
                enabled: true,
            }],
        });
        let mut globals = vec![
            KeyValue {
                key: "shared".to_string(),
                value: "from-global".to_string(),
                enabled: true,
            },
            KeyValue {
                key: "onlyGlobal".to_string(),
                value: "global-value".to_string(),
                enabled: true,
            },
        ];
        let mut collection_variables = vec![KeyValue {
            key: "shared".to_string(),
            value: "from-collection".to_string(),
            enabled: true,
        }];
        let mut item = RequestItem::new("test");
        item.pre_request_script = r#"
            console.log(pm.variables.get("shared"))
            console.log(pm.variables.get("onlyGlobal"))
        "#
        .to_string();

        let script = item.pre_request_script.clone();

        let run = run_pre_request(
            &script,
            &mut item,
            ScriptContext {
                env: &mut env,
                globals: &mut globals,
                collection_variables: &mut collection_variables,
                client: reqwest::Client::new(),
                runtime: rt.handle().clone(),
            },
        );

        assert!(run.error.is_none(), "{:?}", run.error);
        assert_eq!(
            run.log,
            vec!["from-env".to_string(), "global-value".to_string()]
        );
    }

    #[test]
    fn request_item_from_lua_accepts_a_bare_url_string() {
        let lua = Lua::new();
        let value = Value::String(lua.create_string("https://example.com/x").unwrap());
        let item = request_item_from_lua(value).unwrap();
        assert_eq!(item.url, "https://example.com/x");
        assert_eq!(item.method, Method::Get);
    }

    #[test]
    fn request_item_from_lua_reads_a_request_table() {
        let lua = Lua::new();
        let table = lua.create_table().unwrap();
        table.set("url", "https://example.com/x").unwrap();
        table.set("method", "POST").unwrap();
        let headers = lua.create_table().unwrap();
        headers.set("X-Test", "1").unwrap();
        table.set("header", headers).unwrap();
        table.set("body", "hello").unwrap();

        let item = request_item_from_lua(Value::Table(table)).unwrap();
        assert_eq!(item.url, "https://example.com/x");
        assert_eq!(item.method, Method::Post);
        assert_eq!(item.headers[0].key, "X-Test");
        assert_eq!(item.body.mode, BodyMode::Raw);
        assert_eq!(item.body.raw, "hello");
    }

    #[test]
    fn request_item_from_lua_rejects_a_table_without_a_url() {
        let lua = Lua::new();
        let table = lua.create_table().unwrap();
        assert!(request_item_from_lua(Value::Table(table)).is_err());
    }

    #[test]
    fn request_item_from_lua_rejects_other_value_types() {
        assert!(request_item_from_lua(Value::Nil).is_err());
    }

    /// One-off manual verification against a real server that
    /// `pm.sendRequest`'s full path — block_on, actual HTTP round trip,
    /// `SharedResponse` construction, callback invocation — works end to
    /// end, not just the pure `request_item_from_lua` parsing covered above.
    /// Deliberately doesn't depend on the target returning JSON (httpbin.org
    /// intermittently 503s and isn't worth the flakiness) — just that a
    /// request actually went out and the callback saw a real status.
    /// `#[ignore]`d: needs network, not run by default/in CI.
    #[test]
    #[ignore]
    fn pm_send_request_hits_a_real_server() {
        let rt = test_runtime();
        let mut env = None;
        let mut globals = Vec::new();
        let mut collection_variables = Vec::new();
        let mut item = RequestItem::new("test");
        item.pre_request_script = r#"
            pm.sendRequest("https://example.com/", function(err, response)
                if err then
                    console.log("error", err)
                else
                    console.log("status", response.status)
                    console.log("has-body", #response.body > 0)
                end
            end)
        "#
        .to_string();

        let script = item.pre_request_script.clone();

        let run = run_pre_request(
            &script,
            &mut item,
            ScriptContext {
                env: &mut env,
                globals: &mut globals,
                collection_variables: &mut collection_variables,
                client: reqwest::Client::new(),
                runtime: rt.handle().clone(),
            },
        );

        assert!(run.error.is_none(), "{:?}", run.error);
        assert_eq!(
            run.log,
            vec!["status\t200".to_string(), "has-body\ttrue".to_string()]
        );
    }
}
