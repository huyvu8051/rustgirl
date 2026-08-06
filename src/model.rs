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

    pub fn to_reqwest(self) -> reqwest::Method {
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

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BodyMode {
    #[default]
    None,
    Raw,
    Json,
    Form,
    /// `multipart/form-data`: a mix of plain text fields and file attachments.
    Multipart,
    /// A single file sent as the entire request body.
    Binary,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FormFieldType {
    #[default]
    Text,
    File,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FormField {
    pub key: String,
    pub value: String,
    pub enabled: bool,
    pub field_type: FormFieldType,
    /// Populated (via the native file picker) when `field_type` is `File`.
    pub file_path: Option<String>,
}

impl FormField {
    pub fn new() -> Self {
        Self {
            key: String::new(),
            value: String::new(),
            enabled: true,
            field_type: FormFieldType::Text,
            file_path: None,
        }
    }
}

/// Which credentials scheme applies to a request/folder/collection.
/// `Inherit` (the default) means "use whatever the nearest ancestor with a
/// non-`Inherit` kind resolves to" — see [`resolve_effective_auth`].
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AuthKind {
    #[default]
    Inherit,
    None,
    Basic,
    Bearer,
    ApiKey,
    Digest,
    OAuth1,
    OAuth2,
    AwsSigV4,
}

/// `params` holds well-known keys per `kind`, chosen to match Postman's own
/// v2.1 collection format 1-to-1 so import/export can copy the fields
/// directly instead of translating them:
/// - `Basic`/`Digest`: `username`, `password`
/// - `Bearer`: `token`
/// - `ApiKey`: `key`, `value`, `in` (`"header"` or `"query"`)
/// - `OAuth1`: `consumerKey`, `consumerSecret`, `token`, `tokenSecret`,
///   `signatureMethod`
/// - `OAuth2`: `accessToken` (token-acquisition fields land alongside the
///   auth UI that actually drives a grant flow)
/// - `AwsSigV4`: `accessKey`, `secretKey`, `region`, `service`,
///   `sessionToken`
///
/// Not yet applied to outgoing requests — attaching auth to the request
/// builder UI and `http_client` is a later step; for now this only needs to
/// exist as data so it can round-trip through storage and (eventually)
/// Postman collection import/export.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct AuthConfig {
    pub kind: AuthKind,
    pub params: Vec<KeyValue>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct RequestBody {
    pub mode: BodyMode,
    pub raw: String,
    pub form: Vec<KeyValue>,
    #[serde(default)]
    pub multipart: Vec<FormField>,
    /// Selected file path for `BodyMode::Binary`.
    #[serde(default)]
    pub binary_file_path: Option<String>,
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
    /// Free-text documentation, shown above the Params tab — matches
    /// Postman v2.1's own per-item `description` field 1:1 (see
    /// `postman_format.rs`), so import/export round-trips it directly.
    #[serde(default)]
    pub description: String,
    /// Values for `:name` path segments in `url` (e.g. `:tenantId` in
    /// `{{baseUrl}}/:tenantId/document-baskets`). Kept in sync with the URL
    /// text by the UI; absent in JSON saved before this feature existed.
    #[serde(default)]
    pub path_params: Vec<KeyValue>,
    /// Milliseconds to wait before giving up on this specific request —
    /// `None` means "use whatever the underlying HTTP client's own default
    /// is" (today, no explicit timeout at all).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Lua script run before the request is sent; can rewrite the URL,
    /// method, headers, and (raw/JSON) body, and read/write environment
    /// variables via the `pm` table.
    #[serde(default)]
    pub pre_request_script: String,
    /// Lua script run after the response is received (or the request
    /// failed); can inspect `pm.response`, write environment variables, and
    /// record pass/fail assertions via `pm.test(name, fn)`.
    #[serde(default)]
    pub post_response_script: String,
    /// Defaults to `Inherit` (fall back to the owning folder/collection).
    #[serde(default)]
    pub auth: AuthConfig,
    /// Named snapshots of past responses ("Save Response as Example" in
    /// Postman), kept alongside the request so they survive independent of
    /// the global 200-entry history. Absent in JSON saved before this
    /// feature existed.
    #[serde(default)]
    pub saved_examples: Vec<SavedExample>,
}

/// A named, permanently-kept snapshot of one response, attached to the
/// request that produced it. Mirrors the fields `HistoryEntry` already
/// carries for "everything needed to fully replay a response" (see
/// `HistoryEntry`), minus the fields that would just duplicate the owning
/// `RequestItem` (method/url/status are all recoverable from `response`/
/// `sent_request`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SavedExample {
    pub id: Uuid,
    pub name: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub sent_request: Option<crate::http_client::SentRequest>,
    pub response: Option<crate::http_client::HttpResponse>,
    pub error: Option<String>,
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
            description: String::new(),
            path_params: vec![],
            timeout_ms: None,
            pre_request_script: String::new(),
            post_response_script: String::new(),
            auth: AuthConfig::default(),
            saved_examples: vec![],
        }
    }

    /// A copy with a fresh id (never two requests should share one — ids
    /// are relied on for lookup/selection) but the same name — used when
    /// recursing into a duplicated folder/collection, where only the
    /// container itself gets renamed.
    fn with_fresh_id(&self) -> Self {
        let mut copy = self.clone();
        copy.id = Uuid::new_v4();
        copy
    }

    /// A top-level duplicate: fresh id, name suffixed with " (Copy)".
    pub fn duplicate(&self) -> Self {
        let mut copy = self.with_fresh_id();
        copy.name = format!("{} (Copy)", self.name);
        copy
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
    /// Subfolders — folders nest arbitrarily deep, like Postman's.
    #[serde(default)]
    pub folders: Vec<Folder>,
    /// Defaults to `Inherit` (fall back to the parent folder/collection).
    #[serde(default)]
    pub auth: AuthConfig,
    /// Free-text documentation — same 1:1 mapping to Postman v2.1's own
    /// per-folder `description` as `RequestItem::description`.
    #[serde(default)]
    pub description: String,
    /// Runs before every request in this folder (and its subfolders),
    /// innermost folder last — see `Collection::folder_chain` for the
    /// ordering these run in alongside the collection's own script and the
    /// request's own script.
    #[serde(default)]
    pub pre_request_script: String,
    /// Same idea as `pre_request_script`, run after each such request's
    /// response arrives.
    #[serde(default)]
    pub post_response_script: String,
}

impl Folder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            requests: vec![],
            folders: vec![],
            auth: AuthConfig::default(),
            description: String::new(),
            pre_request_script: String::new(),
            post_response_script: String::new(),
        }
    }

    /// Deep copy with a fresh id throughout (this folder and every nested
    /// subfolder/request), name suffixed with " (Copy)".
    pub fn duplicate(&self) -> Self {
        let mut copy = self.with_fresh_ids();
        copy.name = format!("{} (Copy)", self.name);
        copy
    }

    /// Like `duplicate` but keeps the original name at every level — used
    /// when recursing into a duplicated collection, where only the
    /// collection itself is renamed.
    fn with_fresh_ids(&self) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: self.name.clone(),
            requests: self
                .requests
                .iter()
                .map(RequestItem::with_fresh_id)
                .collect(),
            folders: self.folders.iter().map(Folder::with_fresh_ids).collect(),
            auth: self.auth.clone(),
            description: self.description.clone(),
            pre_request_script: self.pre_request_script.clone(),
            post_response_script: self.post_response_script.clone(),
        }
    }

    /// Appends this folder's own requests (tagged with `path`, which is
    /// this folder's own path from the collection root) then recurses into
    /// each subfolder. Shared by `Collection::flatten_requests`/
    /// `flatten_requests_from`.
    fn flatten_requests_into(&self, path: &mut Vec<Uuid>, out: &mut Vec<(Vec<Uuid>, RequestItem)>) {
        out.extend(self.requests.iter().map(|r| (path.clone(), r.clone())));
        for child in &self.folders {
            path.push(child.id);
            child.flatten_requests_into(path, out);
            path.pop();
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Collection {
    pub id: Uuid,
    pub name: String,
    pub folders: Vec<Folder>,
    pub requests: Vec<RequestItem>,
    /// Collection-scoped variables — resolved with lower precedence than the
    /// active environment but higher than [`AppData::globals`].
    #[serde(default)]
    pub variables: Vec<KeyValue>,
    /// The collection-level fallback for any request/folder whose auth is
    /// (transitively) `Inherit`. Collections have no further parent, so this
    /// is never itself `Inherit` in practice — but the type doesn't enforce
    /// that; `resolve_effective_auth` just stops here regardless.
    #[serde(default)]
    pub auth: AuthConfig,
    /// Free-text documentation — same 1:1 mapping to Postman v2.1's own
    /// `info.description` as `RequestItem::description`/`Folder::description`.
    #[serde(default)]
    pub description: String,
    /// Runs before every request anywhere in this collection, first in the
    /// chain (collection → each folder outermost-to-innermost → the
    /// request's own script) — see `Folder::pre_request_script`.
    #[serde(default)]
    pub pre_request_script: String,
    /// Same idea as `pre_request_script` — same chain order (collection →
    /// folder outermost-to-innermost → request), run once each such
    /// request's response arrives.
    #[serde(default)]
    pub post_response_script: String,
}

impl Collection {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            folders: vec![],
            requests: vec![],
            variables: vec![],
            auth: AuthConfig::default(),
            description: String::new(),
            pre_request_script: String::new(),
            post_response_script: String::new(),
        }
    }

    /// DFS for a folder by id, returning the chain of ancestor folders from
    /// outermost to innermost (inclusive of the matched folder itself), or
    /// `None` if no folder in this collection has that id.
    // Not wired into the UI/send path yet — used by `resolve_effective_auth`
    // once the Auth tab lands and needs the ancestor chain to walk.
    #[allow(dead_code)]
    pub fn folder_chain(&self, folder_id: Uuid) -> Option<Vec<&Folder>> {
        fn search<'a>(folders: &'a [Folder], target: Uuid, path: &mut Vec<&'a Folder>) -> bool {
            for folder in folders {
                path.push(folder);
                if folder.id == target || search(&folder.folders, target, path) {
                    return true;
                }
                path.pop();
            }
            false
        }
        let mut path = Vec::new();
        search(&self.folders, folder_id, &mut path).then_some(path)
    }

    /// Recursive lookup by folder-id path (root folder list down to, and
    /// including, the target folder). Callers wanting the collection's own
    /// root lists for an *empty* path use `requests_at_mut`/`folders_at_mut`
    /// directly rather than this — there's no "root folder" to find here.
    pub fn find_folder_mut(&mut self, path: &[Uuid]) -> Option<&mut Folder> {
        find_folder_mut_in(&mut self.folders, path)
    }

    /// Every request in this collection, depth-first (root requests first,
    /// then each subfolder's own requests, recursively) — the same order the
    /// sidebar renders in. Each request is tagged with its folder path, so a
    /// caller (the Collection Runner) can resolve auth/variables for it via
    /// `effective_auth_for`/`collection_scope_variables` without needing a
    /// live `OpenTab`.
    pub fn flatten_requests(&self) -> Vec<(Vec<Uuid>, RequestItem)> {
        let mut out: Vec<_> = self
            .requests
            .iter()
            .map(|r| (Vec::new(), r.clone()))
            .collect();
        for folder in &self.folders {
            let mut path = vec![folder.id];
            folder.flatten_requests_into(&mut path, &mut out);
        }
        out
    }

    /// Same, but scoped to one folder's subtree (for "Run folder" instead of
    /// "Run collection") — `path` is that folder's own path from the root.
    /// Returns an empty `Vec` if `path` doesn't resolve to a real folder.
    pub fn flatten_requests_from(&self, path: &[Uuid]) -> Vec<(Vec<Uuid>, RequestItem)> {
        let Some(folder) = find_folder_in(&self.folders, path) else {
            return Vec::new();
        };
        let mut out: Vec<_> = folder
            .requests
            .iter()
            .map(|r| (path.to_vec(), r.clone()))
            .collect();
        for child in &folder.folders {
            let mut child_path = path.to_vec();
            child_path.push(child.id);
            child.flatten_requests_into(&mut child_path, &mut out);
        }
        out
    }

    /// The request list a folder path points at — the collection's own root
    /// requests if `path` is empty.
    pub fn requests_at_mut(&mut self, path: &[Uuid]) -> Option<&mut Vec<RequestItem>> {
        if path.is_empty() {
            Some(&mut self.requests)
        } else {
            self.find_folder_mut(path).map(|f| &mut f.requests)
        }
    }

    /// The subfolder list a folder path points at — the collection's own
    /// root folder list if `path` is empty.
    pub fn folders_at_mut(&mut self, path: &[Uuid]) -> Option<&mut Vec<Folder>> {
        if path.is_empty() {
            Some(&mut self.folders)
        } else {
            self.find_folder_mut(path).map(|f| &mut f.folders)
        }
    }

    /// Like `requests_at_mut`, but falls back to the collection's own root
    /// requests if `path` doesn't resolve (e.g. the folder it pointed at was
    /// deleted from elsewhere) instead of returning `None`.
    pub fn requests_at_mut_or_root(&mut self, path: &[Uuid]) -> &mut Vec<RequestItem> {
        if path.is_empty() {
            return &mut self.requests;
        }
        match find_folder_mut_in(&mut self.folders, path) {
            Some(folder) => &mut folder.requests,
            None => &mut self.requests,
        }
    }

    /// Removes and returns the request with `id` from `path`'s request list.
    pub fn take_request(&mut self, path: &[Uuid], id: Uuid) -> Option<RequestItem> {
        let list = self.requests_at_mut(path)?;
        let idx = list.iter().position(|r| r.id == id)?;
        Some(list.remove(idx))
    }

    /// Inserts `item` into `path`'s request list, immediately before the
    /// item with `before_id` if given and present there, else at the end. A
    /// no-op (the item is dropped) if `path` doesn't resolve to anything.
    pub fn insert_request(&mut self, path: &[Uuid], item: RequestItem, before_id: Option<Uuid>) {
        let Some(list) = self.requests_at_mut(path) else {
            return;
        };
        let idx = before_id
            .and_then(|id| list.iter().position(|r| r.id == id))
            .unwrap_or(list.len());
        list.insert(idx, item);
    }

    /// Moves a request from `from_path` to `to_path` (which may be the same
    /// path, for a pure reorder), inserting before `before_id` if given.
    /// Returns `false` (leaving the request where it was) if it wasn't found
    /// at `from_path`, or if `to_path` doesn't resolve.
    pub fn move_request(
        &mut self,
        from_path: &[Uuid],
        id: Uuid,
        to_path: &[Uuid],
        before_id: Option<Uuid>,
    ) -> bool {
        let Some(item) = self.take_request(from_path, id) else {
            return false;
        };
        if self.requests_at_mut(to_path).is_none() {
            self.insert_request(from_path, item, None); // target invalid: put it back
            return false;
        }
        self.insert_request(to_path, item, before_id);
        true
    }

    /// Removes and returns the folder with `id` from `path`'s subfolder list.
    pub fn take_folder(&mut self, path: &[Uuid], id: Uuid) -> Option<Folder> {
        let list = self.folders_at_mut(path)?;
        let idx = list.iter().position(|f| f.id == id)?;
        Some(list.remove(idx))
    }

    /// Inserts `item` into `path`'s subfolder list, immediately before the
    /// folder with `before_id` if given and present there, else at the end.
    pub fn insert_folder(&mut self, path: &[Uuid], item: Folder, before_id: Option<Uuid>) {
        let Some(list) = self.folders_at_mut(path) else {
            return;
        };
        let idx = before_id
            .and_then(|id| list.iter().position(|f| f.id == id))
            .unwrap_or(list.len());
        list.insert(idx, item);
    }

    /// Same as `move_request`, but for folders — moving a folder into one of
    /// its own descendants is naturally rejected: once removed, that
    /// subtree (and whatever `to_path` pointed inside it) can no longer be
    /// found, so `folders_at_mut(to_path)` fails to resolve and the folder
    /// is put back where it came from.
    pub fn move_folder(
        &mut self,
        from_path: &[Uuid],
        id: Uuid,
        to_path: &[Uuid],
        before_id: Option<Uuid>,
    ) -> bool {
        let Some(item) = self.take_folder(from_path, id) else {
            return false;
        };
        if self.folders_at_mut(to_path).is_none() {
            self.insert_folder(from_path, item, None);
            return false;
        }
        self.insert_folder(to_path, item, before_id);
        true
    }

    /// Deep copy with a fresh id throughout (every nested folder/request
    /// too — ids are relied on for lookup/selection and must never
    /// collide), name suffixed with " (Copy)".
    pub fn duplicate(&self) -> Self {
        let mut copy = self.with_fresh_ids();
        copy.name = format!("{} (Copy)", self.name);
        copy
    }

    /// Like `duplicate` but keeps the original name — used when recursing
    /// into a duplicated container so only the container itself is renamed,
    /// not everything inside it.
    fn with_fresh_ids(&self) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: self.name.clone(),
            folders: self.folders.iter().map(Folder::with_fresh_ids).collect(),
            requests: self
                .requests
                .iter()
                .map(RequestItem::with_fresh_id)
                .collect(),
            variables: self.variables.clone(),
            auth: self.auth.clone(),
            description: self.description.clone(),
            pre_request_script: self.pre_request_script.clone(),
            post_response_script: self.post_response_script.clone(),
        }
    }
}

