//! On-disk layout, one file per request:
//!
//! ```text
//! <data_dir>/rustgirl/
//!   data.json                        legacy single-file format, kept as a
//!                                     migration source and otherwise unused
//!   collections/
//!     _order.json                    { "order": ["<collection-slug>", ...] }
//!     <collection-slug>/
//!       _collection.json             { id, name, auth, variables, folder_order, request_order }
//!       <request-slug>.json          root-level request
//!       <folder-slug>/
//!         _folder.json               { id, name, auth, folder_order, request_order }
//!         <request-slug>.json
//!         <nested-folder-slug>/      folders nest arbitrarily deep, same shape
//!   environments/
//!     _order.json
//!     <environment-slug>.json
//!   history/
//!     <timestamp>-<short-id>.json    one HistoryEntry per file
//!   globals.json                     flat Vec<KeyValue>, no ordering needed
//!   settings.json                    Settings — client config, not request state
//!   cookies.json                     the cookie jar (CookieStoreMutex, serde via its own impl)
//! ```
//!
//! `settings.json`/`cookies.json` are loaded/saved through their own
//! `load_settings`/`save_settings`/`load_cookie_jar`/`save_cookie_jar`
//! functions, not through `load()`/`save()` — they configure the HTTP
//! client itself, not `AppData`'s request/collection state, so `app.rs`
//! reads/writes them separately (once at startup, and whenever the
//! Settings panel or a request's response changes the cookie jar).
//!
//! Names are derived from `name` fields via [`slugify`], de-duplicated with a
//! short id suffix when two siblings would collide. Order is tracked
//! explicitly in each directory's metadata file (`_collection.json`,
//! `_folder.json`, `_order.json`) rather than via filename prefixes, so slugs
//! stay clean; any `.json` file present on disk but missing from the order
//! list is still picked up (appended, alphabetically) so hand-added files work.
//!
//! Every [`save`] call wipes and fully regenerates `collections/`,
//! `environments/`, and `history/` from the in-memory [`AppData`] snapshot —
//! the same "rewrite everything" semantics the old single-`data.json` format
//! had, just spread across many small files instead of one big one.

use crate::model::{
    AppData, AuthConfig, Collection, ConsoleEntry, Environment, Folder, HistoryEntry, KeyValue,
    Settings,
};
use reqwest_cookie_store::CookieStoreMutex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
struct CollectionMeta {
    id: Uuid,
    name: String,
    #[serde(default)]
    auth: AuthConfig,
    #[serde(default)]
    variables: Vec<KeyValue>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    pre_request_script: String,
    #[serde(default)]
    post_response_script: String,
    #[serde(default)]
    folder_order: Vec<String>,
    #[serde(default)]
    request_order: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct FolderMeta {
    id: Uuid,
    name: String,
    #[serde(default)]
    auth: AuthConfig,
    #[serde(default)]
    description: String,
    #[serde(default)]
    pre_request_script: String,
    #[serde(default)]
    post_response_script: String,
    #[serde(default)]
    folder_order: Vec<String>,
    #[serde(default)]
    request_order: Vec<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct OrderFile {
    #[serde(default)]
    order: Vec<String>,
}

const RESERVED_STEMS: [&str; 3] = ["_collection", "_folder", "_order"];

fn root_dir() -> PathBuf {
    let mut dir = dirs::data_dir().unwrap_or_else(std::env::temp_dir);
    dir.push("rustgirl");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn legacy_data_file(root: &Path) -> PathBuf {
    root.join("data.json")
}

fn collections_dir(root: &Path) -> PathBuf {
    root.join("collections")
}

fn environments_dir(root: &Path) -> PathBuf {
    root.join("environments")
}

fn history_dir(root: &Path) -> PathBuf {
    root.join("history")
}

fn globals_file(root: &Path) -> PathBuf {
    root.join("globals.json")
}

/// One-time migration from the old "postman_clone_rs" data directory (used
/// before the app was renamed to RustGirl) so existing collections aren't lost.
fn migrate_legacy_postman_dir(root: &Path) {
    let new_path = legacy_data_file(root);
    if new_path.exists() {
        return;
    }
    let Some(mut legacy_dir) = dirs::data_dir() else {
        return;
    };
    legacy_dir.push("postman_clone_rs");
    legacy_dir.push("data.json");
    if legacy_dir.exists() {
        let _ = std::fs::copy(&legacy_dir, &new_path);
    }
}

// ---------------------------------------------------------------------------
// Naming helpers
// ---------------------------------------------------------------------------

/// Lowercases, keeps alphanumerics, collapses everything else to single
/// dashes. Falls back to "untitled" for names with no alphanumeric content.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for c in name.trim().chars() {
        if c.is_alphanumeric() {
            slug.extend(c.to_lowercase());
            prev_dash = false;
        } else if !slug.is_empty() && !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "untitled".to_string()
    } else {
        slug
    }
}

/// Picks a slug unique within `used` (a single directory's siblings), adding
/// a short id suffix if the plain slug is already taken.
fn unique_slug(base: &str, id: &Uuid, used: &mut HashSet<String>) -> String {
    let mut candidate = base.to_string();
    if used.contains(&candidate) {
        candidate = format!("{base}-{}", &id.simple().to_string()[..8]);
    }
    while used.contains(&candidate) {
        candidate = format!("{candidate}-x");
    }
    used.insert(candidate.clone());
    candidate
}

/// Orders `available` slugs by `preferred` first, then appends anything in
/// `available` that `preferred` doesn't mention (alphabetically) — so files
/// added or reordered by hand outside the app still show up.
fn merge_order(preferred: Vec<String>, available: Vec<String>) -> Vec<String> {
    let available_set: HashSet<&str> = available.iter().map(|s| s.as_str()).collect();
    let mut result: Vec<String> = preferred
        .into_iter()
        .filter(|s| available_set.contains(s.as_str()))
        .collect();
    let leftovers = {
        let used: HashSet<&str> = result.iter().map(|s| s.as_str()).collect();
        let mut leftovers: Vec<String> = available
            .into_iter()
            .filter(|s| !used.contains(s.as_str()))
            .collect();
        leftovers.sort();
        leftovers
    };
    result.extend(leftovers);
    result
}

// ---------------------------------------------------------------------------
// Generic file I/O
// ---------------------------------------------------------------------------

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) {
    if let Ok(json) = serde_json::to_string_pretty(value) {
        let _ = std::fs::write(path, json);
    }
}

