//! Postman-style pre-request / post-response Lua scripting.
//!
//! Both entry points run synchronously on the calling thread (the UI thread,
//! called from `app.rs` right before sending and right after receiving a
//! response) — `mlua::Lua` isn't `Send`, and there's no need for it to be
//! since script execution is fast and never crosses a thread boundary.
//! `Lua::new()` loads mlua's "safe" standard library subset (no `io`, no
//! `os.execute`/`os.remove`, no FFI), so a script can't touch the filesystem
//! or spawn processes.

use crate::http_client::HttpResponse;
use crate::model::{BodyMode, Environment, KeyValue, Method, RequestItem, TestResult};
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

/// Runs `item.pre_request_script` (a no-op if empty), letting it rewrite the
/// URL/method/headers/(raw or JSON) body in place and read/write `env`'s
/// variables through the `pm` table.
pub fn run_pre_request(item: &mut RequestItem, env: &mut Option<Environment>) -> ScriptRun {
    let script = item.pre_request_script.clone();
    if script.trim().is_empty() {
        return ScriptRun::default();
    }

    let log = Rc::new(RefCell::new(Vec::new()));
    let tests = Rc::new(RefCell::new(Vec::new()));
    let shared_item = Rc::new(RefCell::new(item.clone()));
    let shared_env = Rc::new(RefCell::new(env.clone()));

    let result = (|| -> mlua::Result<()> {
        let lua = Lua::new();
        let pm = install_core(&lua, log.clone(), tests.clone())?;
        install_environment(&lua, shared_env.clone(), &pm)?;
        pm.set("request", SharedRequest(shared_item.clone()))?;
        lua.globals().set("pm", pm)?;
        lua.load(&script).set_name("pre-request script").exec()
    })();

    *item = shared_item.borrow().clone();
    *env = shared_env.borrow().clone();
    finish(result, log, tests)
}

/// Runs `item.post_response_script` (a no-op if empty), giving it read
/// access to the response (or the error message, if the request failed) via
/// `pm.response`, read/write access to `env`'s variables, and `pm.test`.
pub fn run_post_response(
    item: &RequestItem,
    env: &mut Option<Environment>,
    response: Option<&HttpResponse>,
    error: Option<&str>,
) -> ScriptRun {
    let script = item.post_response_script.clone();
    if script.trim().is_empty() {
        return ScriptRun::default();
    }

    let log = Rc::new(RefCell::new(Vec::new()));
    let tests = Rc::new(RefCell::new(Vec::new()));
    let shared_env = Rc::new(RefCell::new(env.clone()));

    let result = (|| -> mlua::Result<()> {
        let lua = Lua::new();
        let pm = install_core(&lua, log.clone(), tests.clone())?;
        install_environment(&lua, shared_env.clone(), &pm)?;
        pm.set("response", SharedResponse::new(response, error))?;
        lua.globals().set("pm", pm)?;
        lua.load(&script).set_name("post-response script").exec()
    })();

    *env = shared_env.borrow().clone();
    finish(result, log, tests)
}

fn finish(
    result: mlua::Result<()>,
    log: Rc<RefCell<Vec<String>>>,
    tests: Rc<RefCell<Vec<TestResult>>>,
) -> ScriptRun {
    ScriptRun {
        log: Rc::try_unwrap(log).map(RefCell::into_inner).unwrap_or_default(),
        tests: Rc::try_unwrap(tests).map(RefCell::into_inner).unwrap_or_default(),
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
            let line = args
                .iter()
                .map(lua_display)
                .collect::<Vec<_>>()
                .join("\t");
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
            tests.borrow_mut().push(TestResult { name, passed, error });
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
        Value::String(s) => s.to_str().map(str::to_string).unwrap_or_else(|_| "<invalid utf8>".to_string()),
        Value::Table(_) => "<table>".to_string(),
        Value::Function(_) => "<function>".to_string(),
        _ => "<value>".to_string(),
    }
}

fn install_environment(lua: &Lua, env: Rc<RefCell<Option<Environment>>>, pm: &Table) -> mlua::Result<()> {
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
                    None => env.variables.push(KeyValue { key, value, enabled: true }),
                }
            }
            Ok(())
        })?,
    )?;

    pm.set("environment", environment)?;
    Ok(())
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
        fields.add_field_method_get("method", |_, this| Ok(this.0.borrow().method.as_str().to_string()));
        fields.add_field_method_set("method", |_, this, value: String| {
            if let Some(m) = Method::ALL.into_iter().find(|m| m.as_str().eq_ignore_ascii_case(&value)) {
                this.0.borrow_mut().method = m;
            }
            Ok(())
        });
        // Only Raw/JSON bodies are a single editable string; form/multipart/
        // binary bodies aren't meaningfully scriptable as text.
        fields.add_field_method_get("body", |_, this| {
            let item = this.0.borrow();
            Ok(matches!(item.body.mode, BodyMode::Raw | BodyMode::Json).then(|| item.body.raw.clone()))
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
            match item.headers.iter_mut().find(|h| h.key.eq_ignore_ascii_case(&key)) {
                Some(h) => {
                    h.value = value;
                    h.enabled = true;
                }
                None => item.headers.push(KeyValue { key, value, enabled: true }),
            }
            Ok(())
        });
    }
}

/// Bound into Lua as `pm.response`. Read-only: nothing about a completed
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
        // Convenience mirroring Postman's `pm.response.json()`.
        methods.add_method("json", |lua, this, ()| {
            let value: serde_json::Value =
                serde_json::from_str(&this.body).map_err(mlua::Error::external)?;
            lua.to_value(&value)
        });
    }
}