fn find_folder_mut_in<'a>(folders: &'a mut [Folder], path: &[Uuid]) -> Option<&'a mut Folder> {
    let (first, rest) = path.split_first()?;
    let folder = folders.iter_mut().find(|f| f.id == *first)?;
    if rest.is_empty() {
        Some(folder)
    } else {
        find_folder_mut_in(&mut folder.folders, rest)
    }
}

/// Read-only counterpart of `find_folder_mut_in`, for callers (the
/// Collection Runner's "Run folder" target) that only need to read the
/// subtree, not mutate it.
fn find_folder_in<'a>(folders: &'a [Folder], path: &[Uuid]) -> Option<&'a Folder> {
    let (first, rest) = path.split_first()?;
    let folder = folders.iter().find(|f| f.id == *first)?;
    if rest.is_empty() {
        Some(folder)
    } else {
        find_folder_in(&folder.folders, rest)
    }
}

/// Walks the auth chain innermost-to-outermost (request, then each folder
/// from immediate parent up to the collection's own auth) and returns the
/// first non-`Inherit` config, falling back to `collection_auth` if every
/// level up to and including it is `Inherit`.
///
/// `folder_chain` must be ordered outermost-to-innermost, as returned by
/// [`Collection::folder_chain`].
// Not called yet — the Auth UI/request-sending phase applies this to the
// outgoing request; for now it's data-model-complete and unit-tested.
#[allow(dead_code)]
pub fn resolve_effective_auth<'a>(
    request_auth: &'a AuthConfig,
    folder_chain: &[&'a Folder],
    collection_auth: &'a AuthConfig,
) -> &'a AuthConfig {
    if request_auth.kind != AuthKind::Inherit {
        return request_auth;
    }
    for folder in folder_chain.iter().rev() {
        if folder.auth.kind != AuthKind::Inherit {
            return &folder.auth;
        }
    }
    collection_auth
}