fn read_order_file(path: &Path) -> Vec<String> {
    read_json::<OrderFile>(path)
        .map(|o| o.order)
        .unwrap_or_default()
}

/// Stems of `*.json` files directly in `dir`, excluding metadata/order files.
fn list_request_files(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().map(|e| e == "json").unwrap_or(false))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .filter(|stem| !RESERVED_STEMS.contains(&stem.as_str()))
        .collect()
}

/// Names of subdirectories of `dir` that contain `marker` (e.g.
/// `_collection.json` or `_folder.json`), i.e. are actually collections/folders
/// and not some unrelated directory a user dropped in.
fn list_marked_dirs(dir: &Path, marker: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join(marker).exists())
        .filter_map(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
        .collect()
}

// ---------------------------------------------------------------------------
// Collections
// ---------------------------------------------------------------------------

fn save_collections(collections: &[Collection], dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::create_dir_all(dir);
    let mut used = HashSet::new();
    let mut order = Vec::new();
    for collection in collections {
        let slug = unique_slug(&slugify(&collection.name), &collection.id, &mut used);
        let coll_dir = dir.join(&slug);
        let _ = std::fs::create_dir_all(&coll_dir);
        save_collection_contents(collection, &coll_dir);
        order.push(slug);
    }
    write_json(&dir.join("_order.json"), &OrderFile { order });
}

fn save_collection_contents(collection: &Collection, coll_dir: &Path) {
    // Folders (subdirectories) and root requests (files) share one namespace
    // so a folder and a request never end up with the same slug.
    let mut used: HashSet<String> = RESERVED_STEMS.iter().map(|s| s.to_string()).collect();

    let mut folder_order = Vec::new();
    for folder in &collection.folders {
        let slug = unique_slug(&slugify(&folder.name), &folder.id, &mut used);
        let folder_dir = coll_dir.join(&slug);
        let _ = std::fs::create_dir_all(&folder_dir);
        save_folder_contents(folder, &folder_dir);
        folder_order.push(slug);
    }

    let mut request_order = Vec::new();
    for req in &collection.requests {
        let slug = unique_slug(&slugify(&req.name), &req.id, &mut used);
        write_json(&coll_dir.join(format!("{slug}.json")), req);
        request_order.push(slug);
    }

    write_json(
        &coll_dir.join("_collection.json"),
        &CollectionMeta {
            id: collection.id,
            name: collection.name.clone(),
            auth: collection.auth.clone(),
            variables: collection.variables.clone(),
            description: collection.description.clone(),
            pre_request_script: collection.pre_request_script.clone(),
            post_response_script: collection.post_response_script.clone(),
            folder_order,
            request_order,
        },
    );
}