/// The collection-scoped variables for a request belonging to `collection`
/// — pulled out of `App::collection_variables`'s body (Phase 8) so both the
/// tab-based send path and the Collection Runner (which has a `Collection`/
/// `folder_path`/`RequestItem` triple, not a live `OpenTab`) can share one
/// implementation.
pub fn collection_scope_variables(collection: &Collection) -> Vec<KeyValue> {
    collection.variables.clone()
}

/// The auth that actually governs a request at `folder_path` within
/// `collection`, once `Inherit` is walked up to whatever it resolves to —
/// pulled out of `App::effective_auth`'s body (Phase 8) for the same reason
/// as `collection_scope_variables` above.
pub fn effective_auth_for(
    collection: &Collection,
    folder_path: &[Uuid],
    request_auth: &AuthConfig,
) -> AuthConfig {
    let chain: Vec<&Folder> = folder_path
        .last()
        .and_then(|&fid| collection.folder_chain(fid))
        .unwrap_or_default();
    resolve_effective_auth(request_auth, &chain, &collection.auth).clone()
}

/// The `(pre_request_script, post_response_script)` pairs that should run
/// around a request at `folder_path` within `collection`, outermost first —
/// the collection's own scripts, then each folder from outermost to
/// innermost (same ordering `folder_chain` already returns). Shared by
/// `App::container_scripts` (the tab-based send path) and the Collection
/// Runner's background thread, which has a `Collection`/`folder_path` pair
/// per iteration but no live `App` to call a method on.
pub fn container_scripts_for(
    collection: &Collection,
    folder_path: &[Uuid],
) -> Vec<(String, String)> {
    let mut chain = vec![(
        collection.pre_request_script.clone(),
        collection.post_response_script.clone(),
    )];
    if let Some(&innermost) = folder_path.last()
        && let Some(folders) = collection.folder_chain(innermost)
    {
        chain.extend(
            folders
                .iter()
                .map(|f| (f.pre_request_script.clone(), f.post_response_script.clone())),
        );
    }
    chain
}

/// Resolves `{{key}}` placeholders in `text` against a precedence-ordered
/// list of variable scopes — earlier scopes win. A key present only in a
/// later (lower-precedence) scope is still substituted, just with that
/// scope's value. Disabled or empty-key entries are ignored.
///
/// Replaces the old `Environment::resolve` method now that variables can
/// come from more than one scope (environment, collection, global, and —
/// later — iteration data).
pub fn resolve_variables(text: &str, scopes: &[&[KeyValue]]) -> String {
    let mut out = text.to_string();
    let mut seen = std::collections::HashSet::new();
    for scope in scopes {
        for kv in scope.iter() {
            if !kv.enabled || kv.key.is_empty() || !seen.insert(kv.key.clone()) {
                continue;
            }
            let pattern = format!("{{{{{}}}}}", kv.key);
            out = out.replace(&pattern, &kv.value);
        }
    }
    resolve_dynamic_variables(&out)
}

/// Postman's "dynamic variables" — `{{$name}}` placeholders that don't come
/// from any static scope and are evaluated fresh on every call, so two
/// occurrences of e.g. `{{$guid}}` in the same text get two different
/// values (unlike the static substitution above, which is stable for a
/// given key). Only a fixed, common subset of Postman's real ~100-strong
/// `faker.js`-backed list is supported — see `dynamic_variable_value`.
fn resolve_dynamic_variables(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{$") {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + 2..]; // skip past "{{", keep the leading "$"
        match after_open.find("}}") {
            Some(end) => {
                let name = &after_open[..end];
                match dynamic_variable_value(name) {
                    Some(value) => out.push_str(&value),
                    // Unrecognized `{{$name}}` — left untouched, same as an
                    // unresolved static variable would be.
                    None => out.push_str(&format!("{{{{{name}}}}}")),
                }
                rest = &after_open[end + 2..];
            }
            None => {
                // Unterminated `{{$...` with no closing `}}` — not a real
                // placeholder, leave the rest of the text as-is.
                out.push_str(&rest[start..]);
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

fn dynamic_variable_value(name: &str) -> Option<String> {
    use rand::RngExt;
    use rand::seq::IndexedRandom;

    const FIRST_NAMES: [&str; 10] = [
        "James",
        "Mary",
        "Robert",
        "Patricia",
        "John",
        "Jennifer",
        "Michael",
        "Linda",
        "William",
        "Elizabeth",
    ];
    const LAST_NAMES: [&str; 10] = [
        "Smith",
        "Johnson",
        "Williams",
        "Brown",
        "Jones",
        "Garcia",
        "Miller",
        "Davis",
        "Rodriguez",
        "Martinez",
    ];
    const WORDS: [&str; 12] = [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
        "juliet", "kilo", "lima",
    ];
    const COLORS: [&str; 8] = [
        "red", "orange", "yellow", "green", "blue", "indigo", "violet", "black",
    ];

    let mut rng = rand::rng();
    match name {
        "$guid" | "$randomUUID" => Some(Uuid::new_v4().to_string()),
        "$timestamp" => Some(chrono::Utc::now().timestamp().to_string()),
        "$isoTimestamp" => Some(chrono::Utc::now().to_rfc3339()),
        "$randomInt" => Some(rng.random_range(0..1000).to_string()),
        "$randomBoolean" => Some(rng.random::<bool>().to_string()),
        "$randomColor" => COLORS.choose(&mut rng).map(|s| s.to_string()),
        "$randomIP" => Some(
            (0..4)
                .map(|_| rng.random_range(0..=255u32).to_string())
                .collect::<Vec<_>>()
                .join("."),
        ),
        "$randomFirstName" => FIRST_NAMES.choose(&mut rng).map(|s| s.to_string()),
        "$randomLastName" => LAST_NAMES.choose(&mut rng).map(|s| s.to_string()),
        "$randomFullName" => Some(format!(
            "{} {}",
            FIRST_NAMES.choose(&mut rng)?,
            LAST_NAMES.choose(&mut rng)?
        )),
        "$randomEmail" => Some(format!(
            "{}.{}@example.com",
            FIRST_NAMES.choose(&mut rng)?.to_lowercase(),
            LAST_NAMES.choose(&mut rng)?.to_lowercase()
        )),
        "$randomWord" => WORDS.choose(&mut rng).map(|s| s.to_string()),
        "$randomWords" => Some(
            (0..3)
                .filter_map(|_| WORDS.choose(&mut rng))
                .copied()
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
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
}

/// A single `pm.test(name, fn)` assertion from a post-response script: `fn`
/// ran without raising an error (`passed`), or `error` holds what it raised.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HistoryEntry {
    pub id: Uuid,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub method: Method,
    pub url: String,
    pub status: Option<u16>,
    /// The request's definition as edited (unresolved `{{variable}}`
    /// placeholders) — reloaded into the editor when this entry is clicked.
    pub request: RequestItem,
    /// The fully-resolved request actually sent on the wire. Absent in
    /// history saved before this field existed.
    #[serde(default)]
    pub sent_request: Option<crate::http_client::SentRequest>,
    /// The full response received, when the request succeeded.
    #[serde(default)]
    pub response: Option<crate::http_client::HttpResponse>,
    /// The error message, when the request failed instead of completing.
    #[serde(default)]
    pub error: Option<String>,
    /// How long the request was in flight, whether it succeeded or failed.
    /// `None` on failures that happened before anything was sent (e.g. an
    /// empty URL).
    #[serde(default)]
    pub duration_ms: Option<u128>,
    /// Results of any `pm.test(...)` assertions from the post-response
    /// script that ran for this entry.
    #[serde(default)]
    pub test_results: Vec<TestResult>,
}

/// Client-level configuration — proxy/TLS — persisted separately from
/// `AppData` (`storage::settings_file`) since it configures the HTTP
/// client itself rather than being request/collection state. Not part of
/// any request's data; there's exactly one, app-wide.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Settings {
    pub proxy: ProxyConfig,
    pub tls: TlsConfig,
    /// Absent in `settings.json` files saved before this feature existed —
    /// defaults to `System`, matching egui's own implicit pre-Phase-11
    /// behavior (no explicit override), so nothing changes for existing
    /// users until they actually pick something.
    #[serde(default)]
    pub theme: ThemeMode,
}

/// A local, serializable mirror of `egui::ThemePreference` — that egui type
/// doesn't derive `Serialize`/`Deserialize` in this build (the `egui`
/// dependency doesn't have the `serde` feature on), and a 3-variant enum
/// isn't worth turning that on crate-wide for. `app.rs` maps this to the
/// real `egui::ThemePreference` when calling `Context::set_theme`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeMode {
    Light,
    Dark,
    #[default]
    System,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 3] = [ThemeMode::Light, ThemeMode::Dark, ThemeMode::System];

    pub fn label(&self) -> &'static str {
        match self {
            ThemeMode::Light => "Light",
            ThemeMode::Dark => "Dark",
            ThemeMode::System => "System",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ProxyConfig {
    pub enabled: bool,
    /// Accepts `http://`, `https://`, or `socks5://` URLs — reqwest parses
    /// the scheme itself, so one field naturally covers all three schemes
    /// without a separate per-scheme toggle.
    pub http_proxy: String,
    pub https_proxy: String,
    /// Comma-separated hostnames to bypass the proxy for.
    pub no_proxy: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct TlsConfig {
    /// The "danger" toggle — UI-gated behind a visible warning.
    pub accept_invalid_certs: bool,
    /// PEM file path, merged *into* the existing trust store (not a
    /// replacement) — trusting an additional internal CA shouldn't also
    /// revoke trust in public ones.
    pub custom_ca_cert_path: Option<String>,
    /// Combined cert+key PEM file path, for mTLS client authentication.
    pub client_cert_path: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AppData {
    pub collections: Vec<Collection>,
    pub environments: Vec<Environment>,
    pub history: Vec<HistoryEntry>,
    /// Variables available everywhere, regardless of active environment or
    /// collection — lowest precedence in [`resolve_variables`].
    #[serde(default)]
    pub globals: Vec<KeyValue>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(key: &str, value: &str) -> KeyValue {
        KeyValue {
            key: key.to_string(),
            value: value.to_string(),
            enabled: true,
        }
    }

    #[test]
    fn resolve_variables_prefers_earlier_scope_on_key_collision() {
        let env = vec![kv("baseUrl", "https://env.example.com")];
        let collection = vec![
            kv("baseUrl", "https://collection.example.com"),
            kv("token", "collection-token"),
        ];
        let globals = vec![
            kv("token", "global-token"),
            kv("onlyGlobal", "global-value"),
        ];

        let resolved = resolve_variables(
            "{{baseUrl}}/x?token={{token}}&g={{onlyGlobal}}",
            &[&env, &collection, &globals],
        );
        assert_eq!(
            resolved,
            "https://env.example.com/x?token=collection-token&g=global-value"
        );
    }

    #[test]
    fn resolve_variables_skips_disabled_and_empty_key_entries() {
        let scope = vec![
            KeyValue {
                key: "a".to_string(),
                value: "should-not-appear".to_string(),
                enabled: false,
            },
            KeyValue {
                key: String::new(),
                value: "ignored".to_string(),
                enabled: true,
            },
        ];
        assert_eq!(resolve_variables("{{a}}", &[&scope]), "{{a}}");
    }

    #[test]
    fn dynamic_variable_guid_looks_like_a_uuid() {
        let resolved = resolve_variables("{{$guid}}", &[]);
        assert!(
            Uuid::parse_str(&resolved).is_ok(),
            "{resolved:?} should parse as a UUID"
        );
    }

    #[test]
    fn dynamic_variable_timestamp_is_a_plausible_unix_time() {
        let resolved = resolve_variables("{{$timestamp}}", &[]);
        let value: i64 = resolved.parse().expect("should be an integer");
        // Any time after 2020-01-01 is a reasonable sanity floor.
        assert!(
            value > 1_577_836_800,
            "{value} doesn't look like a real Unix timestamp"
        );
    }

    #[test]
    fn dynamic_variables_are_evaluated_fresh_per_occurrence() {
        let resolved = resolve_variables("{{$guid}}-{{$guid}}", &[]);
        let (a, b) = resolved.split_once('-').unwrap();
        assert_ne!(
            a, b,
            "each {{{{$guid}}}} occurrence should get its own value"
        );
    }

    #[test]
    fn dynamic_variable_random_int_is_in_range() {
        let resolved = resolve_variables("{{$randomInt}}", &[]);
        let value: i64 = resolved.parse().expect("should be an integer");
        assert!((0..1000).contains(&value));
    }

    #[test]
    fn unrecognized_dynamic_variable_is_left_untouched() {
        assert_eq!(
            resolve_variables("{{$notARealOne}}", &[]),
            "{{$notARealOne}}"
        );
    }

    #[test]
    fn static_scope_takes_precedence_before_the_dynamic_pass_even_runs() {
        // A static variable literally named like a dynamic one still wins —
        // the dynamic pass only ever sees whatever `{{$...}}` text is left
        // over after static substitution.
        let scope = vec![kv("$guid", "not-actually-a-guid")];
        assert_eq!(
            resolve_variables("{{$guid}}", &[&scope]),
            "not-actually-a-guid"
        );
    }

    fn auth(kind: AuthKind) -> AuthConfig {
        AuthConfig {
            kind,
            params: vec![],
        }
    }

    #[test]
    fn resolve_effective_auth_prefers_request_over_ancestors() {
        let request_auth = auth(AuthKind::Bearer);
        let folder = Folder::new("f"); // auth: Inherit
        let collection_auth = auth(AuthKind::Basic);
        let result = resolve_effective_auth(&request_auth, &[&folder], &collection_auth);
        assert_eq!(result.kind, AuthKind::Bearer);
    }

    #[test]
    fn resolve_effective_auth_falls_back_to_nearest_non_inherit_folder() {
        let request_auth = auth(AuthKind::Inherit);
        let mut outer = Folder::new("outer");
        outer.auth = auth(AuthKind::Digest);
        let mut inner = Folder::new("inner"); // auth: Inherit
        inner.auth = auth(AuthKind::Inherit);
        let collection_auth = auth(AuthKind::Basic);

        // Chain ordered outermost -> innermost, as `Collection::folder_chain` returns it.
        let result = resolve_effective_auth(&request_auth, &[&outer, &inner], &collection_auth);
        assert_eq!(result.kind, AuthKind::Digest);
    }

    #[test]
    fn resolve_effective_auth_falls_back_to_collection_when_everything_inherits() {
        let request_auth = auth(AuthKind::Inherit);
        let folder = Folder::new("f");
        let collection_auth = auth(AuthKind::AwsSigV4);
        let result = resolve_effective_auth(&request_auth, &[&folder], &collection_auth);
        assert_eq!(result.kind, AuthKind::AwsSigV4);
    }

    #[test]
    fn folder_chain_finds_nested_folder_outermost_to_innermost() {
        let mut collection = Collection::new("My API");
        let mut outer = Folder::new("outer");
        let inner = Folder::new("inner");
        let inner_id = inner.id;
        outer.folders.push(inner);
        let outer_id = outer.id;
        collection.folders.push(outer);

        let chain = collection.folder_chain(inner_id).expect("chain found");
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].id, outer_id);
        assert_eq!(chain[1].id, inner_id);

        assert!(collection.folder_chain(Uuid::new_v4()).is_none());
    }

    /// Builds: collection { requests: [root], folders: [outer { requests:
    /// [in_outer], folders: [inner { requests: [in_inner] }] }] }.
    fn nested_fixture() -> (Collection, Uuid, Uuid, Uuid, Uuid, Uuid) {
        let mut collection = Collection::new("My API");
        let root_req = RequestItem::new("Root Request");
        let root_req_id = root_req.id;
        collection.requests.push(root_req);

        let mut inner = Folder::new("Inner");
        let inner_req = RequestItem::new("Inner Request");
        let inner_req_id = inner_req.id;
        inner.requests.push(inner_req);
        let inner_id = inner.id;

        let mut outer = Folder::new("Outer");
        let outer_req = RequestItem::new("Outer Request");
        let outer_req_id = outer_req.id;
        outer.requests.push(outer_req);
        outer.folders.push(inner);
        let outer_id = outer.id;

        collection.folders.push(outer);
        (
            collection,
            outer_id,
            inner_id,
            root_req_id,
            outer_req_id,
            inner_req_id,
        )
    }

    #[test]
    fn requests_at_mut_and_folders_at_mut_resolve_at_every_depth() {
        let (mut collection, outer_id, inner_id, root_req_id, outer_req_id, inner_req_id) =
            nested_fixture();

        assert_eq!(collection.requests_at_mut(&[]).unwrap()[0].id, root_req_id);
        assert_eq!(
            collection.requests_at_mut(&[outer_id]).unwrap()[0].id,
            outer_req_id
        );
        assert_eq!(
            collection.requests_at_mut(&[outer_id, inner_id]).unwrap()[0].id,
            inner_req_id
        );
        assert_eq!(collection.folders_at_mut(&[]).unwrap()[0].id, outer_id);
        assert_eq!(
            collection.folders_at_mut(&[outer_id]).unwrap()[0].id,
            inner_id
        );

        // A path through a nonexistent folder resolves to nothing.
        assert!(collection.requests_at_mut(&[Uuid::new_v4()]).is_none());
    }

    #[test]
    fn move_request_reorders_within_the_same_list() {
        let (mut collection, ..) = nested_fixture();
        let a = RequestItem::new("A");
        let a_id = a.id;
        collection.requests.push(a);
        let b_id = collection.requests[0].id; // "Root Request", inserted first

        // Move A before B (the original root request).
        assert!(collection.move_request(&[], a_id, &[], Some(b_id)));
        assert_eq!(collection.requests[0].id, a_id);
        assert_eq!(collection.requests[1].id, b_id);
    }

    #[test]
    fn move_request_moves_across_folders() {
        let (mut collection, outer_id, inner_id, root_req_id, ..) = nested_fixture();
        assert!(collection.move_request(&[], root_req_id, &[outer_id, inner_id], None));

        assert!(collection.requests_at_mut(&[]).unwrap().is_empty());
        let inner_requests = collection.requests_at_mut(&[outer_id, inner_id]).unwrap();
        assert!(inner_requests.iter().any(|r| r.id == root_req_id));
    }

    #[test]
    fn move_request_to_nonexistent_target_is_rejected_and_original_untouched() {
        let (mut collection, .., root_req_id, _, _) = nested_fixture();
        let bogus = vec![Uuid::new_v4()];
        assert!(!collection.move_request(&[], root_req_id, &bogus, None));
        // Still exactly where it started.
        assert_eq!(collection.requests_at_mut(&[]).unwrap()[0].id, root_req_id);
    }

    #[test]
    fn move_folder_into_its_own_descendant_is_rejected_and_rolled_back() {
        let (mut collection, outer_id, inner_id, ..) = nested_fixture();
        // Try to move "Outer" to become a child of "Inner" (its own child).
        let moved = collection.move_folder(&[], outer_id, &[outer_id, inner_id], None);
        assert!(!moved);
        // Outer is still at the root, with Inner still inside it.
        let root_folders = collection.folders_at_mut(&[]).unwrap();
        assert_eq!(root_folders.len(), 1);
        assert_eq!(root_folders[0].id, outer_id);
        assert_eq!(
            collection.folders_at_mut(&[outer_id]).unwrap()[0].id,
            inner_id
        );
    }

    #[test]
    fn move_folder_relocates_into_a_different_folder() {
        let mut collection = Collection::new("My API");
        let folder_a = Folder::new("A");
        let a_id = folder_a.id;
        let folder_b = Folder::new("B");
        let b_id = folder_b.id;
        collection.folders.push(folder_a);
        collection.folders.push(folder_b);

        assert!(collection.move_folder(&[], a_id, &[b_id], None));
        assert!(
            collection
                .folders_at_mut(&[])
                .unwrap()
                .iter()
                .all(|f| f.id != a_id)
        );
        assert_eq!(collection.folders_at_mut(&[b_id]).unwrap()[0].id, a_id);
    }

    #[test]
    fn request_duplicate_has_fresh_id_and_copy_suffix() {
        let original = RequestItem::new("Login");
        let copy = original.duplicate();
        assert_ne!(copy.id, original.id);
        assert_eq!(copy.name, "Login (Copy)");
        assert_eq!(copy.url, original.url);
    }

    #[test]
    fn folder_duplicate_regenerates_every_nested_id_without_renaming_children() {
        let (collection, outer_id, inner_id, _, outer_req_id, inner_req_id) = nested_fixture();
        let outer = collection
            .folders
            .iter()
            .find(|f| f.id == outer_id)
            .unwrap();
        let copy = outer.duplicate();

        assert_ne!(copy.id, outer_id);
        assert_eq!(copy.name, "Outer (Copy)");
        // Children keep their original names...
        assert_eq!(copy.requests[0].name, "Outer Request");
        assert_eq!(copy.folders[0].name, "Inner");
        // ...but every id underneath is fresh, colliding with nothing original.
        assert_ne!(copy.requests[0].id, outer_req_id);
        assert_ne!(copy.folders[0].id, inner_id);
        assert_ne!(copy.folders[0].requests[0].id, inner_req_id);
    }

    #[test]
    fn collection_duplicate_regenerates_every_nested_id_without_renaming_children() {
        let (collection, outer_id, inner_id, root_req_id, outer_req_id, inner_req_id) =
            nested_fixture();
        let copy = collection.duplicate();

        assert_ne!(copy.id, collection.id);
        assert_eq!(copy.name, "My API (Copy)");
        assert_eq!(copy.requests[0].name, "Root Request");
        assert_ne!(copy.requests[0].id, root_req_id);
        assert_ne!(copy.folders[0].id, outer_id);
        assert_ne!(copy.folders[0].requests[0].id, outer_req_id);
        assert_ne!(copy.folders[0].folders[0].id, inner_id);
        assert_ne!(copy.folders[0].folders[0].requests[0].id, inner_req_id);
    }

    #[test]
    fn request_item_with_saved_examples_round_trips_through_json() {
        let mut req = RequestItem::new("Get Widget");
        req.saved_examples.push(SavedExample {
            id: Uuid::new_v4(),
            name: "200 OK".to_string(),
            timestamp: chrono::Utc::now(),
            sent_request: Some(crate::http_client::SentRequest {
                method: "GET".to_string(),
                url: "https://example.com/widget".to_string(),
                headers: vec![("Accept".to_string(), "application/json".to_string())],
                body: None,
            }),
            response: Some(crate::http_client::HttpResponse {
                status: 200,
                status_text: "OK".to_string(),
                headers: vec![],
                body: "{\"ok\":true}".to_string(),
                duration_ms: 42,
                size_bytes: 12,
                // `#[serde(skip)]`: deserializing back always produces
                // `Vec::new()` regardless of what's set here, so this needs
                // to already be empty for the round-trip `assert_eq!` below
                // to hold.
                raw_bytes: Vec::new(),
            }),
            error: None,
        });

        let json = serde_json::to_string(&req).unwrap();
        let restored: RequestItem = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, req);
        assert_eq!(restored.saved_examples[0].name, "200 OK");
        assert_eq!(
            restored.saved_examples[0].response.as_ref().unwrap().status,
            200
        );
    }

    #[test]
    fn request_item_without_saved_examples_field_defaults_to_empty() {
        // Simulates JSON saved before this feature existed: serialize a real
        // request, then strip the `saved_examples` key before parsing back.
        let req = RequestItem::new("Legacy");
        let mut value = serde_json::to_value(&req).unwrap();
        value.as_object_mut().unwrap().remove("saved_examples");
        let restored: RequestItem = serde_json::from_value(value).unwrap();
        assert!(restored.saved_examples.is_empty());
    }

    #[test]
    fn flatten_requests_visits_root_then_each_folder_depth_first() {
        let (collection, outer_id, inner_id, root_req_id, outer_req_id, inner_req_id) =
            nested_fixture();
        let flat = collection.flatten_requests();

        assert_eq!(flat.len(), 3);
        assert_eq!(flat[0], (Vec::new(), collection.requests[0].clone()));
        assert_eq!(flat[0].1.id, root_req_id);
        assert_eq!(flat[1].0, vec![outer_id]);
        assert_eq!(flat[1].1.id, outer_req_id);
        assert_eq!(flat[2].0, vec![outer_id, inner_id]);
        assert_eq!(flat[2].1.id, inner_req_id);
    }

    #[test]
    fn flatten_requests_from_scopes_to_one_folders_subtree() {
        let (collection, outer_id, inner_id, _, outer_req_id, inner_req_id) = nested_fixture();

        // Scoped to "Outer": its own request plus everything in "Inner"
        // beneath it, but not the collection's root-level request.
        let flat = collection.flatten_requests_from(&[outer_id]);
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[0].0, vec![outer_id]);
        assert_eq!(flat[0].1.id, outer_req_id);
        assert_eq!(flat[1].0, vec![outer_id, inner_id]);
        assert_eq!(flat[1].1.id, inner_req_id);

        // Scoped to "Inner": just its own one request.
        let flat_inner = collection.flatten_requests_from(&[outer_id, inner_id]);
        assert_eq!(flat_inner.len(), 1);
        assert_eq!(flat_inner[0].1.id, inner_req_id);

        // A nonexistent path resolves to nothing rather than panicking.
        assert!(
            collection
                .flatten_requests_from(&[Uuid::new_v4()])
                .is_empty()
        );
    }

    #[test]
    fn effective_auth_for_and_collection_scope_variables_match_the_existing_resolution_logic() {
        let mut collection = Collection::new("My API");
        collection.variables.push(KeyValue {
            key: "base".to_string(),
            value: "https://api.example.com".to_string(),
            enabled: true,
        });
        let mut folder = Folder::new("Auth'd");
        folder.auth = AuthConfig {
            kind: AuthKind::Bearer,
            params: vec![KeyValue {
                key: "token".to_string(),
                value: "abc123".to_string(),
                enabled: true,
            }],
        };
        let folder_id = folder.id;
        collection.folders.push(folder);

        assert_eq!(
            collection_scope_variables(&collection),
            collection.variables
        );

        // The request's own auth is `Inherit`, so it should resolve to the
        // folder's Bearer config.
        let request_auth = AuthConfig::default();
        let resolved = effective_auth_for(&collection, &[folder_id], &request_auth);
        assert_eq!(resolved.kind, AuthKind::Bearer);

        // Empty folder_path (a root-level request) falls back to the
        // collection's own auth (`Inherit` by default here, same as the
        // request's), matching `resolve_effective_auth`'s own fallback.
        let root_resolved = effective_auth_for(&collection, &[], &request_auth);
        assert_eq!(root_resolved.kind, AuthKind::Inherit);
    }

    /// Phase 13b: `container_scripts_for` returns the collection's own
    /// scripts first, then each ancestor folder outermost-to-innermost —
    /// the same chain order `App`'s script-chain callers run scripts in
    /// (see `chained_pre_request_scripts_share_the_same_environment` in
    /// `scripting.rs` for the actual execution-order behavior this ordering
    /// enables).
    #[test]
    fn container_scripts_for_orders_collection_then_folders_outermost_first() {
        let mut collection = Collection::new("My API");
        collection.pre_request_script = "-- collection".to_string();
        collection.post_response_script = "-- collection post".to_string();

        let mut inner = Folder::new("Tokens");
        inner.pre_request_script = "-- inner".to_string();
        inner.post_response_script = "-- inner post".to_string();
        let inner_id = inner.id;

        let mut outer = Folder::new("Auth");
        outer.pre_request_script = "-- outer".to_string();
        outer.post_response_script = "-- outer post".to_string();
        outer.folders.push(inner);
        let outer_id = outer.id;

        collection.folders.push(outer);

        // Root-level request: only the collection's own scripts apply.
        assert_eq!(
            container_scripts_for(&collection, &[]),
            vec![(
                "-- collection".to_string(),
                "-- collection post".to_string()
            )]
        );

        // A request directly in the outer folder: collection, then outer.
        assert_eq!(
            container_scripts_for(&collection, &[outer_id]),
            vec![
                (
                    "-- collection".to_string(),
                    "-- collection post".to_string()
                ),
                ("-- outer".to_string(), "-- outer post".to_string()),
            ]
        );

        // A request in the innermost folder: collection, outer, inner.
        assert_eq!(
            container_scripts_for(&collection, &[outer_id, inner_id]),
            vec![
                (
                    "-- collection".to_string(),
                    "-- collection post".to_string()
                ),
                ("-- outer".to_string(), "-- outer post".to_string()),
                ("-- inner".to_string(), "-- inner post".to_string()),
            ]
        );
    }
}