/// Recursive: a folder's own subdirectories can themselves be `_folder.json`
/// folders, nested arbitrarily deep, using the same shared-namespace pattern
/// as a collection's folders/root-requests.
fn save_folder_contents(folder: &Folder, folder_dir: &Path) {
    let mut used: HashSet<String> = RESERVED_STEMS.iter().map(|s| s.to_string()).collect();

    let mut folder_order = Vec::new();
    for subfolder in &folder.folders {
        let slug = unique_slug(&slugify(&subfolder.name), &subfolder.id, &mut used);
        let subfolder_dir = folder_dir.join(&slug);
        let _ = std::fs::create_dir_all(&subfolder_dir);
        save_folder_contents(subfolder, &subfolder_dir);
        folder_order.push(slug);
    }

    let mut request_order = Vec::new();
    for req in &folder.requests {
        let slug = unique_slug(&slugify(&req.name), &req.id, &mut used);
        write_json(&folder_dir.join(format!("{slug}.json")), req);
        request_order.push(slug);
    }

    write_json(
        &folder_dir.join("_folder.json"),
        &FolderMeta {
            id: folder.id,
            name: folder.name.clone(),
            auth: folder.auth.clone(),
            description: folder.description.clone(),
            pre_request_script: folder.pre_request_script.clone(),
            post_response_script: folder.post_response_script.clone(),
            folder_order,
            request_order,
        },
    );
}

fn load_collections(dir: &Path) -> Vec<Collection> {
    let order = read_order_file(&dir.join("_order.json"));
    let available = list_marked_dirs(dir, "_collection.json");
    merge_order(order, available)
        .iter()
        .filter_map(|slug| load_collection(&dir.join(slug)))
        .collect()
}

fn load_collection(coll_dir: &Path) -> Option<Collection> {
    let meta: CollectionMeta = read_json(&coll_dir.join("_collection.json"))?;

    let folder_slugs = merge_order(
        meta.folder_order,
        list_marked_dirs(coll_dir, "_folder.json"),
    );
    let folders = folder_slugs
        .iter()
        .filter_map(|slug| load_folder(&coll_dir.join(slug)))
        .collect();

    let request_slugs = merge_order(meta.request_order, list_request_files(coll_dir));
    let requests = request_slugs
        .iter()
        .filter_map(|slug| read_json(&coll_dir.join(format!("{slug}.json"))))
        .collect();

    Some(Collection {
        id: meta.id,
        name: meta.name,
        folders,
        requests,
        variables: meta.variables,
        auth: meta.auth,
        description: meta.description,
        pre_request_script: meta.pre_request_script,
        post_response_script: meta.post_response_script,
    })
}

fn load_folder(folder_dir: &Path) -> Option<Folder> {
    let meta: FolderMeta = read_json(&folder_dir.join("_folder.json"))?;

    let folder_slugs = merge_order(
        meta.folder_order,
        list_marked_dirs(folder_dir, "_folder.json"),
    );
    let folders = folder_slugs
        .iter()
        .filter_map(|slug| load_folder(&folder_dir.join(slug)))
        .collect();

    let request_slugs = merge_order(meta.request_order, list_request_files(folder_dir));
    let requests = request_slugs
        .iter()
        .filter_map(|slug| read_json(&folder_dir.join(format!("{slug}.json"))))
        .collect();

    Some(Folder {
        id: meta.id,
        name: meta.name,
        requests,
        folders,
        auth: meta.auth,
        description: meta.description,
        pre_request_script: meta.pre_request_script,
        post_response_script: meta.post_response_script,
    })
}

// ---------------------------------------------------------------------------
// Environments
// ---------------------------------------------------------------------------

fn save_environments(envs: &[Environment], dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::create_dir_all(dir);
    let mut used = HashSet::new();
    let mut order = Vec::new();
    for env in envs {
        let slug = unique_slug(&slugify(&env.name), &env.id, &mut used);
        write_json(&dir.join(format!("{slug}.json")), env);
        order.push(slug);
    }
    write_json(&dir.join("_order.json"), &OrderFile { order });
}

fn load_environments(dir: &Path) -> Vec<Environment> {
    let order = read_order_file(&dir.join("_order.json"));
    let available = list_request_files(dir);
    merge_order(order, available)
        .iter()
        .filter_map(|slug| read_json(&dir.join(format!("{slug}.json"))))
        .collect()
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// Human-readable timestamp plus a short id suffix for uniqueness. Order on
/// load comes from the `timestamp` field itself, not the filename, so this
/// only needs to be unique — not lexicographically sortable.
fn history_slug(entry: &HistoryEntry) -> String {
    let ts = entry.timestamp.format("%Y%m%d-%H%M%S");
    let millis = entry.timestamp.timestamp_subsec_millis();
    format!("{ts}-{millis:03}-{}", &entry.id.simple().to_string()[..8])
}

fn save_history(history: &[HistoryEntry], dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::create_dir_all(dir);
    for entry in history {
        write_json(&dir.join(format!("{}.json", history_slug(entry))), entry);
    }
}

fn load_history(dir: &Path) -> Vec<HistoryEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut items: Vec<HistoryEntry> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().map(|e| e == "json").unwrap_or(false))
        .filter_map(|p| read_json(&p))
        .collect();
    // Newest first, matching the old `history.insert(0, entry)` ordering.
    items.sort_by_key(|e| std::cmp::Reverse(e.timestamp));
    items
}

// ---------------------------------------------------------------------------
// Globals
// ---------------------------------------------------------------------------

fn save_globals(globals: &[KeyValue], path: &Path) {
    write_json(path, &globals.to_vec());
}

fn load_globals(path: &Path) -> Vec<KeyValue> {
    read_json(path).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Hotkey bindings
// ---------------------------------------------------------------------------

fn hotkeys_file(root: &Path) -> PathBuf {
    root.join("hotkeys.json")
}

fn save_hotkeys(hotkeys: &std::collections::HashMap<char, Uuid>, path: &Path) {
    write_json(path, hotkeys);
}

fn load_hotkeys(path: &Path) -> std::collections::HashMap<char, Uuid> {
    read_json(path).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Console log — a plain-text troubleshooting trail, deliberately not JSON
// like everything else in this module: the point is to be readable with
// `tail -f`/a text editor, not machine-parsed.
// ---------------------------------------------------------------------------

fn console_log_file(root: &Path) -> PathBuf {
    root.join("console.log")
}

/// Renders one `ConsoleEntry` as a readable block and appends it to
/// `console.log` in the app data root. Never fails loudly — a filesystem
/// problem writing the *log* shouldn't interrupt the request that's
/// already completed by the time this runs.
pub fn append_console_log(entry: &ConsoleEntry) {
    append_console_log_to(entry, &console_log_file(&root_dir()));
}

/// The `console.log` file's real path — shown as plain, selectable text in
/// the Console panel so the user can navigate to it themselves (never
/// auto-opened; no `Command`/`open` invocation here).
pub fn console_log_path() -> PathBuf {
    console_log_file(&root_dir())
}

fn append_console_log_to(entry: &ConsoleEntry, path: &Path) {
    use std::io::Write;

    let mut block = format!(
        "[{}] {} {}\n",
        entry.timestamp.to_rfc3339(),
        entry.method.as_str(),
        entry.url
    );
    match (entry.status, &entry.error) {
        (Some(status), _) => {
            block.push_str(&format!(
                "  -> {status}, {}ms\n",
                entry.duration_ms.unwrap_or_default()
            ));
        }
        (None, Some(err)) => block.push_str(&format!("  -> ERROR: {err}\n")),
        (None, None) => block.push_str("  -> (no response)\n"),
    }
    for line in &entry.script_log {
        block.push_str(&format!("  console.log: {line}\n"));
    }
    if !entry.test_results.is_empty() {
        let passed = entry.test_results.iter().filter(|t| t.passed).count();
        block.push_str(&format!("  tests: {passed}/{}\n", entry.test_results.len()));
    }
    block.push('\n');

    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(block.as_bytes());
    }
}

// ---------------------------------------------------------------------------
// Settings and the cookie jar — see the module doc comment for why these
// are separate from `AppData`'s `load()`/`save()`.
// ---------------------------------------------------------------------------

fn settings_file(root: &Path) -> PathBuf {
    root.join("settings.json")
}

fn load_settings_from(path: &Path) -> Settings {
    read_json(path).unwrap_or_default()
}

fn save_settings_to(settings: &Settings, path: &Path) {
    write_json(path, settings);
}

pub fn load_settings() -> Settings {
    load_settings_from(&settings_file(&root_dir()))
}

pub fn save_settings(settings: &Settings) {
    save_settings_to(settings, &settings_file(&root_dir()));
}

fn cookies_file(root: &Path) -> PathBuf {
    root.join("cookies.json")
}

/// `CookieStoreMutex` implements `Serialize`/`Deserialize` itself (via
/// serde's own `Mutex<T>` support, gated behind the crate's `serde`
/// feature), so this needs no more ceremony than any other `read_json` call.
fn load_cookie_jar_from(path: &Path) -> CookieStoreMutex {
    read_json(path).unwrap_or_default()
}

fn save_cookie_jar_to(jar: &CookieStoreMutex, path: &Path) {
    write_json(path, jar);
}

pub fn load_cookie_jar() -> CookieStoreMutex {
    load_cookie_jar_from(&cookies_file(&root_dir()))
}

pub fn save_cookie_jar(jar: &CookieStoreMutex) {
    save_cookie_jar_to(jar, &cookies_file(&root_dir()));
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn load() -> AppData {
    let root = root_dir();
    let collections_path = collections_dir(&root);
    if collections_path.exists() {
        return AppData {
            collections: load_collections(&collections_path),
            environments: load_environments(&environments_dir(&root)),
            history: load_history(&history_dir(&root)),
            globals: load_globals(&globals_file(&root)),
            hotkey_bindings: load_hotkeys(&hotkeys_file(&root)),
        };
    }

    // Not migrated to the per-file layout yet: fall back to the legacy
    // single `data.json` (migrating it in from the even older
    // "postman_clone_rs" directory first, if needed), then materialize it as
    // the new file tree so subsequent loads take the fast path above.
    // `data.json` itself is left in place as a backup, never deleted.
    migrate_legacy_postman_dir(&root);
    let data = match std::fs::read_to_string(legacy_data_file(&root)) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
        Err(_) => AppData::default(),
    };
    save(&data);
    data
}

pub fn save(data: &AppData) {
    let root = root_dir();
    save_collections(&data.collections, &collections_dir(&root));
    save_environments(&data.environments, &environments_dir(&root));
    save_history(&data.history, &history_dir(&root));
    save_globals(&data.globals, &globals_file(&root));
    save_hotkeys(&data.hotkey_bindings, &hotkeys_file(&root));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{KeyValue, RequestItem};

    /// A fresh, empty directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "rustgirl-storage-test-{label}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn slugify_handles_spaces_case_and_punctuation() {
        assert_eq!(slugify("Get User!"), "get-user");
        assert_eq!(slugify("  multiple   spaces "), "multiple-spaces");
        // Unicode word characters (e.g. CJK) are kept as-is, just lowercased.
        assert_eq!(slugify("日本語"), "日本語");
        assert_eq!(slugify("!!!"), "untitled");
        assert_eq!(slugify(""), "untitled");
    }

    #[test]
    fn unique_slug_dedupes_same_name_siblings() {
        let mut used = HashSet::new();
        let a = unique_slug("login", &Uuid::new_v4(), &mut used);
        let b = unique_slug("login", &Uuid::new_v4(), &mut used);
        assert_eq!(a, "login");
        assert_ne!(a, b);
        assert!(b.starts_with("login-"));
    }

    #[test]
    fn merge_order_keeps_preferred_then_appends_extras_sorted() {
        let preferred = vec!["b".to_string(), "a".to_string()];
        let available = vec![
            "a".to_string(),
            "b".to_string(),
            "z".to_string(),
            "c".to_string(),
        ];
        assert_eq!(merge_order(preferred, available), vec!["b", "a", "c", "z"]);
    }

    #[test]
    fn collection_with_folder_and_requests_round_trips() {
        let tmp = TempDir::new("collections");

        let mut root_req = RequestItem::new("List Users");
        root_req.url = "{{baseUrl}}/users".to_string();
        root_req.params.push(KeyValue {
            key: "page".to_string(),
            value: "1".to_string(),
            enabled: true,
        });

        let mut nested_req = RequestItem::new("Login");
        nested_req.method = crate::model::Method::Post;

        let mut folder = Folder::new("Auth");
        folder.requests.push(nested_req.clone());

        // Two requests sharing a name at the same level must not collide.
        let dup_req = RequestItem::new("List Users");

        let mut collection = Collection::new("My API");
        collection.folders.push(folder);
        collection.requests.push(root_req.clone());
        collection.requests.push(dup_req.clone());

        save_collections(std::slice::from_ref(&collection), &tmp.0);
        let loaded = load_collections(&tmp.0);

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, collection.id);
        assert_eq!(loaded[0].name, "My API");
        assert_eq!(loaded[0].requests.len(), 2);
        assert_eq!(loaded[0].requests[0], root_req);
        assert_eq!(loaded[0].requests[1], dup_req);
        assert_eq!(loaded[0].folders.len(), 1);
        assert_eq!(loaded[0].folders[0].name, "Auth");
        assert_eq!(loaded[0].folders[0].requests[0], nested_req);

        // Files actually landed where the doc comment promises.
        let coll_dir = tmp.0.join("my-api");
        assert!(coll_dir.join("_collection.json").exists());
        assert!(coll_dir.join("list-users.json").exists());
        assert!(coll_dir.join("auth").join("_folder.json").exists());
        assert!(coll_dir.join("auth").join("login.json").exists());
    }

    #[test]
    fn nested_folder_within_folder_round_trips_with_auth_and_variables() {
        use crate::model::{AuthConfig, AuthKind};

        let tmp = TempDir::new("nested-folders");

        let mut leaf_req = RequestItem::new("Refresh Token");
        leaf_req.auth = AuthConfig {
            kind: AuthKind::Bearer,
            params: vec![KeyValue {
                key: "token".to_string(),
                value: "abc123".to_string(),
                enabled: true,
            }],
        };

        let mut inner_folder = Folder::new("Tokens");
        inner_folder.auth = AuthConfig {
            kind: AuthKind::Digest,
            params: vec![],
        };
        inner_folder.requests.push(leaf_req.clone());

        let mut outer_folder = Folder::new("Auth");
        outer_folder.folders.push(inner_folder);

        let mut collection = Collection::new("My API");
        collection.variables.push(KeyValue {
            key: "baseUrl".to_string(),
            value: "https://api.example.com".to_string(),
            enabled: true,
        });
        collection.folders.push(outer_folder);

        save_collections(std::slice::from_ref(&collection), &tmp.0);
        let loaded = load_collections(&tmp.0);

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].variables, collection.variables);
        assert_eq!(loaded[0].folders.len(), 1);
        let loaded_outer = &loaded[0].folders[0];
        assert_eq!(loaded_outer.name, "Auth");
        assert_eq!(loaded_outer.folders.len(), 1);
        let loaded_inner = &loaded_outer.folders[0];
        assert_eq!(loaded_inner.name, "Tokens");
        assert_eq!(loaded_inner.auth.kind, AuthKind::Digest);
        assert_eq!(loaded_inner.requests[0], leaf_req);

        // Nested directory actually landed on disk where expected.
        let leaf_path = tmp
            .0
            .join("my-api")
            .join("auth")
            .join("tokens")
            .join("refresh-token.json");
        assert!(leaf_path.exists());
    }

    /// Phase 13a/13b: `description`/`pre_request_script`/`post_response_script`
    /// on `Collection` and `Folder` (new this phase) round-trip through
    /// save/load just like every other field, at every nesting depth.
    #[test]
    fn collection_and_folder_description_and_scripts_round_trip() {
        let tmp = TempDir::new("descriptions-and-scripts");

        let mut folder = Folder::new("Auth");
        folder.description = "Everything auth-related.".to_string();
        folder.pre_request_script = "pm.environment.set(\"x\", \"1\")\n".to_string();
        folder.post_response_script = "pm.test(\"ok\", function() end)\n".to_string();

        let mut collection = Collection::new("My API");
        collection.description = "The whole public API.".to_string();
        collection.pre_request_script = "pm.globals.set(\"traceId\", \"abc\")\n".to_string();
        collection.post_response_script = "console.log(\"done\")\n".to_string();
        collection.folders.push(folder);

        save_collections(std::slice::from_ref(&collection), &tmp.0);
        let loaded = load_collections(&tmp.0);

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].description, collection.description);
        assert_eq!(loaded[0].pre_request_script, collection.pre_request_script);
        assert_eq!(
            loaded[0].post_response_script,
            collection.post_response_script
        );
        let loaded_folder = &loaded[0].folders[0];
        assert_eq!(loaded_folder.description, "Everything auth-related.");
        assert_eq!(
            loaded_folder.pre_request_script,
            "pm.environment.set(\"x\", \"1\")\n"
        );
        assert_eq!(
            loaded_folder.post_response_script,
            "pm.test(\"ok\", function() end)\n"
        );
    }

    /// Backward compatibility: a `_collection.json`/`_folder.json` written
    /// before this phase (no `description`/script keys at all) still loads,
    /// with those fields defaulting to empty — same `#[serde(default)]`
    /// convention every prior additive field on these structs already
    /// follows (`auth`, `variables`, ...).
    #[test]
    fn collection_and_folder_meta_without_new_fields_default_to_empty() {
        let tmp = TempDir::new("legacy-meta");
        let coll_dir = tmp.0.join("legacy-api");
        let folder_dir = coll_dir.join("auth");
        std::fs::create_dir_all(&folder_dir).unwrap();

        std::fs::write(
            coll_dir.join("_collection.json"),
            format!(
                r#"{{"id":"{}","name":"Legacy API","folder_order":["auth"],"request_order":[]}}"#,
                Uuid::new_v4()
            ),
        )
        .unwrap();
        std::fs::write(
            folder_dir.join("_folder.json"),
            format!(
                r#"{{"id":"{}","name":"Auth","folder_order":[],"request_order":[]}}"#,
                Uuid::new_v4()
            ),
        )
        .unwrap();

        let collection = load_collection(&coll_dir).expect("legacy collection should still load");
        assert_eq!(collection.description, "");
        assert_eq!(collection.pre_request_script, "");
        assert_eq!(collection.post_response_script, "");
        let folder = &collection.folders[0];
        assert_eq!(folder.description, "");
        assert_eq!(folder.pre_request_script, "");
        assert_eq!(folder.post_response_script, "");
    }

    #[test]
    fn hand_added_request_file_is_picked_up_without_order_entry() {
        let tmp = TempDir::new("hand-added");
        let collection = Collection::new("Scratch");
        save_collections(std::slice::from_ref(&collection), &tmp.0);

        let coll_dir = tmp.0.join("scratch");
        let extra = RequestItem::new("Manually Dropped In");
        write_json(&coll_dir.join("manually-dropped-in.json"), &extra);

        let loaded = load_collections(&tmp.0);
        assert_eq!(loaded[0].requests.len(), 1);
        assert_eq!(loaded[0].requests[0].name, "Manually Dropped In");
    }

    #[test]
    fn environments_round_trip_and_preserve_order() {
        let tmp = TempDir::new("environments");
        let envs = vec![Environment::new("Production"), Environment::new("Staging")];
        save_environments(&envs, &tmp.0);
        let loaded = load_environments(&tmp.0);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "Production");
        assert_eq!(loaded[1].name, "Staging");
    }

    #[test]
    fn globals_round_trip() {
        let tmp = TempDir::new("globals");
        let globals = vec![
            KeyValue {
                key: "apiVersion".to_string(),
                value: "v2".to_string(),
                enabled: true,
            },
            KeyValue {
                key: "disabled".to_string(),
                value: "x".to_string(),
                enabled: false,
            },
        ];
        let path = tmp.0.join("globals.json");
        save_globals(&globals, &path);
        assert_eq!(load_globals(&path), globals);

        // Missing file (never saved yet) is an empty scope, not an error.
        assert_eq!(
            load_globals(&tmp.0.join("missing.json")),
            Vec::<KeyValue>::new()
        );
    }

    #[test]
    fn hotkey_bindings_round_trip() {
        let tmp = TempDir::new("hotkeys");
        let mut hotkeys = std::collections::HashMap::new();
        hotkeys.insert('a', Uuid::new_v4());
        hotkeys.insert('5', Uuid::new_v4());
        let path = tmp.0.join("hotkeys.json");
        save_hotkeys(&hotkeys, &path);
        assert_eq!(load_hotkeys(&path), hotkeys);

        // Missing file (never saved yet, or an install from before this
        // feature existed) is an empty map, not an error.
        assert_eq!(
            load_hotkeys(&tmp.0.join("missing.json")),
            std::collections::HashMap::new()
        );
    }

    #[test]
    fn append_console_log_writes_a_readable_block_per_entry() {
        let tmp = TempDir::new("console-log");
        let path = tmp.0.join("console.log");

        let success = ConsoleEntry {
            timestamp: chrono::Utc::now(),
            method: crate::model::Method::Get,
            url: "https://example.com/ok".to_string(),
            status: Some(200),
            duration_ms: Some(42),
            error: None,
            script_log: vec!["hello".to_string()],
            test_results: vec![crate::model::TestResult {
                name: "status is 200".to_string(),
                passed: true,
                error: None,
            }],
        };
        append_console_log_to(&success, &path);

        let failure = ConsoleEntry {
            timestamp: chrono::Utc::now(),
            method: crate::model::Method::Post,
            url: "https://example.com/bad".to_string(),
            status: None,
            duration_ms: None,
            error: Some("connection refused".to_string()),
            script_log: vec![],
            test_results: vec![],
        };
        append_console_log_to(&failure, &path);

        let contents = std::fs::read_to_string(&path).unwrap();
        // Both entries landed in the same file, appended (not overwritten).
        assert!(contents.contains("GET https://example.com/ok"));
        assert!(contents.contains("-> 200, 42ms"));
        assert!(contents.contains("console.log: hello"));
        assert!(contents.contains("tests: 1/1"));
        assert!(contents.contains("POST https://example.com/bad"));
        assert!(contents.contains("-> ERROR: connection refused"));
    }

    #[test]
    fn settings_round_trip() {
        let tmp = TempDir::new("settings");
        let path = tmp.0.join("settings.json");
        let settings = Settings {
            proxy: crate::model::ProxyConfig {
                enabled: true,
                http_proxy: "http://127.0.0.1:8080".to_string(),
                https_proxy: String::new(),
                no_proxy: "localhost".to_string(),
            },
            tls: crate::model::TlsConfig {
                accept_invalid_certs: true,
                custom_ca_cert_path: Some("/tmp/ca.pem".to_string()),
                client_cert_path: None,
            },
            theme: crate::model::ThemeMode::Dark,
        };
        save_settings_to(&settings, &path);
        let loaded = load_settings_from(&path);
        assert_eq!(loaded.proxy.enabled, settings.proxy.enabled);
        assert_eq!(loaded.proxy.http_proxy, settings.proxy.http_proxy);
        assert_eq!(loaded.proxy.no_proxy, settings.proxy.no_proxy);
        assert_eq!(
            loaded.tls.accept_invalid_certs,
            settings.tls.accept_invalid_certs
        );
        assert_eq!(
            loaded.tls.custom_ca_cert_path,
            settings.tls.custom_ca_cert_path
        );
        assert_eq!(loaded.theme, settings.theme);

        // Missing file (never saved yet) is defaults, not an error.
        let defaults = load_settings_from(&tmp.0.join("missing.json"));
        assert!(!defaults.proxy.enabled);
        assert_eq!(defaults.theme, crate::model::ThemeMode::System);
    }

    #[test]
    fn settings_without_a_theme_key_defaults_to_system() {
        // Simulates a `settings.json` saved before Phase 11 existed: no
        // `theme` key at all.
        let tmp = TempDir::new("settings_legacy");
        let path = tmp.0.join("settings.json");
        std::fs::write(&path, r#"{"proxy":{"enabled":false,"http_proxy":"","https_proxy":"","no_proxy":""},"tls":{"accept_invalid_certs":false,"custom_ca_cert_path":null,"client_cert_path":null}}"#).unwrap();
        let loaded = load_settings_from(&path);
        assert_eq!(loaded.theme, crate::model::ThemeMode::System);
    }

    /// No network needed at all — `cookie_store::CookieStore` is a plain
    /// in-memory structure; this only exercises insert + serde round-trip.
    /// Verifies via `iter_any()` (plain enumeration) rather than `matches()`
    /// (domain/path-aware filtering, which is `reqwest`'s own concern when
    /// actually attaching cookies to a request) — that behavior was
    /// separately confirmed end-to-end with a real server in a throwaway
    /// standalone probe during development (a GET that receives a
    /// `Set-Cookie`, followed by a second GET that echoes it back
    /// correctly); this test's job is narrower — just that *our*
    /// `save_cookie_jar_to`/`load_cookie_jar_from` don't lose data.
    ///
    /// Needs an explicit `Max-Age` (or `Expires`): `cookie_store`'s own
    /// `save_json` deliberately only serializes cookies that are both
    /// unexpired *and* persistent — a session cookie (no expiry attribute)
    /// is correctly dropped on save, matching how a real browser doesn't
    /// write session cookies to disk either. Learned this the hard way
    /// (spent a while suspecting a `cookie_store` matching bug before
    /// finding this is working as intended — see `serde.rs`'s doc comment
    /// on `save`).
    #[test]
    fn cookie_jar_round_trip() {
        let tmp = TempDir::new("cookies");
        let path = tmp.0.join("cookies.json");

        let jar = CookieStoreMutex::default();
        {
            let mut store = jar.lock().unwrap();
            let url: reqwest::Url = "https://example.com/".parse().unwrap();
            store
                .parse("session=abc123; Path=/; Max-Age=3600", &url)
                .unwrap();
            assert_eq!(
                store.iter_any().count(),
                1,
                "cookie should be stored right after insert"
            );
        }
        save_cookie_jar_to(&jar, &path);

        let reloaded = load_cookie_jar_from(&path);
        let store = reloaded.lock().unwrap();
        let cookies: Vec<_> = store.iter_any().collect();
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].name(), "session");
        assert_eq!(cookies[0].value(), "abc123");
    }

    /// The flip side of the above: a cookie with no `Max-Age`/`Expires` is a
    /// session cookie, and `cookie_store` intentionally drops it on save —
    /// asserting on that here so it reads as a documented, tested behavior
    /// rather than a surprise if it's ever hit again.
    #[test]
    fn session_cookie_without_max_age_does_not_survive_save() {
        let tmp = TempDir::new("cookies-session");
        let path = tmp.0.join("cookies.json");

        let jar = CookieStoreMutex::default();
        {
            let mut store = jar.lock().unwrap();
            let url: reqwest::Url = "https://example.com/".parse().unwrap();
            store.parse("session=abc123; Path=/", &url).unwrap();
        }
        save_cookie_jar_to(&jar, &path);

        let reloaded = load_cookie_jar_from(&path);
        assert_eq!(reloaded.lock().unwrap().iter_any().count(), 0);
    }

    #[test]
    fn history_round_trips_newest_first() {
        let tmp = TempDir::new("history");
        let make_entry = |secs_ago: i64, url: &str| HistoryEntry {
            id: Uuid::new_v4(),
            timestamp: chrono::Utc::now() - chrono::Duration::seconds(secs_ago),
            method: crate::model::Method::Get,
            url: url.to_string(),
            status: Some(200),
            request: RequestItem::new(url),
            sent_request: None,
            response: None,
            error: None,
            duration_ms: Some(42),
            test_results: vec![],
        };
        let older = make_entry(60, "old");
        let newer = make_entry(0, "new");
        save_history(&[newer.clone(), older.clone()], &tmp.0);

        let loaded = load_history(&tmp.0);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].url, "new");
        assert_eq!(loaded[1].url, "old");
    }
}
