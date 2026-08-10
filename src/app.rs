use crate::codegen;
use crate::curl_import;
use crate::http_client::{self, HttpResponse, RequestOutcome, SentRequest};
use crate::model::{
    self, AppData, AuthConfig, AuthKind, BodyMode, Collection, ConsoleEntry, Environment, Folder,
    FormField, FormFieldType, HistoryEntry, KeyValue, Method, RequestItem, SavedExample, Settings,
    TestResult, ThemeMode,
};
use crate::openapi_import;
use crate::postman_format;
use crate::scripting;
use crate::storage;
use crate::syntax;
use crate::vim_editor;
use eframe::egui;
use reqwest_cookie_store::CookieStoreMutex;
use std::sync::mpsc::{Receiver, Sender};
use uuid::Uuid;

#[derive(PartialEq, Clone, Copy)]
enum SidebarTab {
    Collections,
    Environments,
    History,
}

#[derive(PartialEq, Clone, Copy)]
enum RequestTab {
    Params,
    Headers,
    Auth,
    Body,
    PreRequestScript,
    TestsScript,
    Code,
}

#[derive(PartialEq, Clone, Copy)]
enum ResponseTab {
    Body,
    Headers,
    Request,
    TestResults,
    Cookies,
    Diff,
}

#[derive(Clone)]
enum CentralView {
    Request,
    EnvironmentEditor(Uuid),
    Settings,
    Runner,
    CollectionEditor(Uuid),
    /// `folder_path` is the chain of folder ids from the collection's root
    /// down to the folder being edited — same convention as
    /// `RequestOrigin::Collection`'s own `folder_path`.
    FolderEditor {
        collection: Uuid,
        folder_path: Vec<Uuid>,
    },
}

#[derive(PartialEq, Clone, Copy)]
enum SplitDirection {
    /// Request editor on top, response below.
    Vertical,
    /// Request editor on the left, response on the right.
    Horizontal,
}

/// Where the currently-loaded request came from, so "Save" knows what to overwrite.
#[derive(Clone)]
enum RequestOrigin {
    Unsaved,
    /// `folder_path` is the chain of folder ids from the collection's root
    /// down to the folder directly containing `request` — empty means the
    /// request sits directly under the collection.
    Collection {
        collection: Uuid,
        folder_path: Vec<Uuid>,
        request: Uuid,
    },
}

/// Dragged from a sidebar row — carried by egui's own drag-and-drop
/// (`Ui::dnd_drag_source`/`dnd_drop_zone`) so a drop zone can tell what's
/// being dropped on it and where it came from.
#[derive(Clone)]
enum DragPayload {
    Request {
        collection: Uuid,
        folder_path: Vec<Uuid>,
        id: Uuid,
    },
    Folder {
        collection: Uuid,
        parent_path: Vec<Uuid>,
        id: Uuid,
    },
}

/// A side effect triggered while rendering the (recursive) collection tree,
/// collected during the render pass and applied once it's done — the same
/// "collect then apply" pattern the sidebar already used for single-level
/// delete/load, generalized so one recursive render function can handle
/// every depth without fighting the borrow checker over `self.data`.
enum PendingAction {
    /// Loads a request into a tab — carries just the id (not a cloned
    /// `RequestItem`, unlike before the tree-virtualization rewrite): row
    /// rendering now only ever has a flattened `model::Row` on hand, not a
    /// live `&RequestItem`, so the actual `RequestItem` is looked up by id
    /// from `self.data.collections` once the action is applied.
    Load {
        collection: Uuid,
        folder_path: Vec<Uuid>,
        id: Uuid,
    },
    AddRequest {
        collection: Uuid,
        folder_path: Vec<Uuid>,
    },
    AddFolder {
        collection: Uuid,
        folder_path: Vec<Uuid>,
    },
    DeleteRequest {
        collection: Uuid,
        folder_path: Vec<Uuid>,
        id: Uuid,
    },
    DeleteFolder {
        collection: Uuid,
        parent_path: Vec<Uuid>,
        id: Uuid,
    },
    DuplicateRequest {
        collection: Uuid,
        folder_path: Vec<Uuid>,
        id: Uuid,
    },
    DuplicateFolder {
        collection: Uuid,
        parent_path: Vec<Uuid>,
        id: Uuid,
    },
    /// Dispatched by ID search across collections/folders/requests (see
    /// `App::apply_rename`) rather than carrying a path — simpler than
    /// threading an item-kind tag through the recursive renderer just for
    /// this one action.
    Rename {
        id: Uuid,
        new_name: String,
    },
    MoveRequest {
        collection: Uuid,
        from_path: Vec<Uuid>,
        id: Uuid,
        to_path: Vec<Uuid>,
        before_id: Option<Uuid>,
    },
    MoveFolder {
        collection: Uuid,
        from_path: Vec<Uuid>,
        id: Uuid,
        to_path: Vec<Uuid>,
        before_id: Option<Uuid>,
    },
    /// Opens the Collection Runner scoped to one folder's subtree.
    RunFolder {
        collection: Uuid,
        folder_path: Vec<Uuid>,
        name: String,
    },
    /// Opens the folder editor (description + pre-request/test scripts).
    EditFolder {
        collection: Uuid,
        folder_path: Vec<Uuid>,
    },
    /// Collection-level counterparts of the folder/request actions above —
    /// folded into the same `PendingAction`/`actions: Vec<PendingAction>`
    /// accumulator (rather than the half-dozen separate `Option<Uuid>`
    /// locals `collections_sidebar` used before the tree-virtualization
    /// rewrite) since the new flat `render_tree_row` handles every row kind
    /// in one function and pushes into one shared `actions` list either way.
    DeleteCollection(Uuid),
    DuplicateCollection(Uuid),
    EditCollection(Uuid),
    ExportCollection(Uuid),
    RunCollection {
        collection: Uuid,
        name: String,
    },
    /// Starts the "press a key to assign" capture flow (`App::
    /// pending_hotkey_assignment`) for one request.
    AssignHotkey(Uuid),
}

/// One leader-key chord `fn ui` recognizes (see `LEADER_CHORDS`) — a table
/// rather than a single hardcoded check so a future chord costs one more
/// entry, not new plumbing.
#[derive(Clone, Copy)]
enum LeaderAction {
    /// Requests-only ("search files").
    OpenPalette,
    /// Opens the command palette pre-filtered to the "Environment: ..."
    /// entries (`palette_actions`' own label format) — a quick "pick an
    /// environment" flow reusing the palette's existing fuzzy list/Ctrl+N/P
    /// selection rather than a second, bespoke picker widget.
    SelectEnvironment,
    /// Commands-only, no requests — the counterpart to `OpenPalette`'s
    /// requests-only view, for jumping straight to an action by name.
    OpenCommandPalette,
    /// Starts hotkey assignment for the *active tab's* request — a
    /// keyboard-only alternative to the sidebar's "Assign hotkey…" context-
    /// menu entry, for when the request is already open rather than
    /// visible in the sidebar tree.
    AssignHotkeyToActiveRequest,
    /// Keyboard-only alternative to the top bar's "Hide/Show Sidebar"
    /// button — same `toggle_sidebar` a manual click uses.
    ToggleSidebar,
}

/// Leader-key sequences, checked only while nothing has keyboard focus
/// (Space still types a literal space in every text field otherwise) —
/// the command palette's *only* trigger now, replacing the old Alt+Space/
/// Cmd+K bindings per explicit user preference (neovim-style `<leader>sf`,
/// not a plain shortcut). The leading space is part of the stored match
/// string purely for readability. The third element is a short
/// human-readable label shown by the which-key-style hint popup
/// (`leader_which_key_hint`) while a chord is in progress.
const LEADER_CHORDS: &[(&str, LeaderAction, &str)] = &[
    (" sf", LeaderAction::OpenPalette, "Search requests"),
    (" se", LeaderAction::SelectEnvironment, "Select environment"),
    (" sc", LeaderAction::OpenCommandPalette, "Search commands"),
    (
        " ah",
        LeaderAction::AssignHotkeyToActiveRequest,
        "Assign hotkey to active request",
    ),
    (" us", LeaderAction::ToggleSidebar, "Toggle sidebar"),
];

/// What the command palette's own list is narrowed to — set by which
/// leader chord opened it (see `LeaderAction`).
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum PaletteScope {
    #[default]
    Combined,
    RequestsOnly,
    CommandsOnly,
}

/// A non-request action offered by the command palette
/// (`App::command_palette`) — every variant is a thin wrapper over a field
/// assignment that already exists elsewhere (a button, a shortcut), not new
/// behavior.
#[derive(Clone, Copy)]
enum PaletteAction {
    NewTab,
    CloseActiveTab,
    SaveActiveRequest,
    OpenSettings,
    OpenRunner,
    SwitchSidebarTab(SidebarTab),
    /// `None` = "No Environment".
    SwitchEnvironment(Option<Uuid>),
    ToggleSplitDirection,
    SetTheme(ThemeMode),
}

/// One row in the command palette — either a matching request (opens it,
/// same as the old request-only quick-open) or a matching `PaletteAction`.
#[derive(Clone)]
enum PaletteEntry {
    Request {
        collection: Uuid,
        folder_path: Vec<Uuid>,
        // Boxed per clippy's `large_enum_variant` — same reasoning as
        // `PendingAction::Load`.
        item: Box<RequestItem>,
    },
    Action {
        label: String,
        action: PaletteAction,
    },
}

/// One finished request within a Collection Runner run — deliberately not
/// `HistoryEntry`: Runner runs are ephemeral (never written to
/// `AppData.history`, which would blow through its 200-entry cap on any
/// nontrivial run) and don't need the full request/sent_request snapshot,
/// just enough to render a results row and tally the summary.
#[derive(Clone, Debug)]
struct RunnerResult {
    iteration: usize,
    method: Method,
    name: String,
    status: Option<u16>,
    duration_ms: Option<u128>,
    test_results: Vec<TestResult>,
    error: Option<String>,
}

/// Sent from the Runner's background thread (see `App::start_run`) back to
/// the UI thread, polled the same way `poll_responses` drains `self.rx`.
enum RunnerEvent {
    RequestFinished { run_id: Uuid, result: RunnerResult },
    RunFinished { run_id: Uuid, cancelled: bool },
}

/// Collection Runner configuration + (while a run is active or just
/// finished) its progress and results. Always present on `App` — an
/// unconfigured Runner panel (no target picked yet) is its own valid
/// display, same convention as `Settings`. Not persisted to disk: this is
/// session-only UI/run state, not request/collection data.
struct RunnerState {
    /// `None` until a "Run"/"Run folder" action picks a target.
    collection: Option<Uuid>,
    /// Empty = whole collection; non-empty = a specific folder's subtree.
    folder_path: Vec<Uuid>,
    /// Collection/folder name, captured at target-pick time for the panel
    /// header — avoids re-resolving `collection`/`folder_path` against
    /// `self.data` (which could itself have been deleted mid-run) just to
    /// render a label.
    target_label: String,

    iterations: usize,
    delay_ms: u64,
    data_file_path: Option<String>,
    /// Parsed CSV/JSON rows — empty if no data file is loaded, in which
    /// case `iterations` governs the run instead.
    data_rows: Vec<Vec<KeyValue>>,

    /// `Some` while a run is in flight; events whose `run_id` doesn't match
    /// this are from a stale/cancelled previous run and are dropped.
    active_run_id: Option<Uuid>,
    cancel_flag: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    total_requests: usize,
    completed: usize,
    results: Vec<RunnerResult>,
    started_at: Option<std::time::Instant>,
    /// Whether the most recently *finished* run was stopped early via
    /// "Stop" rather than running to completion — shown in the summary
    /// footer once the run is done.
    last_run_cancelled: bool,
}

impl Default for RunnerState {
    fn default() -> Self {
        Self {
            collection: None,
            folder_path: Vec::new(),
            target_label: String::new(),
            iterations: 1,
            delay_ms: 0,
            data_file_path: None,
            data_rows: Vec::new(),
            active_run_id: None,
            cancel_flag: None,
            total_requests: 0,
            completed: 0,
            results: Vec::new(),
            started_at: None,
            last_run_cancelled: false,
        }
    }
}

/// Everything that belongs to one open request-editor tab — Postman-style
/// multi-tab means every one of these used to be a single `App` field;
/// now each tab owns its own copy. Two closely-related fields deliberately
/// stayed on `App` instead of moving here: `auto_format_response` (a display
/// preference, not request data — no strong reason to vary per tab) and
/// `renaming` (shared by the sidebar tree's rename flow and the saved-example
/// chip rename flow, both of which only ever have one inline edit box open
/// at a time regardless of tab count).
struct OpenTab {
    current_request: RequestItem,
    origin: RequestOrigin,
    in_flight_id: Uuid,
    is_loading: bool,

    request_tab: RequestTab,
    response_tab: ResponseTab,
    response: Option<HttpResponse>,
    response_error: Option<String>,
    sent_request: Option<SentRequest>,

    /// `Some(id)` while the response panel is showing a saved example
    /// instead of the live response/sent_request/error — see
    /// `effective_response`/`effective_sent_request`/`effective_error`.
    viewing_example: Option<Uuid>,
    /// The two saved examples currently picked in the Diff tab's Left/Right
    /// combo boxes, if both have been chosen.
    diff_examples: Option<(Uuid, Uuid)>,
    /// Inline text-entry buffer for naming a new saved example (`Some` while
    /// the "Save as Example" name field is open; `None` otherwise).
    saving_example_name: Option<String>,

    /// `console.log` output from this tab's pre-request and post-response
    /// Lua scripts, in the order the two phases ran.
    script_log: Vec<String>,
    /// The most recent script error (pre-request or post-response), if any.
    script_error: Option<String>,
    /// `pm.test(...)` results from the post-response script.
    test_results: Vec<TestResult>,

    /// Snapshot of `current_request` as of the last disk write, so we can
    /// detect edits without re-saving on every unchanged frame.
    autosave_last_synced: Option<RequestItem>,
    /// Throttles autosave writes to disk so continuous typing doesn't hit
    /// the filesystem every frame.
    autosave_last_saved_at: Option<std::time::Instant>,

    /// Find-in-body text for this tab's request/response body viewers.
    request_body_find: String,
    response_body_find: String,
    /// Manual syntax-highlighting override for this tab's Raw request body
    /// editor and response body viewer respectively — `Auto` (the default)
    /// defers to Content-Type/sniffing exactly as before; picking JSON/XML/
    /// Plain Text overrides that guess. Session-only, like the find text
    /// above — not part of the saved `RequestItem`.
    request_body_language: syntax::LanguageOverride,
    response_body_language: syntax::LanguageOverride,

    /// VS Code-style "preview" tab: at most one tab is ever a preview at a
    /// time. Navigating the sidebar tree's keyboard cursor (Ctrl+N/P) reuses
    /// this same tab slot for whatever request the cursor currently sits
    /// on, instead of opening a new tab for every row stepped over — Enter
    /// (or `open_request_in_tab`'s own normal open path) "pins" it by
    /// clearing this flag, after which it behaves like any other tab.
    is_preview: bool,
}

impl OpenTab {
    /// A brand-new, never-saved tab — the "+ " new-tab button / Cmd/Ctrl+T,
    /// and the very first tab a fresh `App` starts with.
    fn new_unsaved() -> Self {
        Self {
            current_request: RequestItem::new("New Request"),
            origin: RequestOrigin::Unsaved,
            in_flight_id: Uuid::nil(),
            is_loading: false,
            request_tab: RequestTab::Params,
            response_tab: ResponseTab::Body,
            response: None,
            response_error: None,
            sent_request: None,
            viewing_example: None,
            diff_examples: None,
            saving_example_name: None,
            script_log: Vec::new(),
            script_error: None,
            test_results: Vec::new(),
            autosave_last_synced: None,
            autosave_last_saved_at: None,
            request_body_find: String::new(),
            response_body_find: String::new(),
            request_body_language: syntax::LanguageOverride::default(),
            response_body_language: syntax::LanguageOverride::default(),
            is_preview: false,
        }
    }

    /// Opens `req` (loaded from a collection) in a fresh tab. `autosave_last_synced`
    /// is primed to the loaded request itself so it doesn't look dirty the
    /// instant it's opened.
    fn from_saved(collection: Uuid, folder_path: Vec<Uuid>, req: RequestItem) -> Self {
        Self {
            autosave_last_synced: Some(req.clone()),
            origin: RequestOrigin::Collection {
                collection,
                folder_path,
                request: req.id,
            },
            current_request: req,
            ..Self::new_unsaved()
        }
    }

    /// Whether this tab has edits not yet reflected on disk — the same
    /// predicate `autosave_if_dirty` uses to decide whether to write, reused
    /// here so the tab bar's unsaved-dot indicator never disagrees with it.
    fn is_dirty(&self) -> bool {
        self.autosave_last_synced.as_ref() != Some(&self.current_request)
    }
}

pub struct App {
    data: AppData,
    /// Proxy/TLS config — persisted separately from `AppData` (see
    /// `storage::load_settings`), since it configures `client` itself
    /// rather than being request/collection state.
    settings: Settings,
    /// Shared with `client` (via `.cookie_provider`) so rebuilding `client`
    /// after a Settings change doesn't lose accumulated cookies.
    cookie_jar: std::sync::Arc<CookieStoreMutex>,
    rt: tokio::runtime::Runtime,
    client: reqwest::Client,
    tx: Sender<(Uuid, RequestOutcome)>,
    rx: Receiver<(Uuid, RequestOutcome)>,

    /// Collection Runner config/progress/results — see `RunnerState`. Kept
    /// as its own field (not folded into `data`) since it's session-only,
    /// never persisted.
    runner: RunnerState,
    /// A running, in-app log of every send (across every tab) plus its
    /// script `console.log` output and test results — Postman's own
    /// "Console" panel. Session-only, newest-first, capped the same way
    /// `data.history` is (see `push_console_entry`) — the durable trail
    /// for troubleshooting *after* a restart is the separate `console.log`
    /// text file `push_console_entry` also writes to on every entry.
    console: Vec<ConsoleEntry>,
    /// Separate from `tx`/`rx` above: unrelated event shape (`RunnerEvent`
    /// vs. `(Uuid, RequestOutcome)`) and unrelated consumer (`poll_runner`
    /// vs. `poll_responses`), so kept as its own channel rather than
    /// widening the tab-shaped one.
    runner_tx: Sender<RunnerEvent>,
    runner_rx: Receiver<RunnerEvent>,

    sidebar_tab: SidebarTab,
    central_view: CentralView,
    split_direction: SplitDirection,
    active_environment: Option<Uuid>,

    /// Open request-editor tabs, Postman-style — never empty (see `close_tab`).
    tabs: Vec<OpenTab>,
    active_tab: usize,
    /// Last `active_tab` the horizontal tab bar auto-scrolled to — lets
    /// `tab_bar` scroll the active chip into view only on the frame it
    /// actually changes, not every frame (which would fight a manual
    /// horizontal scroll).
    tab_bar_scrolled_to: usize,
    /// `self.console.len()` last time `console_panel` auto-scrolled to the
    /// newest entry — lets a fresh send jump the view to the top (where the
    /// newest entry now lives) without fighting a manual scroll otherwise.
    console_scrolled_len: usize,

    /// A display preference, not request data — stays global rather than
    /// per-tab (see the `OpenTab` doc comment).
    auto_format_response: bool,
    /// Which language the Code tab renders — same "display preference,
    /// stays global" reasoning as `auto_format_response`.
    codegen_target: codegen::CodeGenTarget,

    new_collection_name: String,
    new_environment_name: String,

    /// Whether the inline "paste a curl command" row is expanded.
    curl_import_open: bool,
    curl_import_text: String,
    /// One-line status from the last import/export action (success or
    /// error), shown under the sidebar's import buttons. There's no general
    /// toast/notification system yet, so this is a single slot, overwritten
    /// by the next action.
    import_message: Option<String>,

    /// Which sidebar item (collection, folder, or request — dispatched by
    /// ID search, see `PendingAction::Rename`) is currently showing an
    /// inline rename text field, and its edit buffer. Also reused by the
    /// saved-examples strip's chip rename (see the `OpenTab` doc comment).
    renaming: Option<(Uuid, String)>,

    /// Which collections/folders (by their own id) are currently expanded
    /// in the sidebar tree — the actual source of truth `flatten_visible_rows`
    /// reads every frame to decide what to include, replacing egui's own
    /// per-`CollapsingHeader` memory (which the old recursive renderer
    /// relied on, and which a flat, virtualized `show_rows` list can't
    /// consult before it knows what to lay out). Empty by default — same
    /// "collapsed by default" starting point the pre-virtualization fix
    /// already established. "Expand all"/"Collapse all" mutate this
    /// directly (insert every id / clear it) instead of the old
    /// one-shot-per-frame `tree_force_open` field this replaced.
    expanded_nodes: std::collections::HashSet<Uuid>,

    /// Emacs-style tree navigation cursor — an index into the current
    /// frame's flattened `model::Row` list (`None` until the user first
    /// presses one of Ctrl+N/P/F/B in the sidebar). Highlighted in
    /// `render_tree_row`; Ctrl+N/P move it, Ctrl+F/B expand/collapse the
    /// row it points to, Enter opens it if it's a request.
    ///
    /// Same Ctrl+N/P/Enter convention reused for the History and
    /// Environments sidebar tabs (`history_focused_index`/
    /// `environments_focused_index`, right below) — each keyed to its own
    /// list, since only one sidebar tab is ever rendered at a time
    /// (`sidebar_tab`), which is what scopes the shortcuts to "whichever
    /// pane you're looking at," the same way clicking between panes in a
    /// native macOS app changes which one arrow keys apply to.
    tree_focused_index: Option<usize>,
    /// Ctrl+N/P cursor for the History sidebar tab (Enter re-opens that
    /// entry, same as clicking it). `None` until first used.
    history_focused_index: Option<usize>,
    /// Ctrl+N/P cursor for the Environments sidebar tab (Enter switches to
    /// it, same as clicking it). `None` until first used.
    environments_focused_index: Option<usize>,

    /// `Some(request_id)` while the sidebar's "Assign hotkey…" context-menu
    /// entry is waiting for the user to press a `0`-`9`/`a`-`z` key (no
    /// modifier — the *next* raw key, not an Option/Alt-chord) to bind to
    /// that request; `fn ui`'s shortcut block checks this every frame and
    /// clears it once a key is captured (or Escape cancels it). See
    /// `AppData.hotkey_bindings` for where the binding itself lives, and
    /// the `Alt`+key loop (also in `fn ui`) for how it's actually invoked
    /// afterward.
    pending_hotkey_assignment: Option<Uuid>,

    /// The command palette's leader-key chord in progress — empty when
    /// none is (the common case). Only builds up while nothing has
    /// keyboard focus (see `fn ui`'s own check), so Space still types a
    /// literal space in every text field. See `LEADER_CHORDS`.
    leader_buffer: String,
    /// When the in-progress chord above expires if no further matching key
    /// arrives — `None` exactly when `leader_buffer` is empty.
    leader_deadline: Option<std::time::Instant>,

    /// Search every request by name/URL/method — opened by completing a
    /// leader-key chord (see `LEADER_CHORDS`), not a plain keyboard
    /// shortcut anymore.
    search_open: bool,
    search_query: String,
    search_needs_focus: bool,
    /// `<leader>sf` ("search files", i.e. requests) shows *only* request
    /// entries; `<leader>sc` shows *only* `PaletteAction`s; every other
    /// chord that opens this same window (`<leader>se`) wants the combined
    /// list, just pre-filtered by its own query text.
    palette_scope: PaletteScope,
    /// Which entry in the command palette's filtered list is highlighted —
    /// `Ctrl+N`/`Ctrl+P` (Emacs-style) move it, Enter picks whichever one
    /// this points at. Reset to `0` whenever the palette opens or the
    /// query text changes, matching fzf/telescope's own "a new filter
    /// always starts selection back at the top" convention.
    palette_selected: usize,
    /// The fzf-quality fuzzy matcher backing the command palette's scoring
    /// (see `fuzzy_score`/`fuzzy_score_request`) — kept as a persistent
    /// field and reused every frame rather than constructed fresh each
    /// time: `nucleo_matcher::Matcher::new` eagerly allocates a sizable
    /// scratch buffer (~135KB per its own docs), which the crate's own
    /// docs specifically say to reuse when matching is called often, like
    /// once per candidate on every frame the palette is open.
    fuzzy_matcher: nucleo_matcher::Matcher,

    /// Window size as of last frame, to detect an active resize drag.
    last_screen_size: Option<egui::Vec2>,
    /// While in the future, treat the window as still being actively resized
    /// (reset a bit further out on every size change) so body text editors
    /// can freeze their wrap width instead of re-laying-out huge bodies on
    /// every single intermediate frame of the drag.
    resize_settle_deadline: Option<std::time::Instant>,

    /// Whether the left sidebar (`sidebar`) renders at all this frame —
    /// toggled by the top bar's "Hide/Show Sidebar" button, and also
    /// flipped automatically when the window narrows past
    /// `SIDEBAR_AUTO_HIDE_WIDTH` (see `sidebar_auto_hidden`/`sidebar_was_narrow`).
    sidebar_visible: bool,
    /// `true` exactly when the sidebar is currently hidden *because* the
    /// window is narrow, not because the user asked for it to be hidden —
    /// lets widening the window back out restore it automatically, while a
    /// manual toggle (which always clears this) is never fought by that
    /// auto-restore.
    sidebar_auto_hidden: bool,
    /// Last frame's narrow/wide bucket, so the auto-hide/auto-restore check
    /// is edge-triggered (fires once on the transition) instead of forcing
    /// `sidebar_visible` every single frame the window happens to be narrow
    /// — which would fight a manual re-open while still narrow.
    sidebar_was_narrow: bool,

    /// Every tab (by its request's own id, stable for the tab's lifetime)
    /// the user has actually landed on, in visiting order — a Vim-jumplist-
    /// style back/forth history, not just a plain "most recent" stack.
    /// Appended to once per frame whenever the active tab ends up
    /// different from whatever's already recorded at `tab_jump_cursor`
    /// (see the end of `fn ui`) — covers every way the active tab can
    /// change (clicking a tab, opening one from the sidebar, a hotkey
    /// jump, Option+H/L, Ctrl+Tab), not just one dedicated "navigate"
    /// entry point. Ctrl+O/Ctrl+I (`jump_tab_history`) move `tab_jump_cursor`
    /// and set `active_tab` to match the entry already at that position,
    /// so they never re-record themselves as a new jump.
    tab_jump_history: Vec<Uuid>,
    /// Index into `tab_jump_history` for "where we currently are" — distinct
    /// from `active_tab` itself, since after Ctrl+O moves back, the entries
    /// *ahead* of this cursor are kept (not discarded) until the user
    /// navigates somewhere new by any other means, matching how a browser's
    /// back/forward history works.
    tab_jump_cursor: usize,

    /// Whether the Console drawer is showing along the bottom of the window
    /// — a real Postman-fidelity fix: Console used to be a `CentralView`
    /// that replaced the whole request/response area, but real Postman's
    /// Console is a collapsible panel anchored to the *bottom* of the
    /// window, sitting below the request/response area rather than hiding
    /// it. Toggled by the top bar's "Console" button; rendered in `fn ui`
    /// via `egui::Panel::bottom` *before* `self.central(ui)` is called (side/
    /// bottom panels must be declared before the central panel that fills
    /// whatever space is left).
    console_open: bool,
}

impl App {
    pub fn new() -> Self {
        let data = storage::load();
        let settings = storage::load_settings();
        let cookie_jar = std::sync::Arc::new(storage::load_cookie_jar());
        Self::with_data_settings_and_jar(data, settings, cookie_jar)
    }

    /// Builds the app from an already-loaded [`AppData`] instead of reading
    /// it from disk — lets tests (e.g. `egui_kittest` snapshot tests) drive a
    /// real `App` against a fixture without touching the user's real data
    /// directory. Settings/cookie jar default rather than load from disk
    /// too, for the same reason. Test-only (production goes through `new()`
    /// directly) — hence `#[cfg(test)]` rather than plain dead code.
    #[cfg(test)]
    fn with_data(data: AppData) -> Self {
        Self::with_data_settings_and_jar(
            data,
            Settings::default(),
            std::sync::Arc::new(CookieStoreMutex::default()),
        )
    }

    fn with_data_settings_and_jar(
        mut data: AppData,
        settings: Settings,
        cookie_jar: std::sync::Arc<CookieStoreMutex>,
    ) -> Self {
        migrate_number_shortcuts_to_hotkey_bindings(&mut data);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to create tokio runtime");
        let _guard = rt.enter();
        let client = http_client::build_client(&settings, cookie_jar.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let (runner_tx, runner_rx) = std::sync::mpsc::channel();
        let active_environment = data.environments.first().map(|e| e.id);

        Self {
            data,
            settings,
            cookie_jar,
            rt,
            client,
            tx,
            rx,
            runner: RunnerState::default(),
            console: Vec::new(),
            runner_tx,
            runner_rx,
            sidebar_tab: SidebarTab::Collections,
            central_view: CentralView::Request,
            split_direction: SplitDirection::Vertical,
            active_environment,
            tabs: vec![OpenTab::new_unsaved()],
            active_tab: 0,
            tab_bar_scrolled_to: 0,
            console_scrolled_len: 0,
            auto_format_response: true,
            codegen_target: codegen::CodeGenTarget::Curl,
            new_collection_name: String::new(),
            new_environment_name: String::new(),
            curl_import_open: false,
            curl_import_text: String::new(),
            import_message: None,
            renaming: None,
            expanded_nodes: std::collections::HashSet::new(),
            tree_focused_index: None,
            history_focused_index: None,
            environments_focused_index: None,
            pending_hotkey_assignment: None,
            leader_buffer: String::new(),
            leader_deadline: None,
            search_open: false,
            search_query: String::new(),
            search_needs_focus: false,
            palette_scope: PaletteScope::default(),
            palette_selected: 0,
            fuzzy_matcher: nucleo_matcher::Matcher::default(),
            last_screen_size: None,
            resize_settle_deadline: None,
            sidebar_visible: true,
            sidebar_auto_hidden: false,
            sidebar_was_narrow: false,
            tab_jump_history: Vec::new(),
            tab_jump_cursor: 0,
            console_open: false,
        }
    }

    fn active_tab(&self) -> &OpenTab {
        &self.tabs[self.active_tab]
    }

    fn active_tab_mut(&mut self) -> &mut OpenTab {
        &mut self.tabs[self.active_tab]
    }

    /// Flips `sidebar_visible`, shared by the top bar's "Hide/Show Sidebar"
    /// button and the `<leader>us` chord — a manual toggle always wins over
    /// the auto-hide memory (`sidebar_auto_hidden`'s own doc comment),
    /// otherwise re-opening it while the window is still narrow would just
    /// get auto-hidden again the next time the width check runs.
    fn toggle_sidebar(&mut self) {
        self.sidebar_visible = !self.sidebar_visible;
        self.sidebar_auto_hidden = false;
    }

    /// A neovim `which-key`-style hint: while a leader-key chord is in
    /// progress (`leader_buffer` non-empty), shows every `LEADER_CHORDS`
    /// entry that still matches what's been typed so far, each with its
    /// remaining key(s) and short label — so the available chords don't
    /// have to be memorized. Disappears the same frame the chord resolves
    /// or aborts, since that's exactly when `leader_buffer` clears.
    fn leader_which_key_hint(&self, ctx: &egui::Context) {
        if self.leader_buffer.is_empty() {
            return;
        }
        let candidates: Vec<(&'static str, &'static str)> = LEADER_CHORDS
            .iter()
            .filter(|(seq, _, _)| seq.starts_with(self.leader_buffer.as_str()))
            .map(|(seq, _, label)| (&seq[self.leader_buffer.len()..], *label))
            .collect();
        if candidates.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("leader_which_key_hint"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -12.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    for (keys, label) in &candidates {
                        ui.horizontal(|ui| {
                            ui.strong(*keys);
                            ui.label(*label);
                        });
                    }
                });
            });
    }

    fn save(&self) {
        storage::save(&self.data);
        // Cookies accumulate as a side effect of sending requests, not from
        // an explicit user action — persisted alongside every other save
        // rather than needing its own dirty-tracking. Cheap: cookie jars
        // are small, and `cookie_store`'s own `save_json` already only
        // writes unexpired, persistent cookies (session-only cookies are
        // correctly dropped, matching browser behavior).
        storage::save_cookie_jar(&self.cookie_jar);
    }

    /// Adds one entry to the in-app Console (newest-first, capped the same
    /// way `data.history` is) and appends the same information to the
    /// on-disk `console.log` text trail — called once per completed send,
    /// success or failure, right where `HistoryEntry` is already built.
    fn push_console_entry(&mut self, entry: ConsoleEntry) {
        storage::append_console_log(&entry);
        self.console.insert(0, entry);
        self.console.truncate(500);
    }

    // ---------- Import / Export ----------

    /// Imports every file the user selects in one go (Postman's own import
    /// dialog also accepts multiple files at once) — one collection is
    /// added per file, a single `save()` happens after the whole batch
    /// rather than once per file, and the status line summarizes all of
    /// them together instead of only ever reporting the last one.
    fn import_postman_collection(&mut self) {
        let Some(paths) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .pick_files()
        else {
            return;
        };
        let mut added_any = false;
        let results: Vec<String> = paths
            .iter()
            .map(|path| match std::fs::read_to_string(path) {
                Ok(contents) => match postman_format::import_collection(&contents) {
                    Ok((collection, warnings)) => {
                        let name = collection.name.clone();
                        self.data.collections.push(collection);
                        added_any = true;
                        describe_import_result(&format!("Postman collection {name:?}"), &warnings)
                    }
                    Err(e) => format!("{}: import failed: {e}", path.display()),
                },
                Err(e) => format!("{}: could not read file: {e}", path.display()),
            })
            .collect();
        if added_any {
            self.save();
        }
        self.import_message = Some(describe_import_batch(results));
    }

    fn import_openapi_spec(&mut self) {
        let Some(paths) = rfd::FileDialog::new()
            .add_filter("OpenAPI", &["json", "yaml", "yml"])
            .pick_files()
        else {
            return;
        };
        let mut added_any = false;
        let results: Vec<String> = paths
            .iter()
            .map(|path| match std::fs::read_to_string(path) {
                Ok(contents) => match openapi_import::import_openapi(&contents) {
                    Ok((collection, warnings)) => {
                        let name = collection.name.clone();
                        self.data.collections.push(collection);
                        added_any = true;
                        describe_import_result(&format!("OpenAPI spec {name:?}"), &warnings)
                    }
                    Err(e) => format!("{}: import failed: {e}", path.display()),
                },
                Err(e) => format!("{}: could not read file: {e}", path.display()),
            })
            .collect();
        if added_any {
            self.save();
        }
        self.import_message = Some(describe_import_batch(results));
    }

    /// Parses `self.curl_import_text` and, on success, loads it as an
    /// editable unsaved request — same entry point as "New Request", just
    /// pre-filled.
    fn import_curl_command(&mut self) {
        match curl_import::import_curl(&self.curl_import_text) {
            Ok(req) => {
                // Same entry point as "New Request": a fresh, editable,
                // unsaved tab — reviewable/Saveable before it touches disk.
                self.tabs.push(OpenTab {
                    current_request: req,
                    ..OpenTab::new_unsaved()
                });
                self.active_tab = self.tabs.len() - 1;
                self.central_view = CentralView::Request;
                self.curl_import_open = false;
                self.curl_import_text.clear();
                self.import_message = Some("Imported curl command — review and Save".to_string());
            }
            Err(e) => self.import_message = Some(format!("Could not parse curl command: {e}")),
        }
    }

    fn import_postman_environment(&mut self) {
        let Some(paths) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .pick_files()
        else {
            return;
        };
        let mut added_any = false;
        let results: Vec<String> = paths
            .iter()
            .map(|path| {
                let result = std::fs::read_to_string(path)
                    .map_err(|e| e.to_string())
                    .and_then(|contents| postman_format::import_environment(&contents));
                match result {
                    Ok(env) => {
                        let name = env.name.clone();
                        self.data.environments.push(env);
                        added_any = true;
                        format!("Imported environment {name:?}")
                    }
                    Err(e) => format!("{}: import failed: {e}", path.display()),
                }
            })
            .collect();
        if added_any {
            self.save();
        }
        self.import_message = Some(describe_import_batch(results));
    }

    /// Adds a fresh copy of `Collection::sample()` — self-service, so a user
    /// who deleted or messed with the sample collection can always get a
    /// clean one back without needing to ask for it to be re-seeded by
    /// hand (which is how it was done before this button existed).
    fn add_sample_collection(&mut self) {
        self.data.collections.push(Collection::sample());
        self.save();
        self.import_message = Some("Added the \"Sample Requests\" collection".to_string());
    }

    fn export_collection_to_file(&mut self, collection_id: Uuid) {
        let Some(collection) = self.data.collections.iter().find(|c| c.id == collection_id) else {
            return;
        };
        let json = postman_format::export_collection(collection);
        let default_name = format!("{}.postman_collection.json", collection.name);
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(&default_name)
            .save_file()
        else {
            return;
        };
        self.import_message = Some(match std::fs::write(&path, json) {
            Ok(()) => format!("Exported to {}", path.display()),
            Err(e) => format!("Export failed: {e}"),
        });
    }

    fn export_environment_to_file(&mut self, env_id: Uuid) {
        let Some(env) = self.data.environments.iter().find(|e| e.id == env_id) else {
            return;
        };
        let json = postman_format::export_environment(env);
        let default_name = format!("{}.postman_environment.json", env.name);
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(&default_name)
            .save_file()
        else {
            return;
        };
        self.import_message = Some(match std::fs::write(&path, json) {
            Ok(()) => format!("Exported to {}", path.display()),
            Err(e) => format!("Export failed: {e}"),
        });
    }

    fn active_env(&self) -> Option<Environment> {
        self.active_environment
            .and_then(|id| self.data.environments.iter().find(|e| e.id == id).cloned())
    }

    /// Routes each incoming `(id, outcome)` to whichever tab actually sent
    /// that request — **not** just the active tab. This is the fix for a
    /// real concurrency bug: previously there was only one global
    /// `in_flight_id`, so sending from a second tab while a first tab's
    /// request was still in flight would silently drop the first tab's
    /// reply (`id != self.in_flight_id` was true for it once a second
    /// request started, so it hit the `continue` below and vanished for
    /// good). Now every tab has its own `in_flight_id`, so a background
    /// tab's response lands correctly whenever it arrives.
    fn poll_responses(&mut self, ctx: &egui::Context) {
        while let Ok((id, outcome)) = self.rx.try_recv() {
            let Some(idx) = self.tabs.iter().position(|t| t.in_flight_id == id) else {
                continue; // stale response from a superseded/closed tab
            };
            self.tabs[idx].is_loading = false;
            let origin = self.tabs[idx].origin.clone();
            let mut env = self.active_env();
            let mut globals = self.data.globals.clone();
            let mut collection_vars = self.collection_variables(&origin);
            let container_scripts = self.container_scripts(&origin);
            match outcome {
                RequestOutcome::Success { request, response } => {
                    // Post-response chain, same order as pre-request:
                    // collection, each folder outermost to innermost, then
                    // the request's own script.
                    let mut post_scripts: Vec<String> = container_scripts
                        .iter()
                        .map(|(_, post)| post.clone())
                        .collect();
                    post_scripts.push(self.tabs[idx].current_request.post_response_script.clone());
                    let mut chain_log = Vec::new();
                    let mut chain_tests = Vec::new();
                    let mut chain_error = None;
                    for script in &post_scripts {
                        let run = scripting::run_post_response(
                            script,
                            scripting::ScriptContext {
                                env: &mut env,
                                globals: &mut globals,
                                collection_variables: &mut collection_vars,
                                client: self.client.clone(),
                                runtime: self.rt.handle().clone(),
                            },
                            Some(&response),
                            None,
                        );
                        chain_log.extend(run.log);
                        chain_tests.extend(run.tests);
                        if run.error.is_some() {
                            chain_error = run.error;
                            break;
                        }
                    }
                    if let Some(env) = &env {
                        self.persist_environment(env);
                    }
                    self.persist_globals(&globals);
                    self.persist_collection_variables(&origin, &collection_vars);

                    let tab = &mut self.tabs[idx];
                    tab.script_log.extend(chain_log);
                    if chain_error.is_some() {
                        tab.script_error = chain_error;
                    }
                    tab.test_results = chain_tests;

                    let entry = HistoryEntry {
                        id: Uuid::new_v4(),
                        timestamp: chrono::Utc::now(),
                        method: tab.current_request.method,
                        url: tab.current_request.url.clone(),
                        status: Some(response.status),
                        request: tab.current_request.clone(),
                        sent_request: Some(request.clone()),
                        duration_ms: Some(response.duration_ms),
                        response: Some(response.clone()),
                        error: None,
                        test_results: tab.test_results.clone(),
                    };
                    let console_entry = ConsoleEntry {
                        timestamp: chrono::Utc::now(),
                        method: tab.current_request.method,
                        url: tab.current_request.url.clone(),
                        status: Some(response.status),
                        duration_ms: Some(response.duration_ms),
                        error: None,
                        script_log: tab.script_log.clone(),
                        test_results: tab.test_results.clone(),
                    };
                    self.data.history.insert(0, entry);
                    self.data.history.truncate(200);
                    self.push_console_entry(console_entry);
                    let tab = &mut self.tabs[idx];
                    tab.sent_request = Some(request);
                    tab.response = Some(response);
                    tab.response_error = None;
                    self.save();
                }
                RequestOutcome::Error {
                    request,
                    message,
                    duration_ms,
                } => {
                    let mut post_scripts: Vec<String> = container_scripts
                        .iter()
                        .map(|(_, post)| post.clone())
                        .collect();
                    post_scripts.push(self.tabs[idx].current_request.post_response_script.clone());
                    let mut chain_log = Vec::new();
                    let mut chain_tests = Vec::new();
                    let mut chain_error = None;
                    for script in &post_scripts {
                        let run = scripting::run_post_response(
                            script,
                            scripting::ScriptContext {
                                env: &mut env,
                                globals: &mut globals,
                                collection_variables: &mut collection_vars,
                                client: self.client.clone(),
                                runtime: self.rt.handle().clone(),
                            },
                            None,
                            Some(&message),
                        );
                        chain_log.extend(run.log);
                        chain_tests.extend(run.tests);
                        if run.error.is_some() {
                            chain_error = run.error;
                            break;
                        }
                    }
                    if let Some(env) = &env {
                        self.persist_environment(env);
                    }
                    self.persist_globals(&globals);
                    self.persist_collection_variables(&origin, &collection_vars);

                    let tab = &mut self.tabs[idx];
                    tab.script_log.extend(chain_log);
                    if chain_error.is_some() {
                        tab.script_error = chain_error;
                    }
                    tab.test_results = chain_tests;

                    let entry = HistoryEntry {
                        id: Uuid::new_v4(),
                        timestamp: chrono::Utc::now(),
                        method: tab.current_request.method,
                        url: tab.current_request.url.clone(),
                        status: None,
                        request: tab.current_request.clone(),
                        sent_request: request.clone(),
                        duration_ms,
                        response: None,
                        error: Some(message.clone()),
                        test_results: tab.test_results.clone(),
                    };
                    let console_entry = ConsoleEntry {
                        timestamp: chrono::Utc::now(),
                        method: tab.current_request.method,
                        url: tab.current_request.url.clone(),
                        status: None,
                        duration_ms,
                        error: Some(message.clone()),
                        script_log: tab.script_log.clone(),
                        test_results: tab.test_results.clone(),
                    };
                    self.data.history.insert(0, entry);
                    self.data.history.truncate(200);
                    self.push_console_entry(console_entry);
                    let tab = &mut self.tabs[idx];
                    tab.sent_request = request;
                    tab.response = None;
                    tab.response_error = Some(message);
                    self.save();
                }
            }
        }
        if self.tabs.iter().any(|t| t.is_loading) {
            ctx.request_repaint();
        }
    }

    fn send_current_request(&mut self) {
        let origin = self.active_tab().origin.clone();
        let mut env = self.active_env();
        let mut globals = self.data.globals.clone();
        let mut collection_vars = self.collection_variables(&origin);
        let auth = self.effective_auth(self.active_tab());
        let container_scripts = self.container_scripts(&origin);

        let id = Uuid::new_v4();
        let tab = self.active_tab_mut();
        tab.in_flight_id = id;
        tab.is_loading = true;
        tab.response = None;
        tab.response_error = None;
        tab.sent_request = None;
        tab.script_log.clear();
        tab.script_error = None;
        tab.test_results.clear();

        let mut item = tab.current_request.clone();
        // Pre-request chain: collection, then each folder outermost to
        // innermost, then the request's own script — a broken script
        // anywhere in the chain stops it early (don't send half-configured,
        // matching Postman's behavior); a script's rewrites to `item` carry
        // forward into the next link, same as `pm.request` always has.
        let mut pre_scripts: Vec<String> = container_scripts
            .iter()
            .map(|(pre, _)| pre.clone())
            .collect();
        pre_scripts.push(item.pre_request_script.clone());
        let mut chain_log = Vec::new();
        let mut chain_error = None;
        for script in &pre_scripts {
            let run = scripting::run_pre_request(
                script,
                &mut item,
                scripting::ScriptContext {
                    env: &mut env,
                    globals: &mut globals,
                    collection_variables: &mut collection_vars,
                    client: self.client.clone(),
                    runtime: self.rt.handle().clone(),
                },
            );
            chain_log.extend(run.log);
            if run.error.is_some() {
                chain_error = run.error;
                break;
            }
        }
        if let Some(env) = &env {
            self.persist_environment(env);
        }
        self.persist_globals(&globals);
        self.persist_collection_variables(&origin, &collection_vars);

        let tab = self.active_tab_mut();
        tab.script_log.extend(chain_log);
        tab.script_error = chain_error;
        // A broken pre-request script (syntax error, uncaught Lua error)
        // means the request may be missing headers/auth it was meant to set
        // up — don't send it half-configured, matching Postman's behavior.
        if tab.script_error.is_some() {
            tab.is_loading = false;
            return;
        }

        // Precedence order: environment > collection > global.
        let variable_scopes = vec![
            env.map(|e| e.variables).unwrap_or_default(),
            collection_vars,
            globals,
        ];

        let client = self.client.clone();
        let tx = self.tx.clone();

        self.rt.spawn(async move {
            let outcome = http_client::send_request(client, item, variable_scopes, auth).await;
            let _ = tx.send((id, outcome));
        });
    }

    /// Variables scoped to the collection `origin` belongs to (empty for an
    /// unsaved request, or one not attached to any collection). Takes
    /// `origin` explicitly (rather than reading `self.tabs[...]` itself) so
    /// callers can resolve this *before* taking a `&mut` borrow of a tab —
    /// see the `OpenTab` doc comment on why that ordering matters.
    fn collection_variables(&self, origin: &RequestOrigin) -> Vec<KeyValue> {
        match origin {
            RequestOrigin::Collection { collection, .. } => self
                .data
                .collections
                .iter()
                .find(|c| c.id == *collection)
                .map(model::collection_scope_variables)
                .unwrap_or_default(),
            RequestOrigin::Unsaved => Vec::new(),
        }
    }

    /// The collection/folder-level `(pre_request_script, post_response_script)`
    /// pairs that should run around a request at `origin`, outermost first
    /// (the collection's own scripts, then each folder from outermost to
    /// innermost — the same order `Collection::folder_chain` already
    /// returns, reused here rather than re-deriving it). Empty for an
    /// unsaved request or one not attached to any collection. The
    /// request's own script isn't included — callers already have it
    /// directly and append it as the final link in the chain.
    fn container_scripts(&self, origin: &RequestOrigin) -> Vec<(String, String)> {
        let RequestOrigin::Collection {
            collection,
            folder_path,
            ..
        } = origin
        else {
            return Vec::new();
        };
        let Some(coll) = self.data.collections.iter().find(|c| c.id == *collection) else {
            return Vec::new();
        };
        model::container_scripts_for(coll, folder_path)
    }

    /// The auth config that actually governs `tab`'s request once `Inherit`
    /// is walked up to whatever it resolves to (folder chain, then the
    /// collection) — what gets sent on the wire, and what the Auth tab shows
    /// for `Inherit`'s read-only "resolves to" label. Thin wrapper over
    /// `model::effective_auth_for`, which the Collection Runner (Phase 9)
    /// also calls directly — it has a `Collection`/`folder_path`/
    /// `RequestItem` triple per iteration, not a live `OpenTab`.
    fn effective_auth(&self, tab: &OpenTab) -> AuthConfig {
        let RequestOrigin::Collection {
            collection,
            folder_path,
            ..
        } = &tab.origin
        else {
            return tab.current_request.auth.clone();
        };
        let Some(coll) = self.data.collections.iter().find(|c| c.id == *collection) else {
            return tab.current_request.auth.clone();
        };
        model::effective_auth_for(coll, folder_path, &tab.current_request.auth)
    }

    // ---------- Collection Runner ----------

    /// Kicks off a Runner run against `self.runner`'s currently-configured
    /// target (`collection`/`folder_path`) — a no-op if no target is set, or
    /// if a run is already active (only one at a time, matching Postman's
    /// own model).
    fn start_run(&mut self) {
        if self.runner.active_run_id.is_some() {
            return;
        }
        let Some(collection_id) = self.runner.collection else {
            return;
        };
        let Some(collection) = self
            .data
            .collections
            .iter()
            .find(|c| c.id == collection_id)
            .cloned()
        else {
            return;
        };

        let requests = if self.runner.folder_path.is_empty() {
            collection.flatten_requests()
        } else {
            collection.flatten_requests_from(&self.runner.folder_path)
        };
        if requests.is_empty() {
            return;
        }

        // The data file's row count wins over the manual `iterations` field
        // once one is loaded — one row = one iteration, Postman's own
        // convention. `vec![Vec::new(); n]` gives each manual iteration an
        // empty (no-op) data scope.
        let iteration_rows = if self.runner.data_rows.is_empty() {
            vec![Vec::new(); self.runner.iterations.max(1)]
        } else {
            self.runner.data_rows.clone()
        };

        let run_id = Uuid::new_v4();
        let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.runner.active_run_id = Some(run_id);
        self.runner.cancel_flag = Some(cancel_flag.clone());
        self.runner.total_requests = iteration_rows.len() * requests.len();
        self.runner.completed = 0;
        self.runner.results.clear();
        self.runner.started_at = Some(std::time::Instant::now());

        let client = self.client.clone();
        let handle = self.rt.handle().clone();
        let tx = self.runner_tx.clone();
        let env = self.active_env();
        let globals = self.data.globals.clone();
        let collection_vars = model::collection_scope_variables(&collection);
        let delay_ms = self.runner.delay_ms;

        // A plain OS thread — deliberately NOT `self.rt.spawn`. See the
        // Phase 9 plan's threading note: `run_pre_request`/`run_post_response`
        // may call `pm.sendRequest`, which itself does
        // `handle.block_on(http_client::send_request(...))`. If this whole
        // sequential loop ran inside one `rt.spawn`ed async task, that inner
        // `block_on` would be a *nested* `block_on` on the same
        // runtime-owned thread — Tokio panics on that ("Cannot start a
        // runtime from within a runtime"). On a plain thread, each
        // `block_on` call (ours for the HTTP send, and any script's own for
        // `pm.sendRequest`) fully completes before the next one starts, so
        // none of them ever nest.
        std::thread::spawn(move || {
            'iterations: for (iteration, data_row) in iteration_rows.iter().enumerate() {
                for (folder_path, item) in &requests {
                    if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                        break 'iterations;
                    }

                    let auth = model::effective_auth_for(&collection, folder_path, &item.auth);
                    // Highest precedence first: iteration data row, then
                    // environment, then collection, then globals — matches
                    // Postman's own iteration-data-wins convention.
                    let mut env = env.clone();
                    let mut globals = globals.clone();
                    let mut collection_vars = collection_vars.clone();
                    let mut prepared = item.clone();

                    let container_scripts = model::container_scripts_for(&collection, folder_path);
                    let mut pre_scripts: Vec<String> = container_scripts
                        .iter()
                        .map(|(pre, _)| pre.clone())
                        .collect();
                    pre_scripts.push(prepared.pre_request_script.clone());
                    let mut pre_error = None;
                    for script in &pre_scripts {
                        let run = scripting::run_pre_request(
                            script,
                            &mut prepared,
                            scripting::ScriptContext {
                                env: &mut env,
                                globals: &mut globals,
                                collection_variables: &mut collection_vars,
                                client: client.clone(),
                                runtime: handle.clone(),
                            },
                        );
                        if run.error.is_some() {
                            pre_error = run.error;
                            break;
                        }
                    }
                    if let Some(err) = pre_error {
                        let result = RunnerResult {
                            iteration,
                            method: prepared.method,
                            name: prepared.name.clone(),
                            status: None,
                            duration_ms: None,
                            test_results: Vec::new(),
                            error: Some(format!("Pre-request script error: {err}")),
                        };
                        let _ = tx.send(RunnerEvent::RequestFinished { run_id, result });
                        continue;
                    }

                    let variable_scopes = vec![
                        data_row.clone(),
                        env.clone().map(|e| e.variables).unwrap_or_default(),
                        collection_vars.clone(),
                        globals.clone(),
                    ];
                    // One self-contained `block_on` call, fully completing
                    // before the next line runs — see the doc comment above.
                    let outcome = handle.block_on(http_client::send_request(
                        client.clone(),
                        prepared.clone(),
                        variable_scopes,
                        auth,
                    ));

                    let (status, duration_ms, response, error) = match &outcome {
                        RequestOutcome::Success { response, .. } => (
                            Some(response.status),
                            Some(response.duration_ms),
                            Some(response),
                            None,
                        ),
                        RequestOutcome::Error {
                            message,
                            duration_ms,
                            ..
                        } => (None, *duration_ms, None, Some(message.as_str())),
                    };
                    let mut post_scripts: Vec<String> = container_scripts
                        .iter()
                        .map(|(_, post)| post.clone())
                        .collect();
                    post_scripts.push(prepared.post_response_script.clone());
                    let mut post_tests = Vec::new();
                    let mut post_error = None;
                    for script in &post_scripts {
                        let run = scripting::run_post_response(
                            script,
                            scripting::ScriptContext {
                                env: &mut env,
                                globals: &mut globals,
                                collection_variables: &mut collection_vars,
                                client: client.clone(),
                                runtime: handle.clone(),
                            },
                            response,
                            error,
                        );
                        post_tests.extend(run.tests);
                        if run.error.is_some() {
                            post_error = run.error;
                            break;
                        }
                    }

                    let result = RunnerResult {
                        iteration,
                        method: prepared.method,
                        name: prepared.name.clone(),
                        status,
                        duration_ms,
                        test_results: post_tests,
                        error: error.map(str::to_string).or(post_error),
                    };
                    if tx
                        .send(RunnerEvent::RequestFinished { run_id, result })
                        .is_err()
                    {
                        // The UI side is gone (app closed) — stop early.
                        break 'iterations;
                    }

                    if delay_ms > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    }
                }
            }
            let cancelled = cancel_flag.load(std::sync::atomic::Ordering::Relaxed);
            let _ = tx.send(RunnerEvent::RunFinished { run_id, cancelled });
        });
    }

    /// Signals the active run's background thread to stop after its
    /// current request — doesn't try to kill the thread forcibly, so a
    /// request already in flight is allowed to finish.
    fn cancel_run(&mut self) {
        if let Some(flag) = &self.runner.cancel_flag {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Drains `self.runner_rx` each frame — same shape as `poll_responses`.
    /// Events from a superseded/cancelled run (`run_id` mismatch) are
    /// dropped rather than applied.
    fn poll_runner(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.runner_rx.try_recv() {
            match event {
                RunnerEvent::RequestFinished { run_id, result } => {
                    if self.runner.active_run_id != Some(run_id) {
                        continue;
                    }
                    self.runner.completed += 1;
                    self.runner.results.push(result);
                }
                RunnerEvent::RunFinished { run_id, cancelled } => {
                    if self.runner.active_run_id != Some(run_id) {
                        continue;
                    }
                    self.runner.active_run_id = None;
                    self.runner.cancel_flag = None;
                    self.runner.last_run_cancelled = cancelled;
                }
            }
        }
        if self.runner.active_run_id.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    /// Parses a CSV or JSON data file into precedence-ordered rows — a CSV
    /// with a header row (each row's `KeyValue`s keyed by that header), or a
    /// JSON array of flat objects (each object's entries, stringified,
    /// become one row). Non-string JSON values are stringified via their
    /// `serde_json::Value` `Display`-ish rendering (numbers/bools print
    /// plainly; nested arrays/objects print as compact JSON) rather than
    /// rejected — Postman's own CSV/JSON data files are all-string anyway,
    /// so this just extends that same expectation to non-string JSON.
    fn parse_runner_data_file(path: &std::path::Path) -> Result<Vec<Vec<KeyValue>>, String> {
        let is_json = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("json"));
        let contents = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        if is_json {
            let value: serde_json::Value =
                serde_json::from_str(&contents).map_err(|e| e.to_string())?;
            let serde_json::Value::Array(rows) = value else {
                return Err("Expected a JSON array of objects".to_string());
            };
            rows.into_iter()
                .map(|row| {
                    let serde_json::Value::Object(map) = row else {
                        return Err("Expected each row to be a JSON object".to_string());
                    };
                    Ok(map
                        .into_iter()
                        .map(|(key, v)| KeyValue {
                            key,
                            value: match v {
                                serde_json::Value::String(s) => s,
                                other => other.to_string(),
                            },
                            enabled: true,
                        })
                        .collect())
                })
                .collect()
        } else {
            let mut reader = csv::Reader::from_reader(contents.as_bytes());
            let headers = reader.headers().map_err(|e| e.to_string())?.clone();
            reader
                .records()
                .map(|record| {
                    let record = record.map_err(|e| e.to_string())?;
                    Ok(headers
                        .iter()
                        .zip(record.iter())
                        .map(|(key, value)| KeyValue {
                            key: key.to_string(),
                            value: value.to_string(),
                            enabled: true,
                        })
                        .collect())
                })
                .collect()
        }
    }

    /// Writes environment variables that a pre-request/post-response script
    /// changed via `pm.environment.set(...)` back into the persisted
    /// environment, so later requests (and a future app launch) see them.
    fn persist_environment(&mut self, env: &Environment) {
        if let Some(existing) = self.data.environments.iter_mut().find(|e| e.id == env.id)
            && existing.variables != env.variables
        {
            existing.variables = env.variables.clone();
            self.save();
        }
    }

    /// Writes `AppData.globals` back after a script's `pm.globals.set(...)`
    /// — same "only touch disk if it actually changed" shape as
    /// `persist_environment`.
    fn persist_globals(&mut self, globals: &[KeyValue]) {
        if self.data.globals != globals {
            self.data.globals = globals.to_vec();
            self.save();
        }
    }

    /// Writes the owning collection's `variables` back after a script's
    /// `pm.collectionVariables.set(...)` — a no-op for an unsaved request
    /// (there's no collection to write into).
    fn persist_collection_variables(&mut self, origin: &RequestOrigin, variables: &[KeyValue]) {
        let RequestOrigin::Collection { collection, .. } = origin else {
            return;
        };
        let collection_id = *collection;
        if let Some(c) = self
            .data
            .collections
            .iter_mut()
            .find(|c| c.id == collection_id)
            && c.variables != variables
        {
            c.variables = variables.to_vec();
            self.save();
        }
    }

    // ---------- UI: top bar ----------
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("RustGirl");
                ui.separator();
                let sidebar_label = if self.sidebar_visible {
                    "Hide Sidebar"
                } else {
                    "Show Sidebar"
                };
                if ui
                    .button(sidebar_label)
                    .on_hover_text("Toggle the left sidebar (also auto-hides on a narrow window)")
                    .clicked()
                {
                    self.toggle_sidebar();
                }
                ui.separator();
                ui.label("Environment:");
                let current_name = self
                    .active_env()
                    .map(|e| e.name)
                    .unwrap_or_else(|| "No Environment".to_string());
                let env_combo = egui::ComboBox::from_id_salt("active_env_combo")
                    .selected_text(current_name.as_str())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.active_environment, None, "No Environment");
                        for env in &self.data.environments {
                            ui.selectable_value(
                                &mut self.active_environment,
                                Some(env.id),
                                &env.name,
                            );
                        }
                    });
                env_combo.response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::ComboBox,
                        true,
                        format!("Active environment: {current_name}"),
                    )
                });

                ui.separator();
                let (icon, tooltip) = match self.split_direction {
                    SplitDirection::Vertical => (
                        "⬍ Split",
                        "Request on top, response below — click for side-by-side",
                    ),
                    SplitDirection::Horizontal => (
                        "⬌ Split",
                        "Request left, response right — click for stacked",
                    ),
                };
                if ui.button(icon).on_hover_text(tooltip).clicked() {
                    self.split_direction = match self.split_direction {
                        SplitDirection::Vertical => SplitDirection::Horizontal,
                        SplitDirection::Horizontal => SplitDirection::Vertical,
                    };
                }

                ui.separator();
                let console_label = if self.console.is_empty() {
                    "Console".to_string()
                } else {
                    format!("Console ({})", self.console.len())
                };
                // A toggle, not a view switch — Postman's own Console lives
                // in a collapsible drawer along the bottom of the window,
                // not a full-screen view that replaces the request/response
                // area (see the bottom-panel render in `fn ui`).
                if ui
                    .selectable_label(self.console_open, console_label)
                    .clicked()
                {
                    self.console_open = !self.console_open;
                }
                if ui.button("Settings").clicked() {
                    self.central_view = CentralView::Settings;
                }
            });
            if let Some(request_id) = self.pending_hotkey_assignment {
                // Names the request being assigned, not just a generic
                // "this request" — checks open tabs first (covers the new
                // `<leader>ah` flow, which targets the active tab and could
                // be an unsaved request with no home in `self.data.collections`
                // at all), falling back to searching every collection
                // (covers the sidebar context menu's "Assign hotkey…",
                // which can target a request that isn't open in any tab).
                let name = self
                    .tabs
                    .iter()
                    .find(|t| t.current_request.id == request_id)
                    .map(|t| t.current_request.name.clone())
                    .or_else(|| {
                        self.data.collections.iter().find_map(|c| {
                            c.flatten_requests()
                                .into_iter()
                                .find(|(_, r)| r.id == request_id)
                                .map(|(_, r)| r.name)
                        })
                    })
                    .unwrap_or_else(|| "this request".to_string());
                ui.colored_label(
                    egui::Color32::YELLOW,
                    format!(
                        "Press a key (0-9, a-z) to assign a hotkey to \"{name}\" — Esc to cancel."
                    ),
                );
            }
        });
    }

    // ---------- UI: sidebar ----------
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        // A subtly darker fill than the main content panel — Postman's own
        // sidebar reads as a visually distinct "chrome" surface, not a flat
        // continuation of the request/response area (`theme::recessed_panel_fill`
        // computes this relative to the resolved theme's own panel color, so
        // it looks right in dark, light, and system-follow mode alike).
        let fill = theme::recessed_panel_fill(ui.style().visuals.panel_fill);
        let frame = egui::Frame::side_top_panel(ui.style()).fill(fill);
        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(280.0)
            .frame(frame)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut self.sidebar_tab,
                        SidebarTab::Collections,
                        "Collections",
                    );
                    ui.selectable_value(
                        &mut self.sidebar_tab,
                        SidebarTab::Environments,
                        "Environments",
                    );
                    ui.selectable_value(&mut self.sidebar_tab, SidebarTab::History, "History");
                });
                ui.separator();

                egui::ScrollArea::vertical().show(ui, |ui| match self.sidebar_tab {
                    SidebarTab::Collections => self.collections_sidebar(ui),
                    SidebarTab::Environments => self.environments_sidebar(ui),
                    SidebarTab::History => self.history_sidebar(ui),
                });
            });
    }

    fn collections_sidebar(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.new_collection_name)
                    .hint_text("Collection name")
                    .desired_width(f32::INFINITY),
            );
            let submitted = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let can_add = !self.new_collection_name.trim().is_empty();
            let clicked = ui
                .add_enabled(
                    can_add,
                    egui::Button::new("+ Collection")
                        .min_size(egui::vec2(ui.available_width(), 0.0)),
                )
                .clicked();
            if can_add && (clicked || submitted) {
                self.data
                    .collections
                    .push(Collection::new(self.new_collection_name.trim().to_string()));
                self.new_collection_name.clear();
                self.save();
            }
        });

        ui.horizontal(|ui| {
            if ui
                .small_button("📥 Postman")
                .on_hover_text("Import a Postman collection (.json)")
                .clicked()
            {
                self.import_postman_collection();
            }
            if ui
                .small_button("📥 OpenAPI")
                .on_hover_text("Import an OpenAPI 3.0 spec (.json/.yaml)")
                .clicked()
            {
                self.import_openapi_spec();
            }
            if ui
                .small_button("📥 curl")
                .on_hover_text("Paste a curl command to import")
                .clicked()
            {
                self.curl_import_open = !self.curl_import_open;
            }
            if ui
                .small_button("🧪 Add sample requests")
                .on_hover_text(
                    "Add a ready-made \"Sample Requests\" collection to try the app's features",
                )
                .clicked()
            {
                self.add_sample_collection();
            }
        });
        if self.curl_import_open {
            ui.add(
                egui::TextEdit::multiline(&mut self.curl_import_text)
                    .hint_text("Paste a curl command…")
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            ui.horizontal(|ui| {
                if ui.small_button("Import").clicked() {
                    self.import_curl_command();
                }
                if ui.small_button("Cancel").clicked() {
                    self.curl_import_open = false;
                    self.curl_import_text.clear();
                }
            });
        }
        import_message_banner(ui, &mut self.import_message);
        ui.horizontal(|ui| {
            if ui
                .small_button("Expand all")
                .on_hover_text("Open every collection and folder")
                .clicked()
            {
                for c in &self.data.collections {
                    self.expanded_nodes.insert(c.id);
                    insert_all_folder_ids(&c.folders, &mut self.expanded_nodes);
                }
            }
            if ui
                .small_button("Collapse all")
                .on_hover_text("Close every collection and folder")
                .clicked()
            {
                self.expanded_nodes.clear();
            }
        });
        ui.separator();

        // Two-tier highlighting: `active_id` gets the full selected-row
        // highlight (same as before tabs existed), `open_ids` gets a small
        // marker for "open in some other tab."
        let active_id = self.active_tab().current_request.id;
        let open_ids: std::collections::HashSet<Uuid> =
            self.tabs.iter().map(|t| t.current_request.id).collect();
        // Flattened fresh every frame from `self.data.collections` +
        // `self.expanded_nodes` — an owned `Vec<model::Row>`, not a borrow
        // into `self.data`, so rendering below can freely take `&mut`
        // borrows of other `self` fields (`expanded_nodes`, `renaming`)
        // without fighting the borrow checker the way the old recursive
        // renderer (which held live `&mut Collection`/`&mut Folder`
        // references throughout) would have.
        let hotkeys_by_request = model::reverse_hotkey_bindings(&self.data.hotkey_bindings);
        let rows = model::flatten_visible_rows(
            &self.data.collections,
            &self.expanded_nodes,
            &hotkeys_by_request,
        );
        // Taken out for the duration of the render pass, same reasoning as
        // before the rewrite — put back once rendering is done, below.
        let mut renaming = self.renaming.take();
        let mut actions: Vec<PendingAction> = Vec::new();

        // Emacs-style tree navigation: Ctrl+N/P move the cursor, Ctrl+F/B
        // expand/collapse the row it's on, Enter opens a request (or
        // toggles expand for a collection/folder). Gated on no widget
        // having keyboard focus (same guard the leader-key chords use) so
        // typing "n"/"p"/"f"/"b" in a text field is never affected, and on
        // the command palette being closed, since it already claims
        // Ctrl+N/P for its own selection while open.
        if !rows.is_empty() && !self.search_open && ui.ctx().memory(|m| m.focused().is_none()) {
            let mut cursor_moved = false;
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::N)) {
                self.tree_focused_index = Some(
                    self.tree_focused_index
                        .map_or(0, |i| (i + 1).min(rows.len() - 1)),
                );
                cursor_moved = true;
            }
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::P)) {
                self.tree_focused_index =
                    Some(self.tree_focused_index.map_or(0, |i| i.saturating_sub(1)));
                cursor_moved = true;
            }
            let focused_row = self.tree_focused_index.and_then(|i| rows.get(i));
            // VS Code-style preview: stepping the keyboard cursor onto a
            // request opens it into the single reusable preview tab
            // (`preview_open_request`) instead of piling up a new tab per
            // row stepped over — only on the frame the cursor actually
            // moved, not every frame, so it doesn't fight the user
            // interacting with whatever's already showing in that tab.
            if cursor_moved
                && let Some(model::RowKind::Request {
                    collection,
                    folder_path,
                    id,
                }) = focused_row.map(|r| &r.kind)
                && let Some(req) = self
                    .data
                    .collections
                    .iter()
                    .find(|c| c.id == *collection)
                    .and_then(|c| c.requests_at(folder_path))
                    .and_then(|list| list.iter().find(|r| r.id == *id))
                    .cloned()
            {
                self.preview_open_request(*collection, folder_path.clone(), req);
            }
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::F))
                && let Some(row) = focused_row
            {
                match &row.kind {
                    model::RowKind::Collection { id } | model::RowKind::Folder { id, .. } => {
                        self.expanded_nodes.insert(*id);
                    }
                    model::RowKind::Request { .. } => {}
                }
            }
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::B))
                && let Some(row) = focused_row
            {
                match &row.kind {
                    model::RowKind::Collection { id } | model::RowKind::Folder { id, .. } => {
                        self.expanded_nodes.remove(id);
                    }
                    model::RowKind::Request { .. } => {}
                }
            }
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
                && let Some(row) = focused_row
            {
                match &row.kind {
                    model::RowKind::Request {
                        collection,
                        folder_path,
                        id,
                    } => {
                        actions.push(PendingAction::Load {
                            collection: *collection,
                            folder_path: folder_path.clone(),
                            id: *id,
                        });
                    }
                    model::RowKind::Collection { id } | model::RowKind::Folder { id, .. } => {
                        if self.expanded_nodes.contains(id) {
                            self.expanded_nodes.remove(id);
                        } else {
                            self.expanded_nodes.insert(*id);
                        }
                    }
                }
            }
        }

        // A single fixed row height for every row kind — required by
        // `show_rows` (confirmed against the cached egui source: it only
        // takes one `row_height_sans_spacing`, not a per-row callback), and
        // safe here since every row's content is already single-line
        // (long request names are truncated with an ellipsis, not wrapped).
        let row_height = ui.spacing().interact_size.y;
        let tree_focused_index = self.tree_focused_index;
        egui::ScrollArea::vertical()
            .id_salt("collections_tree_rows")
            .auto_shrink([false, false])
            .show_rows(ui, row_height, rows.len(), |ui, range| {
                let start = range.start;
                for (i, row) in rows[range].iter().enumerate() {
                    render_tree_row(
                        ui,
                        row,
                        &mut self.expanded_nodes,
                        active_id,
                        &open_ids,
                        &mut renaming,
                        &mut actions,
                        tree_focused_index == Some(start + i),
                    );
                }
            });

        self.renaming = renaming;

        let mut save_needed = false;
        for action in actions {
            match action {
                PendingAction::Load {
                    collection,
                    folder_path,
                    id,
                } => {
                    if let Some(req) = self
                        .data
                        .collections
                        .iter()
                        .find(|c| c.id == collection)
                        .and_then(|c| c.requests_at(&folder_path))
                        .and_then(|list| list.iter().find(|r| r.id == id))
                        .cloned()
                    {
                        self.open_request_in_tab(collection, folder_path, req);
                    }
                }
                PendingAction::AddRequest {
                    collection,
                    folder_path,
                } => {
                    if let Some(c) = self
                        .data
                        .collections
                        .iter_mut()
                        .find(|c| c.id == collection)
                        && let Some(list) = c.requests_at_mut(&folder_path)
                    {
                        list.push(RequestItem::new("New Request"));
                        save_needed = true;
                    }
                }
                PendingAction::AddFolder {
                    collection,
                    folder_path,
                } => {
                    if let Some(c) = self
                        .data
                        .collections
                        .iter_mut()
                        .find(|c| c.id == collection)
                        && let Some(list) = c.folders_at_mut(&folder_path)
                    {
                        list.push(Folder::new("New Folder"));
                        save_needed = true;
                    }
                }
                PendingAction::DeleteRequest {
                    collection,
                    folder_path,
                    id,
                } => {
                    if let Some(c) = self
                        .data
                        .collections
                        .iter_mut()
                        .find(|c| c.id == collection)
                        && let Some(list) = c.requests_at_mut(&folder_path)
                    {
                        let name = list
                            .iter()
                            .find(|r| r.id == id)
                            .map(|r| r.name.clone())
                            .unwrap_or_default();
                        if confirm_delete(&format!("the request \"{name}\"")) {
                            list.retain(|r| r.id != id);
                            save_needed = true;
                            self.clear_origin_if_deleted_request(id);
                        }
                    }
                }
                PendingAction::DeleteFolder {
                    collection,
                    parent_path,
                    id,
                } => {
                    if let Some(c) = self
                        .data
                        .collections
                        .iter_mut()
                        .find(|c| c.id == collection)
                        && let Some(list) = c.folders_at_mut(&parent_path)
                    {
                        let name = list
                            .iter()
                            .find(|f| f.id == id)
                            .map(|f| f.name.clone())
                            .unwrap_or_default();
                        if confirm_delete(&format!(
                            "the folder \"{name}\" and everything inside it"
                        )) {
                            list.retain(|f| f.id != id);
                            save_needed = true;
                            self.clear_origin_if_deleted_folder(id);
                        }
                    }
                }
                PendingAction::DuplicateRequest {
                    collection,
                    folder_path,
                    id,
                } => {
                    if let Some(c) = self
                        .data
                        .collections
                        .iter_mut()
                        .find(|c| c.id == collection)
                        && let Some(list) = c.requests_at_mut(&folder_path)
                        && let Some(idx) = list.iter().position(|r| r.id == id)
                    {
                        let dup = list[idx].duplicate();
                        list.insert(idx + 1, dup);
                        save_needed = true;
                    }
                }
                PendingAction::DuplicateFolder {
                    collection,
                    parent_path,
                    id,
                } => {
                    if let Some(c) = self
                        .data
                        .collections
                        .iter_mut()
                        .find(|c| c.id == collection)
                        && let Some(list) = c.folders_at_mut(&parent_path)
                        && let Some(idx) = list.iter().position(|f| f.id == id)
                    {
                        let dup = list[idx].duplicate();
                        list.insert(idx + 1, dup);
                        save_needed = true;
                    }
                }
                PendingAction::Rename { id, new_name } => {
                    self.apply_rename(id, new_name);
                    save_needed = true;
                }
                PendingAction::MoveRequest {
                    collection,
                    from_path,
                    id,
                    to_path,
                    before_id,
                } => {
                    if let Some(c) = self
                        .data
                        .collections
                        .iter_mut()
                        .find(|c| c.id == collection)
                        && c.move_request(&from_path, id, &to_path, before_id)
                    {
                        save_needed = true;
                    }
                }
                PendingAction::MoveFolder {
                    collection,
                    from_path,
                    id,
                    to_path,
                    before_id,
                } => {
                    if let Some(c) = self
                        .data
                        .collections
                        .iter_mut()
                        .find(|c| c.id == collection)
                        && c.move_folder(&from_path, id, &to_path, before_id)
                    {
                        save_needed = true;
                    }
                }
                PendingAction::RunFolder {
                    collection,
                    folder_path,
                    name,
                } => {
                    self.runner = RunnerState {
                        collection: Some(collection),
                        folder_path,
                        target_label: name,
                        ..RunnerState::default()
                    };
                    self.central_view = CentralView::Runner;
                }
                PendingAction::EditFolder {
                    collection,
                    folder_path,
                } => {
                    self.central_view = CentralView::FolderEditor {
                        collection,
                        folder_path,
                    };
                }
                PendingAction::DeleteCollection(id) => {
                    let name = self
                        .data
                        .collections
                        .iter()
                        .find(|c| c.id == id)
                        .map(|c| c.name.clone())
                        .unwrap_or_default();
                    if confirm_delete(&format!(
                        "the collection \"{name}\" and everything inside it"
                    )) {
                        self.data.collections.retain(|c| c.id != id);
                        save_needed = true;
                        for tab in &mut self.tabs {
                            if matches!(&tab.origin, RequestOrigin::Collection { collection, .. } if *collection == id)
                            {
                                tab.origin = RequestOrigin::Unsaved;
                            }
                        }
                    }
                }
                PendingAction::DuplicateCollection(id) => {
                    if let Some(idx) = self.data.collections.iter().position(|c| c.id == id) {
                        let dup = self.data.collections[idx].duplicate();
                        self.data.collections.insert(idx + 1, dup);
                        save_needed = true;
                    }
                }
                PendingAction::EditCollection(id) => {
                    self.central_view = CentralView::CollectionEditor(id);
                }
                PendingAction::ExportCollection(id) => {
                    self.export_collection_to_file(id);
                }
                PendingAction::RunCollection { collection, name } => {
                    self.runner = RunnerState {
                        collection: Some(collection),
                        target_label: name,
                        ..RunnerState::default()
                    };
                    self.central_view = CentralView::Runner;
                }
                PendingAction::AssignHotkey(id) => {
                    self.pending_hotkey_assignment = Some(id);
                }
            }
        }

        if save_needed {
            self.save();
        }
    }

    /// If any open tab points at the request just deleted, disconnect that
    /// tab from the tree (`Unsaved`) rather than silently pointing at a
    /// request that no longer exists — the in-progress edit stays put. A
    /// background (non-active) tab can have the deleted item open too, so
    /// this checks every tab, not just the active one.
    fn clear_origin_if_deleted_request(&mut self, deleted_id: Uuid) {
        for tab in &mut self.tabs {
            if matches!(&tab.origin, RequestOrigin::Collection { request, .. } if *request == deleted_id)
            {
                tab.origin = RequestOrigin::Unsaved;
            }
        }
    }

    /// Same, but for a deleted folder — disconnects any tab whose open
    /// request lived anywhere inside that folder's subtree.
    fn clear_origin_if_deleted_folder(&mut self, deleted_id: Uuid) {
        for tab in &mut self.tabs {
            if let RequestOrigin::Collection { folder_path, .. } = &tab.origin
                && folder_path.contains(&deleted_id)
            {
                tab.origin = RequestOrigin::Unsaved;
            }
        }
    }

    /// Renames whichever collection/folder/request has `id` — dispatched by
    /// ID search since the recursive tree renderer doesn't track item
    /// "kind" separately (see `PendingAction::Rename`). Also patches any
    /// open tab's name if it's the request just renamed, so it doesn't show
    /// a stale name until reloaded.
    fn apply_rename(&mut self, id: Uuid, new_name: String) {
        'search: for collection in &mut self.data.collections {
            if collection.id == id {
                collection.name = new_name.clone();
                break 'search;
            }
            if let Some(req) = collection.requests.iter_mut().find(|r| r.id == id) {
                req.name = new_name.clone();
                break 'search;
            }
            if rename_in_folders(&mut collection.folders, id, &new_name) {
                break 'search;
            }
        }
        for tab in &mut self.tabs {
            if let RequestOrigin::Collection { request, .. } = &tab.origin
                && *request == id
            {
                tab.current_request.name = new_name.clone();
            }
        }
    }

    /// Opens `req` in a tab — focusing an already-open tab for the same
    /// request instead of duplicating it (Postman-style), or pushing a
    /// fresh tab otherwise. Shared by sidebar clicks, history, search, and
    /// the Option/Alt+1..9 shortcut.
    fn open_request_in_tab(&mut self, collection: Uuid, folder_path: Vec<Uuid>, req: RequestItem) {
        let already_open = self.tabs.iter().position(|t| {
            matches!(&t.origin, RequestOrigin::Collection { collection: c, folder_path: fp, request }
                if *c == collection && *fp == folder_path && *request == req.id)
        });
        match already_open {
            Some(idx) => self.active_tab = idx,
            None => {
                self.tabs
                    .push(OpenTab::from_saved(collection, folder_path, req));
                self.active_tab = self.tabs.len() - 1;
            }
        }
        // "Really" opening a request (as opposed to the tree's Ctrl+N/P
        // preview cursor, `preview_open_request`) always pins it — this is
        // every other way a request gets opened (a click, a hotkey jump,
        // the search palette, Enter on the tree), so clearing the flag
        // here covers all of them in one place.
        self.tabs[self.active_tab].is_preview = false;
        self.central_view = CentralView::Request;
    }

    /// VS Code-style "preview" open: reuses the single existing preview tab
    /// (if any) for `req` instead of opening a brand-new one — used by the
    /// sidebar tree's Ctrl+N/P keyboard cursor, so stepping through rows
    /// doesn't pile up a tab per row stepped over. `open_request_in_tab`
    /// (Enter, a click, ...) "pins" whatever tab it targets by clearing its
    /// `is_preview` flag, after which this function leaves it alone.
    fn preview_open_request(&mut self, collection: Uuid, folder_path: Vec<Uuid>, req: RequestItem) {
        let already_open = self.tabs.iter().position(|t| {
            matches!(&t.origin, RequestOrigin::Collection { collection: c, folder_path: fp, request }
                if *c == collection && *fp == folder_path && *request == req.id)
        });
        if let Some(idx) = already_open {
            // Already open, pinned or not — just focus it, don't touch
            // its pinned state either way.
            self.active_tab = idx;
            return;
        }
        match self.tabs.iter().position(|t| t.is_preview) {
            Some(idx) => {
                self.tabs[idx] = OpenTab::from_saved(collection, folder_path, req);
                self.tabs[idx].is_preview = true;
                self.active_tab = idx;
            }
            None => {
                let mut tab = OpenTab::from_saved(collection, folder_path, req);
                tab.is_preview = true;
                self.tabs.push(tab);
                self.active_tab = self.tabs.len() - 1;
            }
        }
        self.central_view = CentralView::Request;
    }

    /// Opens a brand-new, never-saved tab — the "+" tab-bar button and
    /// Cmd/Ctrl+T.
    fn new_blank_tab(&mut self) {
        self.tabs.push(OpenTab::new_unsaved());
        self.active_tab = self.tabs.len() - 1;
        self.central_view = CentralView::Request;
    }

    /// Closes tab `idx`. Always leaves at least one tab open — closing the
    /// last one immediately opens a fresh blank tab, matching
    /// browser/Postman convention. Closing a tab with unsaved (never-saved)
    /// content silently discards it, same as today's existing behavior when
    /// e.g. a history click replaces the currently-open unsaved edit.
    fn close_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            self.tabs.push(OpenTab::new_unsaved());
        }
        self.active_tab = self.active_tab.min(self.tabs.len() - 1);
    }

    /// Every real UI path that closes a tab (the × button, Cmd/Ctrl+W, the
    /// command palette's "Close Tab") goes through this, not `close_tab`
    /// directly — a dirty tab gets a native "close anyway?" confirmation
    /// first, via the same blocking `rfd` dialog pattern already used for
    /// file pickers elsewhere in this file. `close_tab` itself stays a
    /// plain, dialog-free mutation so existing tests (and this method
    /// itself) can keep calling it directly without popping a real OS
    /// dialog in a headless test run.
    fn close_tab_with_confirmation(&mut self, idx: usize) {
        let Some(tab) = self.tabs.get(idx) else {
            return;
        };
        if !tab.is_dirty() {
            self.close_tab(idx);
            return;
        }
        let name = tab.current_request.name.clone();
        let result = rfd::MessageDialog::new()
            .set_title("Close tab?")
            .set_description(format!("\"{name}\" has unsaved changes. Close it anyway?"))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if result == rfd::MessageDialogResult::Yes {
            self.close_tab(idx);
        }
    }

    /// Finds a request anywhere in `self.data.collections` by its own id
    /// and opens it via `open_request_in_tab` — the execution side of the
    /// Option/Alt+key hotkey system (`AppData.hotkey_bindings`). A stale
    /// binding (the request was since deleted, or moved into a collection
    /// that no longer exists) is silently a no-op, not an error — the same
    /// tolerance `open_request_in_tab`'s other callers already have for a
    /// missing target.
    fn open_request_by_id(&mut self, id: Uuid) {
        for c in &self.data.collections {
            if let Some((folder_path, req)) =
                c.flatten_requests().into_iter().find(|(_, r)| r.id == id)
            {
                let collection_id = c.id;
                self.open_request_in_tab(collection_id, folder_path, req);
                return;
            }
        }
    }

    /// Vim-jumplist-style back (`delta = -1`, Ctrl+O) / forward (`delta =
    /// 1`, Ctrl+I) navigation across previously active tabs. Walks
    /// `tab_jump_cursor` in the given direction, skipping any entry whose
    /// tab has since been closed (rather than getting stuck on a stale id),
    /// and stops silently once it runs off either end of the history —
    /// there's nothing further to jump to, not an error.
    fn jump_tab_history(&mut self, delta: isize) {
        let mut cursor = self.tab_jump_cursor as isize;
        loop {
            cursor += delta;
            if cursor < 0 || cursor as usize >= self.tab_jump_history.len() {
                return;
            }
            let candidate_id = self.tab_jump_history[cursor as usize];
            if let Some(idx) = self
                .tabs
                .iter()
                .position(|t| t.current_request.id == candidate_id)
            {
                self.tab_jump_cursor = cursor as usize;
                self.active_tab = idx;
                return;
            }
            // That tab was closed since it was recorded — keep walking the
            // same direction instead of stopping on a dead entry.
        }
    }

    /// Records the currently active tab into `tab_jump_history` if it isn't
    /// already what's recorded at `tab_jump_cursor` — called once, at the
    /// end of every frame, so it captures *every* way the active tab can
    /// change (clicking a tab, opening one from the sidebar, a hotkey jump,
    /// Option+H/L, Ctrl+Tab) without needing each of those call sites to
    /// remember to record it themselves. `jump_tab_history` itself never
    /// triggers a new recording here: it sets `active_tab` to match the
    /// entry already sitting at the new `tab_jump_cursor`, so the comparison
    /// below finds them equal and does nothing.
    fn record_tab_jump_if_changed(&mut self) {
        let Some(current_id) = self.tabs.get(self.active_tab).map(|t| t.current_request.id) else {
            return;
        };
        if self.tab_jump_history.get(self.tab_jump_cursor) == Some(&current_id) {
            return;
        }
        self.tab_jump_history.truncate(self.tab_jump_cursor + 1);
        self.tab_jump_history.push(current_id);
        self.tab_jump_cursor = self.tab_jump_history.len() - 1;
    }

    fn environments_sidebar(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.new_environment_name)
                    .hint_text("Environment name")
                    .desired_width(f32::INFINITY),
            );
            let submitted = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let can_add = !self.new_environment_name.trim().is_empty();
            let clicked = ui
                .add_enabled(
                    can_add,
                    egui::Button::new("+ Environment")
                        .min_size(egui::vec2(ui.available_width(), 0.0)),
                )
                .clicked();
            if can_add && (clicked || submitted) {
                self.data.environments.push(Environment::new(
                    self.new_environment_name.trim().to_string(),
                ));
                self.new_environment_name.clear();
                self.save();
            }
        });

        ui.horizontal(|ui| {
            if ui
                .small_button("📥 Import")
                .on_hover_text("Import a Postman environment (.json)")
                .clicked()
            {
                self.import_postman_environment();
            }
        });
        import_message_banner(ui, &mut self.import_message);
        ui.separator();

        let enter_pressed = handle_list_nav_keys(
            ui,
            self.search_open,
            self.data.environments.len(),
            &mut self.environments_focused_index,
        );

        let mut delete_env: Option<Uuid> = None;
        let mut export_env: Option<Uuid> = None;
        for (i, env) in self.data.environments.iter().enumerate() {
            if enter_pressed && Some(i) == self.environments_focused_index {
                self.active_environment = Some(env.id);
            }
            let mut frame = egui::Frame::new();
            if Some(i) == self.environments_focused_index {
                frame = frame.stroke(egui::Stroke::new(1.0, theme::ACCENT));
            }
            frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    let is_active = self.active_environment == Some(env.id);
                    if ui.selectable_label(is_active, &env.name).clicked() {
                        self.active_environment = Some(env.id);
                    }
                    if ui
                        .small_button("edit")
                        .on_hover_text(format!("Edit {}", env.name))
                        .clicked()
                    {
                        self.central_view = CentralView::EnvironmentEditor(env.id);
                    }
                    if ui
                        .small_button("📤")
                        .on_hover_text(format!(
                            "Export {} as a Postman environment (.json)",
                            env.name
                        ))
                        .clicked()
                    {
                        export_env = Some(env.id);
                    }
                    if icon_button(ui, "🗑", &format!("Delete {}", env.name)).clicked() {
                        delete_env = Some(env.id);
                    }
                });
            });
        }
        if let Some(id) = export_env {
            self.export_environment_to_file(id);
        }
        if let Some(id) = delete_env {
            let name = self
                .data
                .environments
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.name.clone())
                .unwrap_or_default();
            if confirm_delete(&format!("the environment \"{name}\"")) {
                self.data.environments.retain(|e| e.id != id);
                if self.active_environment == Some(id) {
                    self.active_environment = None;
                }
                self.save();
            }
        }
    }

    fn history_sidebar(&mut self, ui: &mut egui::Ui) {
        if ui.button("Clear history").clicked() && confirm_delete("all history") {
            self.data.history.clear();
            self.save();
        }
        ui.separator();
        let enter_pressed = handle_list_nav_keys(
            ui,
            self.search_open,
            self.data.history.len(),
            &mut self.history_focused_index,
        );
        let mut load: Option<usize> = if enter_pressed {
            self.history_focused_index
        } else {
            None
        };
        for (i, entry) in self.data.history.iter().enumerate() {
            let status = match (entry.status, &entry.error) {
                (Some(status), _) => status.to_string(),
                (None, Some(_)) => "ERROR".to_string(),
                (None, None) => "-".to_string(),
            };
            let duration = entry
                .duration_ms
                .map(|ms| format!(" {ms}ms"))
                .unwrap_or_default();
            let label = format!("{} [{}]{}", entry.url, status, duration);
            let mut frame = egui::Frame::new();
            if Some(i) == self.history_focused_index {
                frame = frame.stroke(egui::Stroke::new(1.0, theme::ACCENT));
            }
            frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::method_color(entry.method), entry.method.as_str());
                    if ui.selectable_label(false, label).clicked() {
                        load = Some(i);
                    }
                });
            });
        }
        // Restores both the request definition *and* the exact response
        // that was received at the time, so a history entry is a full
        // replay of what happened — not just a template to resend. Opens in
        // a fresh tab (as an editable Unsaved copy, not linked back to
        // whatever collection it originally came from).
        if let Some(i) = load {
            let entry = self.data.history[i].clone();
            let mut tab = OpenTab::new_unsaved();
            tab.current_request = entry.request;
            tab.response = entry.response;
            tab.response_error = entry.error;
            tab.sent_request = entry.sent_request;
            self.tabs.push(tab);
            self.active_tab = self.tabs.len() - 1;
            self.central_view = CentralView::Request;
        }
    }

    /// The fixed command list plus one `SwitchEnvironment` entry per
    /// environment — rebuilt each frame the palette is open (cheap: a
    /// handful of entries plus one per environment), so a newly-added/
    /// renamed environment shows up immediately without any extra wiring.
    fn palette_actions(&self) -> Vec<(String, PaletteAction)> {
        let mut actions = vec![
            ("New Tab".to_string(), PaletteAction::NewTab),
            ("Close Tab".to_string(), PaletteAction::CloseActiveTab),
            ("Save Request".to_string(), PaletteAction::SaveActiveRequest),
            ("Open Settings".to_string(), PaletteAction::OpenSettings),
            (
                "Open Collection Runner".to_string(),
                PaletteAction::OpenRunner,
            ),
            (
                "Switch to Collections".to_string(),
                PaletteAction::SwitchSidebarTab(SidebarTab::Collections),
            ),
            (
                "Switch to Environments".to_string(),
                PaletteAction::SwitchSidebarTab(SidebarTab::Environments),
            ),
            (
                "Switch to History".to_string(),
                PaletteAction::SwitchSidebarTab(SidebarTab::History),
            ),
            (
                "Toggle Split Direction".to_string(),
                PaletteAction::ToggleSplitDirection,
            ),
            (
                "Theme: Light".to_string(),
                PaletteAction::SetTheme(ThemeMode::Light),
            ),
            (
                "Theme: Dark".to_string(),
                PaletteAction::SetTheme(ThemeMode::Dark),
            ),
            (
                "Theme: System".to_string(),
                PaletteAction::SetTheme(ThemeMode::System),
            ),
            (
                "Environment: No Environment".to_string(),
                PaletteAction::SwitchEnvironment(None),
            ),
        ];
        for env in &self.data.environments {
            actions.push((
                format!("Environment: {}", env.name),
                PaletteAction::SwitchEnvironment(Some(env.id)),
            ));
        }
        actions
    }

    /// Every `PaletteAction` is a thin wrapper over a field assignment that
    /// already exists elsewhere (a button, a shortcut) — this is the one
    /// place that actually performs it.
    fn perform_palette_action(&mut self, action: PaletteAction) {
        match action {
            PaletteAction::NewTab => self.new_blank_tab(),
            PaletteAction::CloseActiveTab => self.close_tab_with_confirmation(self.active_tab),
            PaletteAction::SaveActiveRequest => self.save_current_request(),
            PaletteAction::OpenSettings => self.central_view = CentralView::Settings,
            PaletteAction::OpenRunner => self.central_view = CentralView::Runner,
            PaletteAction::SwitchSidebarTab(tab) => self.sidebar_tab = tab,
            PaletteAction::SwitchEnvironment(id) => self.active_environment = id,
            PaletteAction::ToggleSplitDirection => {
                self.split_direction = match self.split_direction {
                    SplitDirection::Vertical => SplitDirection::Horizontal,
                    SplitDirection::Horizontal => SplitDirection::Vertical,
                };
            }
            PaletteAction::SetTheme(mode) => self.settings.theme = mode,
        }
    }

    // ---------- UI: leader-key ("space s f") command palette ----------
    fn command_palette(&mut self, ctx: &egui::Context) {
        if !self.search_open {
            return;
        }

        let mut still_open = true;
        let mut selected: Option<PaletteEntry> = None;
        let mut close = false;

        let title = match self.palette_scope {
            PaletteScope::RequestsOnly => "Search Requests",
            PaletteScope::CommandsOnly => "Search Commands",
            PaletteScope::Combined => "Search Requests & Commands",
        };
        egui::Window::new(title)
            .id(egui::Id::new("search_palette_window"))
            .open(&mut still_open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 80.0))
            .default_width(520.0)
            .show(ctx, |ui| {
                let hint = match self.palette_scope {
                    PaletteScope::RequestsOnly => "Search requests\u{2026}",
                    PaletteScope::CommandsOnly => "Search commands\u{2026}",
                    PaletteScope::Combined => "Search requests or type a command\u{2026}",
                };
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.search_query)
                        .hint_text(hint)
                        .desired_width(f32::INFINITY),
                );
                if self.search_needs_focus {
                    response.request_focus();
                    self.search_needs_focus = false;
                }
                // Tracks whether `palette_selected` actually moved this
                // frame (typing a new query resets it to the top; Ctrl+N/P
                // below can also change it) — the scroll area only snaps
                // the selected row into view on a frame where this is true,
                // not unconditionally every frame, so it doesn't fight a
                // manual mouse-wheel scroll through the list.
                let mut selection_moved = response.changed();
                if response.changed() {
                    self.palette_selected = 0;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }

                ui.separator();

                let query = self.search_query.trim().to_string();

                // Every candidate (actions and requests alike) is fuzzy-
                // scored and the whole list sorted by score, rather than
                // the old "actions first, then requests" fixed grouping —
                // real fzf-style pickers rank by relevance regardless of
                // kind. The *stable* sort still means an empty query (every
                // score tied at 0) preserves today's original insertion
                // order, so "show everything, unranked" looks the same as
                // before; only an actual query reorders anything.
                // `<leader>sf` (requests only) and `<leader>sc` (commands
                // only) each skip *scoring* the other half of the list
                // entirely, rather than scoring-then-hiding it, so neither
                // can leak in via a lucky fuzzy match.
                let mut scored: Vec<(u32, PaletteEntry)> =
                    if self.palette_scope == PaletteScope::RequestsOnly {
                        Vec::new()
                    } else {
                        self.palette_actions()
                            .into_iter()
                            .filter_map(|(label, action)| {
                                let score = fuzzy_score(&mut self.fuzzy_matcher, &label, &query)?;
                                Some((score, PaletteEntry::Action { label, action }))
                            })
                            .collect()
                    };

                if self.palette_scope != PaletteScope::CommandsOnly {
                    for c in &self.data.collections {
                        for r in &c.requests {
                            if let Some(score) =
                                fuzzy_score_request(&mut self.fuzzy_matcher, r, &query)
                            {
                                scored.push((
                                    score,
                                    PaletteEntry::Request {
                                        collection: c.id,
                                        folder_path: Vec::new(),
                                        item: Box::new(r.clone()),
                                    },
                                ));
                            }
                        }
                        let mut path = Vec::new();
                        let mut matches = Vec::new();
                        collect_matching_requests(
                            &c.folders,
                            &mut path,
                            &query,
                            c.id,
                            &mut self.fuzzy_matcher,
                            &mut matches,
                        );
                        scored.extend(matches.into_iter().map(
                            |(score, collection, folder_path, item)| {
                                (
                                    score,
                                    PaletteEntry::Request {
                                        collection,
                                        folder_path,
                                        item: Box::new(item.clone()),
                                    },
                                )
                            },
                        ));
                    }
                }
                scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
                scored.truncate(50);
                let entries: Vec<PaletteEntry> = scored.into_iter().map(|(_, e)| e).collect();

                // Emacs-style Ctrl+N/Ctrl+P move the highlighted entry,
                // wrapping at both ends — the palette had no selection
                // cursor at all before this (Enter always picked the very
                // first entry, regardless of what was visibly on top).
                if !entries.is_empty() {
                    if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::N)) {
                        self.palette_selected = (self.palette_selected + 1) % entries.len();
                        selection_moved = true;
                    }
                    if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::P)) {
                        self.palette_selected =
                            (self.palette_selected + entries.len() - 1) % entries.len();
                        selection_moved = true;
                    }
                }
                self.palette_selected = self.palette_selected.min(entries.len().saturating_sub(1));

                let enter_pressed =
                    response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if enter_pressed && let Some(picked) = entries.get(self.palette_selected) {
                    selected = Some(picked.clone());
                }

                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        if entries.is_empty() {
                            ui.weak("No matching requests or commands.");
                        }
                        for (i, entry) in entries.iter().enumerate() {
                            // A hand-rolled composite row rather than a plain
                            // `selectable_label(i == self.palette_selected, label)`:
                            // a request entry's method needs its own color
                            // (`theme::method_color`), and a `colored_label`
                            // sitting next to a `selectable_label` wouldn't
                            // share its selection background — painting the
                            // background on a wrapping `Frame` first, then
                            // making that whole frame clickable via
                            // `.interact(Sense::click())`, keeps the
                            // Ctrl+N/P highlight covering the full row.
                            let fill = if i == self.palette_selected {
                                ui.visuals().selection.bg_fill
                            } else {
                                egui::Color32::TRANSPARENT
                            };
                            let row = egui::Frame::new()
                                .fill(fill)
                                .inner_margin(egui::Margin::symmetric(4, 2))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| match entry {
                                        PaletteEntry::Action { label, .. } => {
                                            ui.label(format!("> {label}"));
                                        }
                                        PaletteEntry::Request { item, .. } => {
                                            ui.colored_label(
                                                theme::method_color(item.method),
                                                item.method.as_str(),
                                            );
                                            ui.label(&item.name);
                                            ui.weak("—");
                                            ui.weak(&item.url);
                                        }
                                    });
                                });
                            if row.response.interact(egui::Sense::click()).clicked() {
                                selected = Some(entry.clone());
                            }
                            // Real, reported bug: Ctrl+N/P moved the
                            // highlight but never scrolled the list, so
                            // once the selection went past the visible
                            // rows it kept moving invisibly off-screen.
                            // Only snaps into view on the frame the
                            // selection actually changed (see
                            // `selection_moved`'s own comment) so it
                            // doesn't fight a manual mouse-wheel scroll.
                            if i == self.palette_selected && selection_moved {
                                row.response.scroll_to_me(Some(egui::Align::Center));
                            }
                        }
                    });
            });

        if let Some(entry) = selected {
            match entry {
                PaletteEntry::Request {
                    collection,
                    folder_path,
                    item,
                } => {
                    self.open_request_in_tab(collection, folder_path, *item);
                }
                PaletteEntry::Action { action, .. } => self.perform_palette_action(action),
            }
            close = true;
        }

        self.search_open = still_open && !close;
    }

    // ---------- UI: central ----------
    fn central(&mut self, ui: &mut egui::Ui) {
        // Cloned once up front: `CentralView` isn't `Copy` (`FolderEditor`
        // carries an owned `Vec<Uuid>`), and matching `self.central_view`
        // by value directly would try to move out of `&mut self`. The arms
        // below call `self.*_editor(...)` methods, which need `self` free.
        let view = self.central_view.clone();
        egui::CentralPanel::default().show(ui, |ui| match view {
            CentralView::Request => {
                self.tab_bar(ui);
                ui.separator();
                match self.split_direction {
                    SplitDirection::Vertical => {
                        // Rounded to a whole pixel: an unrounded fraction
                        // here (more likely now that the Console drawer can
                        // also be eating a slice of `available_height`)
                        // trips egui's debug-only `show_unaligned` overlay
                        // in `cargo test`/dev builds — harmless in a real
                        // `--release` binary, but worth avoiding for a
                        // clean snapshot baseline.
                        let height = (ui.available_height() * 0.55).round();
                        egui::Panel::top("request_editor_panel_v")
                            .resizable(true)
                            .default_size(height)
                            .min_size(280.0)
                            .show(ui, |ui| self.request_editor(ui));
                        egui::CentralPanel::default().show(ui, |ui| self.response_viewer(ui));
                    }
                    SplitDirection::Horizontal => {
                        let width = (ui.available_width() * 0.5).round();
                        egui::Panel::left("request_editor_panel_h")
                            .resizable(true)
                            .default_size(width)
                            .min_size(320.0)
                            .show(ui, |ui| self.request_editor(ui));
                        egui::CentralPanel::default().show(ui, |ui| self.response_viewer(ui));
                    }
                }
            }
            CentralView::EnvironmentEditor(id) => self.environment_editor(ui, id),
            CentralView::Settings => self.settings_panel(ui),
            CentralView::Runner => self.runner_panel(ui),
            CentralView::CollectionEditor(id) => self.collection_editor(ui, id),
            CentralView::FolderEditor {
                collection,
                folder_path,
            } => self.folder_editor(ui, collection, &folder_path),
        });
    }

    /// Postman-styled tab strip: one chip per open tab (method badge colored
    /// per Postman's own per-method palette, name, unsaved dot, close ×),
    /// scrollable horizontally once there are more tabs than fit, plus a
    /// trailing "+" to open a new blank tab. Drag-to-reorder uses the same
    /// `dnd_drag_source`/`dnd_drop_zone` pattern as the collection tree
    /// (Phase 3), just keyed by tab index instead of a tree path.
    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut close_idx: Option<usize> = None;
        let mut select_idx: Option<usize> = None;
        let mut reorder: Option<(usize, usize)> = None;
        // Same reverse lookup `collections_sidebar` uses for its own
        // `"[key]"` badge — computed once per frame so every open tab's
        // chip can show its own assigned hotkey too, not just the sidebar
        // row.
        let hotkeys_by_request = model::reverse_hotkey_bindings(&self.data.hotkey_bindings);

        // egui's default scroll style is "floating" — the bar only reserves
        // space while actively being dragged, and otherwise overlaps the
        // content on hover (reported: hovering the scrollbar covered the
        // tab chips underneath it). Scoped to just this scroll area, not a
        // global style change, since only the tab bar was reported.
        ui.scope(|ui| {
            ui.spacing_mut().scroll.floating = false;
            egui::ScrollArea::horizontal()
                .id_salt("tab_bar_scroll")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for (idx, tab) in self.tabs.iter().enumerate() {
                            let drag_id = egui::Id::new(("open_tab", idx));
                            // The active tab's chip gets an accent-colored border —
                            // egui's `Frame` strokes the whole rectangle rather
                            // than one edge, so this approximates Postman's own
                            // colored active-tab underline as a full outline
                            // instead of a broken attempt at a partial one.
                            let mut chip_frame = egui::Frame::group(ui.style());
                            if idx == self.active_tab {
                                chip_frame =
                                    chip_frame.stroke(egui::Stroke::new(1.5, theme::ACCENT));
                            }
                            let (zone, dropped) = ui.dnd_drop_zone::<usize, _>(chip_frame, |ui| {
                                ui.horizontal(|ui| {
                                    // A dedicated drag handle, separate from the
                                    // selectable label/close button below: wrapping
                                    // the *whole* chip in `dnd_drag_source` (as this
                                    // did originally) intercepts plain clicks — its
                                    // own `Sense::drag()` interact sits on top of
                                    // the label's `Sense::click()` and swallows the
                                    // click before it reaches it, so a tab could
                                    // never be selected. Same bug, same fix, as the
                                    // collection-tree rows in Phase 3.
                                    ui.dnd_drag_source(drag_id, idx, |ui| {
                                        ui.weak("::");
                                    });
                                    let method_text = tab.current_request.method.as_str();
                                    let color = theme::method_color(tab.current_request.method);
                                    ui.colored_label(color, method_text);
                                    let name = if tab.current_request.name.is_empty() {
                                        "Untitled Request"
                                    } else {
                                        tab.current_request.name.as_str()
                                    };
                                    // VS Code-style preview tab: italicized
                                    // name, same visual convention that app
                                    // uses to mark "this will be replaced by
                                    // the next thing you preview, until you
                                    // pin it" (Enter, or any real open).
                                    let name_text: egui::WidgetText = if tab.is_preview {
                                        egui::RichText::new(name).italics().into()
                                    } else {
                                        name.into()
                                    };
                                    let selected = ui
                                        .selectable_label(idx == self.active_tab, name_text)
                                        .clicked();
                                    if let Some(key) =
                                        hotkeys_by_request.get(&tab.current_request.id)
                                    {
                                        ui.weak(format!("[{key}]"));
                                    }
                                    if tab.is_dirty() {
                                        // Plain ASCII "*" rather than "●"
                                        // (U+25CF): egui's bundled font doesn't
                                        // cover it, rendering as an empty tofu
                                        // box — same class of missing-glyph
                                        // issue hit (and fixed the same way) in
                                        // Phases 2, 3, and 7.
                                        ui.weak("*");
                                    }
                                    if ui.small_button("\u{d7}").clicked() {
                                        close_idx = Some(idx);
                                    }
                                    if selected {
                                        select_idx = Some(idx);
                                    }
                                });
                            });
                            if let Some(dropped) = dropped {
                                reorder = Some((*dropped, idx));
                            }
                            // Only scroll on the frame the active tab actually
                            // changed (not every frame) — otherwise this would
                            // fight a manual horizontal scroll through the bar.
                            if idx == self.active_tab && self.active_tab != self.tab_bar_scrolled_to
                            {
                                zone.response.scroll_to_me(Some(egui::Align::Center));
                            }
                        }
                        if ui
                            .button("+")
                            .on_hover_text("New tab (Cmd/Ctrl+T)")
                            .clicked()
                        {
                            self.new_blank_tab();
                        }
                    });
                });
        });

        if let Some(idx) = select_idx {
            self.active_tab = idx;
        }
        if let Some(idx) = close_idx {
            self.close_tab_with_confirmation(idx);
        }
        if let Some((from, to)) = reorder
            && from != to
            && from < self.tabs.len()
        {
            let tab = self.tabs.remove(from);
            let to = to.min(self.tabs.len());
            self.tabs.insert(to, tab);
            // Keep following whichever tab was active through the reorder.
            self.active_tab = if self.active_tab == from {
                to
            } else if from < self.active_tab && self.active_tab <= to {
                self.active_tab - 1
            } else if to <= self.active_tab && self.active_tab < from {
                self.active_tab + 1
            } else {
                self.active_tab
            };
        }
        self.tab_bar_scrolled_to = self.active_tab;
    }

    fn request_editor(&mut self, ui: &mut egui::Ui) {
        // Cmd/Ctrl+Enter is the more standard "send" convention (Postman
        // itself uses it) — added alongside the original Alt+Enter rather
        // than replacing it, since that one already works and costs
        // nothing to keep.
        let send_shortcut = ui.input_mut(|i| i.consume_key(egui::Modifiers::ALT, egui::Key::Enter))
            || ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
        if send_shortcut && !self.active_tab().is_loading {
            self.send_current_request();
        }

        let mut save_clicked = false;
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.tabs[self.active_tab].current_request.name);
            if ui.button("Save").clicked() {
                save_clicked = true;
            }
        });
        if save_clicked {
            self.save_current_request();
        }

        let mut send_clicked = false;
        ui.horizontal(|ui| {
            let tab = &mut self.tabs[self.active_tab];
            egui::ComboBox::from_id_salt("method_combo")
                .selected_text(tab.current_request.method.as_str())
                .show_ui(ui, |ui| {
                    for m in Method::ALL {
                        ui.selectable_value(&mut tab.current_request.method, m, m.as_str());
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut tab.current_request.url)
                    .hint_text("https://api.example.com/{{path}}")
                    .desired_width(ui.available_width() - 80.0),
            );
            // The one widget in the app with an explicit color override on
            // top of the global accent styling (`theme::apply_postman_visuals`)
            // — Postman's own Send button is the single most visually
            // emphasized control in its whole UI, so this one gets a solid
            // fill rather than just the accent-colored outline every other
            // button gets.
            let send = egui::Button::new(egui::RichText::new("Send").color(egui::Color32::WHITE))
                .fill(theme::ACCENT);
            if ui.add(send).on_hover_text("Option+Enter").clicked() {
                send_clicked = true;
            }
        });
        ui.horizontal(|ui| {
            let tab = &mut self.tabs[self.active_tab];
            ui.label("Timeout (ms):");
            let mut timeout_text = tab
                .current_request
                .timeout_ms
                .map(|ms| ms.to_string())
                .unwrap_or_default();
            let response = ui.add(
                egui::TextEdit::singleline(&mut timeout_text)
                    .hint_text("default")
                    .desired_width(80.0),
            );
            if response.changed() {
                tab.current_request.timeout_ms = if timeout_text.trim().is_empty() {
                    None
                } else {
                    timeout_text.trim().parse().ok()
                };
            }
        });
        if send_clicked {
            self.send_current_request();
        }
        if self.active_tab().is_loading {
            ui.spinner();
        }

        ui.separator();
        let tab = &mut self.tabs[self.active_tab];
        // Live counts, same "(N)" convention the Tests tab already uses for
        // "Tests (passed/total)" below — only enabled rows count, matching
        // what actually gets sent.
        let params_count = tab
            .current_request
            .params
            .iter()
            .filter(|kv| kv.enabled)
            .count();
        let params_label = if params_count > 0 {
            format!("Params ({params_count})")
        } else {
            "Params".to_string()
        };
        let headers_count = tab
            .current_request
            .headers
            .iter()
            .filter(|kv| kv.enabled)
            .count();
        let headers_label = if headers_count > 0 {
            format!("Headers ({headers_count})")
        } else {
            "Headers".to_string()
        };
        ui.horizontal(|ui| {
            // Order matches Postman's own request-tab strip: Params,
            // Authorization, Headers, Body, then scripts — Auth used to sit
            // after Headers/Body, which didn't match. "Code" has no Postman
            // equivalent, so it stays appended at the end.
            ui.selectable_value(&mut tab.request_tab, RequestTab::Params, params_label);
            ui.selectable_value(&mut tab.request_tab, RequestTab::Auth, "Authorization");
            ui.selectable_value(&mut tab.request_tab, RequestTab::Headers, headers_label);
            ui.selectable_value(&mut tab.request_tab, RequestTab::Body, "Body");
            ui.selectable_value(
                &mut tab.request_tab,
                RequestTab::PreRequestScript,
                "Pre-request Script",
            );
            ui.selectable_value(&mut tab.request_tab, RequestTab::TestsScript, "Tests");
            ui.selectable_value(&mut tab.request_tab, RequestTab::Code, "Code");
        });
        ui.separator();

        // Computed once here (rather than via `self.effective_auth()` inside
        // the closure below) — same borrow-checker reasoning as Phase 7's
        // `effective_response`/`effective_sent_request`: the match arms
        // below borrow `self.tabs[self.active_tab]` fields directly, which
        // would conflict with a method call needing the whole `self`.
        let effective_auth = self.effective_auth(self.active_tab());

        egui::ScrollArea::vertical()
            .id_salt("request_editor_scroll")
            .show(ui, |ui| match self.tabs[self.active_tab].request_tab {
                RequestTab::Params => {
                    let tab = &mut self.tabs[self.active_tab];
                    ui.label("Description:");
                    ui.add(
                        egui::TextEdit::multiline(&mut tab.current_request.description)
                            .hint_text("What does this request do?")
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(8.0);
                    ui.label("Query Params:");
                    key_value_table(ui, "params_table", &mut tab.current_request.params);
                    let url = tab.current_request.url.clone();
                    path_params_editor(ui, &url, &mut tab.current_request.path_params);
                }
                RequestTab::Headers => {
                    key_value_table(ui, "headers_table", &mut self.tabs[self.active_tab].current_request.headers)
                }
                RequestTab::Auth => {
                    let tab = &mut self.tabs[self.active_tab];
                    auth_editor(ui, &mut tab.current_request.auth, &effective_auth);
                }
                RequestTab::Body => {
                    let tab = &mut self.tabs[self.active_tab];
                    let body = &mut tab.current_request.body;
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut body.mode, BodyMode::None, "None");
                        ui.selectable_value(&mut body.mode, BodyMode::Json, "JSON");
                        ui.selectable_value(&mut body.mode, BodyMode::Raw, "Raw");
                        ui.selectable_value(&mut body.mode, BodyMode::Form, "Form");
                        ui.selectable_value(&mut body.mode, BodyMode::Multipart, "Form-data");
                        ui.selectable_value(&mut body.mode, BodyMode::Binary, "Binary");
                    });
                    if !matches!(
                        body.mode,
                        BodyMode::None | BodyMode::Form | BodyMode::Multipart | BodyMode::Binary
                    ) {
                        ui.horizontal(|ui| {
                            ui.label("Find:");
                            ui.add(
                                egui::TextEdit::singleline(&mut tab.request_body_find)
                                    .hint_text("search in body")
                                    .desired_width(200.0),
                            );
                            if !tab.request_body_find.is_empty() {
                                let n = syntax::find_matches(&body.raw, &tab.request_body_find).len();
                                ui.weak(format!("{n} match{}", if n == 1 { "" } else { "es" }));
                            }
                        });
                    }
                    // Only meaningful for Raw — Json mode's body is already
                    // unambiguously JSON, nothing to override.
                    if body.mode == BodyMode::Raw {
                        ui.horizontal(|ui| {
                            ui.label("Format:");
                            egui::ComboBox::from_id_salt("request_body_language")
                                .selected_text(tab.request_body_language.label())
                                .show_ui(ui, |ui| {
                                    for opt in syntax::LanguageOverride::ALL {
                                        ui.selectable_value(
                                            &mut tab.request_body_language,
                                            opt,
                                            opt.label(),
                                        );
                                    }
                                });
                        });
                    }
                    match body.mode {
                        BodyMode::None => {
                            ui.label("This request has no body.");
                        }
                        BodyMode::Json | BodyMode::Raw => {
                            let language = match body.mode {
                                BodyMode::Json => syntax::Language::Json,
                                _ => tab
                                    .request_body_language
                                    .resolve(|| syntax::sniff_language(&body.raw)),
                            };
                            let dark = ui.visuals().dark_mode;
                            let font_id = egui::TextStyle::Monospace.resolve(ui.style());
                            let search_query = tab.request_body_find.clone();
                            let freeze_wrap = self
                                .resize_settle_deadline
                                .is_some_and(|deadline| std::time::Instant::now() < deadline);
                            let mut layouter =
                                move |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
                                    let wrap_width = effective_wrap_width(
                                        ui.ctx(),
                                        egui::Id::new("request_body_wrap"),
                                        wrap_width,
                                        freeze_wrap,
                                    );
                                    let mut job = syntax::highlight_cached(
                                        ui.ctx(),
                                        egui::Id::new("request_body_highlight"),
                                        dark,
                                        font_id.clone(),
                                        buf.as_str(),
                                        language,
                                        &search_query,
                                    );
                                    job.wrap.max_width = wrap_width;
                                    ui.fonts_mut(|f| f.layout_job(job))
                                };
                            let content_snapshot = body.raw.clone();
                            show_code_editor_with_links(
                                ui,
                                egui::Id::new("request_body_raw_editor"),
                                &content_snapshot,
                                &mut body.raw,
                                self.settings.vim_mode_enabled,
                                false,
                                |te| {
                                    te.code_editor()
                                        .desired_rows(10)
                                        .desired_width(f32::INFINITY)
                                        .layouter(&mut layouter)
                                },
                            );
                        }
                        BodyMode::Form => {
                            key_value_table(ui, "form_table", &mut body.form);
                        }
                        BodyMode::Multipart => {
                            multipart_form_table(ui, "multipart_table", &mut body.multipart);
                        }
                        BodyMode::Binary => {
                            binary_body_picker(ui, &mut body.binary_file_path);
                        }
                    }
                }
                RequestTab::PreRequestScript => {
                    let tab = &mut self.tabs[self.active_tab];
                    ui.label("Runs before the request is sent. Available: pm.environment/pm.globals/pm.collectionVariables .get/set(key, value), pm.variables.get(key), pm.request.url/method/body, pm.request:getHeader/setHeader(key, value), pm.sendRequest(urlOrTable, function(err, response) ... end), console.log(...).");
                    snippet_buttons(ui, PRE_REQUEST_SNIPPETS, &mut tab.current_request.pre_request_script);
                    script_editor(
                        ui,
                        "pre_request_script_editor",
                        &mut tab.current_request.pre_request_script,
                        self.settings.vim_mode_enabled,
                    );
                }
                RequestTab::TestsScript => {
                    let tab = &mut self.tabs[self.active_tab];
                    ui.label("Runs after the response arrives. Available: pm.response.status/body/duration_ms/error, pm.response:getHeader(key), pm.response:json(), pm.environment/pm.globals/pm.collectionVariables .get/set(key, value), pm.variables.get(key), pm.test(name, function() ... end), pm.sendRequest(urlOrTable, function(err, response) ... end), console.log(...).");
                    snippet_buttons(ui, TEST_SNIPPETS, &mut tab.current_request.post_response_script);
                    script_editor(
                        ui,
                        "tests_script_editor",
                        &mut tab.current_request.post_response_script,
                        self.settings.vim_mode_enabled,
                    );
                }
                RequestTab::Code => {
                    egui::ComboBox::from_id_salt("codegen_target_combo")
                        .selected_text(self.codegen_target.label())
                        .show_ui(ui, |ui| {
                            for target in codegen::CodeGenTarget::ALL {
                                ui.selectable_value(&mut self.codegen_target, target, target.label());
                            }
                        });
                    ui.add_space(4.0);
                    // Raw request text, `{{variable}}` placeholders intact
                    // (matches Postman's own snippet behavior — see the
                    // module doc comment on `codegen.rs`), plus whatever
                    // auth actually applies once `Inherit` is resolved.
                    let snippet =
                        codegen::generate_snippet(&self.tabs[self.active_tab].current_request, &effective_auth, self.codegen_target);
                    let mut snippet_display = snippet.clone();
                    ui.add(
                        egui::TextEdit::multiline(&mut snippet_display)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .interactive(false),
                    );
                    ui.add_space(4.0);
                    if ui.button("Copy snippet").clicked() {
                        ui.ctx().copy_text(snippet);
                    }
                }
            });
    }

    /// Writes tab `tab_index`'s `current_request` into its slot under
    /// `data.collections` if its `origin` points at one. Returns `false`
    /// (a no-op) when the request hasn't been saved anywhere yet
    /// (`RequestOrigin::Unsaved`).
    fn sync_tab_into_data(&mut self, tab_index: usize) -> bool {
        let (collection, folder_path, request) = match &self.tabs[tab_index].origin {
            RequestOrigin::Collection {
                collection,
                folder_path,
                request,
            } => (*collection, folder_path.clone(), *request),
            RequestOrigin::Unsaved => return false,
        };
        let req = self.tabs[tab_index].current_request.clone();
        let Some(c) = self
            .data
            .collections
            .iter_mut()
            .find(|c| c.id == collection)
        else {
            return false;
        };
        let list = c.requests_at_mut_or_root(&folder_path);
        if let Some(existing) = list.iter_mut().find(|r| r.id == request) {
            *existing = req;
        } else {
            list.push(req);
        }
        true
    }

    /// Explicit "Save" button: parks a never-saved request into a default
    /// "Saved Requests" collection, or otherwise just writes it back in
    /// place. Always targets the active tab.
    fn save_current_request(&mut self) {
        if matches!(self.active_tab().origin, RequestOrigin::Unsaved) {
            let req = self.active_tab().current_request.clone();
            let target = if let Some(c) = self
                .data
                .collections
                .iter_mut()
                .find(|c| c.name == "Saved Requests")
            {
                c
            } else {
                self.data
                    .collections
                    .push(Collection::new("Saved Requests"));
                self.data.collections.last_mut().unwrap()
            };
            let target_id = target.id;
            target.requests.push(req.clone());
            self.active_tab_mut().origin = RequestOrigin::Collection {
                collection: target_id,
                folder_path: Vec::new(),
                request: req.id,
            };
        } else {
            self.sync_tab_into_data(self.active_tab);
        }
        self.save();
        let tab = self.active_tab_mut();
        tab.autosave_last_synced = Some(tab.current_request.clone());
        tab.autosave_last_saved_at = Some(std::time::Instant::now());
    }

    /// Whether any *collection-backed* tab has edits pending within the
    /// autosave debounce window — the one case worth keeping the UI
    /// repainting for (see `fn ui`'s own use of this). Deliberately
    /// excludes `Unsaved`-origin tabs: `autosave_if_dirty` never writes them
    /// (there's no file slot to write into), so their `is_dirty()` is
    /// permanently `true` and would otherwise mean "keep repainting
    /// forever" for the entire lifetime of any app that has one open — a
    /// real, measured bug (confirmed via `ps`/`vmmap`: ~20-24% CPU at true
    /// idle, and a GPU/graphics memory footprint of ~170MB+ that never got
    /// to settle back down, vs. 0% CPU and ~5MB once fixed), not a
    /// hypothetical one.
    fn has_savable_dirty_tab(&self) -> bool {
        self.tabs
            .iter()
            .any(|t| !matches!(t.origin, RequestOrigin::Unsaved) && t.is_dirty())
    }

    /// Called every frame: transparently persists edits to every open,
    /// already-saved tab (not just the active one — a background tab's
    /// edits should still autosave) without needing the Save button. Writes
    /// are rate-limited per tab so continuous typing doesn't hit disk every
    /// frame; a never-saved tab is left alone since there's no collection
    /// slot to write into yet (use the Save button once to pick one).
    fn autosave_if_dirty(&mut self) {
        for idx in 0..self.tabs.len() {
            if matches!(self.tabs[idx].origin, RequestOrigin::Unsaved) {
                continue;
            }
            if !self.tabs[idx].is_dirty() {
                continue;
            }
            let now = std::time::Instant::now();
            let throttle = std::time::Duration::from_millis(400);
            let ready = self.tabs[idx]
                .autosave_last_saved_at
                .is_none_or(|t| now.duration_since(t) >= throttle);
            if ready {
                if self.sync_tab_into_data(idx) {
                    self.save();
                }
                let tab = &mut self.tabs[idx];
                tab.autosave_last_synced = Some(tab.current_request.clone());
                tab.autosave_last_saved_at = Some(now);
            }
        }
    }

    /// The "Save as Example" button plus, once at least one exists, a row of
    /// chips (one per saved example) shown directly above the response tab
    /// bar. Clicking a chip toggles `viewing_example` (see
    /// `effective_response` etc.); a right-click context menu offers
    /// Rename/Delete, matching the collection tree's rename convention
    /// (`renaming: Option<(Uuid, String)>` — reused here since example IDs
    /// never collide with collection/folder/request IDs, so the two rename
    /// flows can safely share the one field without interfering).
    fn saved_examples_strip(&mut self, ui: &mut egui::Ui) {
        let tab = &self.tabs[self.active_tab];
        let has_live_response = tab.response.is_some() || tab.response_error.is_some();
        if !has_live_response && tab.current_request.saved_examples.is_empty() {
            return;
        }

        let mut just_saved = false;
        ui.horizontal(|ui| {
            let tab = &mut self.tabs[self.active_tab];
            if let Some(name) = &mut tab.saving_example_name {
                let resp = ui.add(
                    egui::TextEdit::singleline(name)
                        .hint_text("Example name")
                        .desired_width(160.0),
                );
                if resp.lost_focus() {
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        let new_name = name.trim().to_string();
                        if !new_name.is_empty() {
                            tab.current_request.saved_examples.push(SavedExample {
                                id: Uuid::new_v4(),
                                name: new_name,
                                timestamp: chrono::Utc::now(),
                                sent_request: tab.sent_request.clone(),
                                response: tab.response.clone(),
                                error: tab.response_error.clone(),
                            });
                            just_saved = true;
                        }
                    }
                    tab.saving_example_name = None;
                } else {
                    resp.request_focus();
                }
            } else if ui
                .add_enabled(has_live_response, egui::Button::new("💾 Save as Example"))
                .on_hover_text("Attach the current response to this request as a named example")
                .clicked()
            {
                tab.saving_example_name = Some(String::new());
            }
        });
        if just_saved {
            self.sync_tab_into_data(self.active_tab);
            self.save();
        }

        if self.tabs[self.active_tab]
            .current_request
            .saved_examples
            .is_empty()
        {
            return;
        }

        enum ExampleAction {
            View(Uuid),
            Delete(Uuid),
            StartRename(Uuid),
            Rename(Uuid, String),
        }
        let mut action = None;
        // Same "take, render, put back" dance as the collection tree's own
        // `renaming` handling (see `collections_sidebar`) — shared field.
        let mut renaming = self.renaming.take();
        ui.horizontal_wrapped(|ui| {
            for example in &self.tabs[self.active_tab].current_request.saved_examples {
                if renaming.as_ref().is_some_and(|(id, _)| *id == example.id) {
                    let (_, buf) = renaming.as_mut().unwrap();
                    let resp = ui.add(egui::TextEdit::singleline(buf).desired_width(120.0));
                    if resp.lost_focus() {
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let new_name = buf.trim().to_string();
                            if !new_name.is_empty() {
                                action = Some(ExampleAction::Rename(example.id, new_name));
                            }
                        }
                        renaming = None;
                    } else {
                        resp.request_focus();
                    }
                    continue;
                }

                let selected = self.tabs[self.active_tab].viewing_example == Some(example.id);
                let resp = ui
                    .selectable_label(selected, &example.name)
                    .on_hover_text(example.timestamp.to_rfc3339());
                if resp.clicked() {
                    action = Some(ExampleAction::View(example.id));
                }
                resp.context_menu(|ui| {
                    if ui.button("Rename").clicked() {
                        action = Some(ExampleAction::StartRename(example.id));
                        ui.close();
                    }
                    if ui.button("Delete").clicked() {
                        action = Some(ExampleAction::Delete(example.id));
                        ui.close();
                    }
                });
            }
        });
        self.renaming = renaming;

        match action {
            Some(ExampleAction::View(id)) => {
                let tab = self.active_tab_mut();
                tab.viewing_example = if tab.viewing_example == Some(id) {
                    None
                } else {
                    Some(id)
                };
            }
            Some(ExampleAction::Delete(id)) => {
                let tab = self.active_tab_mut();
                let name = tab
                    .current_request
                    .saved_examples
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| e.name.clone())
                    .unwrap_or_default();
                if confirm_delete(&format!("the saved example \"{name}\"")) {
                    let tab = self.active_tab_mut();
                    tab.current_request.saved_examples.retain(|e| e.id != id);
                    if tab.viewing_example == Some(id) {
                        tab.viewing_example = None;
                    }
                    self.sync_tab_into_data(self.active_tab);
                    self.save();
                }
            }
            Some(ExampleAction::StartRename(id)) => {
                let name = self.tabs[self.active_tab]
                    .current_request
                    .saved_examples
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| e.name.clone())
                    .unwrap_or_default();
                self.renaming = Some((id, name));
            }
            Some(ExampleAction::Rename(id, new_name)) => {
                if let Some(e) = self.tabs[self.active_tab]
                    .current_request
                    .saved_examples
                    .iter_mut()
                    .find(|e| e.id == id)
                {
                    e.name = new_name;
                }
                self.sync_tab_into_data(self.active_tab);
                self.save();
            }
            None => {}
        }
    }

    /// The response currently shown in the panel: a saved example's, when
    /// `viewing_example` points at one, otherwise the live response from the
    /// last send. Lets the `Body`/`Headers`/`Request`/status-line code stay
    /// unaware of which mode it's in. Takes `tab` explicitly (rather than
    /// reading `self.active_tab()` itself) for the same reason
    /// `effective_auth` does — callers need to hold this result while also
    /// mutably borrowing tab fields elsewhere.
    fn effective_response(tab: &OpenTab) -> Option<&HttpResponse> {
        match tab.viewing_example {
            Some(id) => tab
                .current_request
                .saved_examples
                .iter()
                .find(|e| e.id == id)
                .and_then(|e| e.response.as_ref()),
            None => tab.response.as_ref(),
        }
    }

    fn effective_sent_request(tab: &OpenTab) -> Option<&SentRequest> {
        match tab.viewing_example {
            Some(id) => tab
                .current_request
                .saved_examples
                .iter()
                .find(|e| e.id == id)
                .and_then(|e| e.sent_request.as_ref()),
            None => tab.sent_request.as_ref(),
        }
    }

    fn effective_error(tab: &OpenTab) -> Option<&String> {
        match tab.viewing_example {
            Some(id) => tab
                .current_request
                .saved_examples
                .iter()
                .find(|e| e.id == id)
                .and_then(|e| e.error.as_ref()),
            None => tab.response_error.as_ref(),
        }
    }

    fn response_viewer(&mut self, ui: &mut egui::Ui) {
        let tab = &self.tabs[self.active_tab];
        if Self::effective_response(tab).is_none()
            && Self::effective_error(tab).is_none()
            && Self::effective_sent_request(tab).is_none()
            && tab.script_error.is_none()
            && tab.script_log.is_empty()
            && tab.current_request.saved_examples.is_empty()
        {
            ui.weak("Send a request to see the response here.");
            return;
        }

        // Captured as an owned local (not read from `resp`/`tab` again below)
        // so the "Save Response" button, a few lines down, doesn't need
        // `tab`'s borrow of `self.tabs` still alive when it mutates
        // `self.import_message` on a write failure.
        let mut save_response_bytes: Option<Vec<u8>> = None;
        let mut has_response_without_bytes = false;
        if let Some(resp) = Self::effective_response(tab) {
            ui.horizontal(|ui| {
                let color = theme::status_color(resp.status);
                ui.colored_label(
                    color,
                    format!("Status: {} {}", resp.status, resp.status_text),
                );
                ui.label(format!("Time: {} ms", resp.duration_ms));
                ui.label(format!("Size: {} bytes", resp.size_bytes));
            });
            // `raw_bytes` is `#[serde(skip)]` (deliberately not persisted,
            // see `HttpResponse`'s doc comment) — a response reloaded from
            // disk (a saved example from a previous session) has an empty
            // `raw_bytes` even though `body` isn't, which would otherwise
            // silently "save" an empty file. Only offer the button when the
            // bytes are actually there (a genuinely empty response body,
            // e.g. `204 No Content`, has both empty — saving an empty file
            // for that case is correct, not a gap).
            if !resp.raw_bytes.is_empty() || resp.body.is_empty() {
                save_response_bytes = Some(resp.raw_bytes.clone());
            } else {
                has_response_without_bytes = true;
            }
        } else if let Some(err) = Self::effective_error(tab) {
            ui.colored_label(egui::Color32::RED, err);
        }
        if let Some(bytes) = save_response_bytes {
            if ui.button("Save Response").clicked()
                && let Some(path) = rfd::FileDialog::new().save_file()
            {
                if let Err(e) = std::fs::write(&path, &bytes) {
                    self.import_message = Some(format!("Could not save response: {e}"));
                } else {
                    self.import_message = Some(format!("Saved response to {}", path.display()));
                }
            }
        } else if has_response_without_bytes {
            ui.weak("Raw bytes for this saved example aren't available (not persisted across restarts) — save from a live response instead.");
        }
        if let Some(id) = tab.viewing_example {
            let name = tab
                .current_request
                .saved_examples
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.name.as_str())
                .unwrap_or("?");
            ui.colored_label(
                ui.visuals().weak_text_color(),
                format!("Viewing example: {name} (read-only) — click it again to go back to the live response"),
            );
        }
        ui.separator();

        self.saved_examples_strip(ui);

        let tab = &mut self.tabs[self.active_tab];
        ui.horizontal(|ui| {
            // Order matches Postman's own response-tab strip for the tabs
            // that have a real equivalent there (Body, Cookies, Headers,
            // Tests) — "Request" and "Diff" are this app's own additions
            // with no Postman counterpart, so they stay appended at the end.
            ui.selectable_value(&mut tab.response_tab, ResponseTab::Body, "Body");
            ui.selectable_value(&mut tab.response_tab, ResponseTab::Cookies, "Cookies");
            ui.selectable_value(&mut tab.response_tab, ResponseTab::Headers, "Headers");
            let passed = tab.test_results.iter().filter(|t| t.passed).count();
            let total = tab.test_results.len();
            let tests_label = if total > 0 {
                format!("Tests ({passed}/{total})")
            } else {
                "Tests".to_string()
            };
            ui.selectable_value(&mut tab.response_tab, ResponseTab::TestResults, tests_label);
            ui.selectable_value(&mut tab.response_tab, ResponseTab::Request, "Request");
            ui.selectable_value(&mut tab.response_tab, ResponseTab::Diff, "Diff");
            if tab.response_tab == ResponseTab::Body {
                ui.add_space(12.0);
                ui.checkbox(&mut self.auto_format_response, "Auto format")
                    .on_hover_text("Formats JSON/XML/HTML based on the response's Content-Type");
            }
        });
        ui.separator();

        // Computed once here (rather than via `Self::effective_response()`
        // inside the closure below) so the match arms below can keep
        // borrowing individual `self.tabs[self.active_tab]` fields (e.g.
        // `&mut self.tabs[self.active_tab].response_body_find`) without
        // fighting a whole-`self` borrow that a method call would require.
        let tab = &self.tabs[self.active_tab];
        let effective_response = Self::effective_response(tab).cloned();
        let effective_sent_request = Self::effective_sent_request(tab).cloned();
        let response_tab = tab.response_tab;

        egui::ScrollArea::vertical()
            .id_salt("response_scroll")
            .show(ui, |ui| match response_tab {
                ResponseTab::Body => {
                    let Some(resp) = &effective_response else {
                        ui.weak("No response body.");
                        return;
                    };
                    let text = if self.auto_format_response {
                        format_body(&resp.headers, &resp.body)
                    } else {
                        resp.body.clone()
                    };
                    let content_type = content_type_of(&resp.headers);
                    let language =
                        self.tabs[self.active_tab]
                            .response_body_language
                            .resolve(|| {
                                if content_type.is_empty() {
                                    syntax::sniff_language(&text)
                                } else {
                                    syntax::language_for_content_type(&content_type)
                                }
                            });

                    ui.horizontal(|ui| {
                        ui.label("Format:");
                        egui::ComboBox::from_id_salt("response_body_language")
                            .selected_text(
                                self.tabs[self.active_tab].response_body_language.label(),
                            )
                            .show_ui(ui, |ui| {
                                for opt in syntax::LanguageOverride::ALL {
                                    ui.selectable_value(
                                        &mut self.tabs[self.active_tab].response_body_language,
                                        opt,
                                        opt.label(),
                                    );
                                }
                            });
                    });

                    ui.horizontal(|ui| {
                        ui.label("Find:");
                        ui.add(
                            egui::TextEdit::singleline(
                                &mut self.tabs[self.active_tab].response_body_find,
                            )
                            .hint_text("search in body")
                            .desired_width(200.0),
                        );
                        if !self.tabs[self.active_tab].response_body_find.is_empty() {
                            let n = syntax::find_matches(
                                &text,
                                &self.tabs[self.active_tab].response_body_find,
                            )
                            .len();
                            ui.weak(format!("{n} match{}", if n == 1 { "" } else { "es" }));
                        }
                    });

                    let dark = ui.visuals().dark_mode;
                    let font_id = egui::TextStyle::Monospace.resolve(ui.style());
                    let search_query = self.tabs[self.active_tab].response_body_find.clone();
                    let freeze_wrap = self
                        .resize_settle_deadline
                        .is_some_and(|deadline| std::time::Instant::now() < deadline);
                    let mut layouter =
                        move |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
                            let wrap_width = effective_wrap_width(
                                ui.ctx(),
                                egui::Id::new("response_body_wrap"),
                                wrap_width,
                                freeze_wrap,
                            );
                            let mut job = syntax::highlight_cached(
                                ui.ctx(),
                                egui::Id::new("response_body_highlight"),
                                dark,
                                font_id.clone(),
                                buf.as_str(),
                                language,
                                &search_query,
                            );
                            job.wrap.max_width = wrap_width;
                            ui.fonts_mut(|f| f.layout_job(job))
                        };
                    let mut text_copy = text.clone();
                    show_code_editor_with_links(
                        ui,
                        egui::Id::new("response_body_viewer"),
                        &text,
                        &mut text_copy,
                        self.settings.vim_mode_enabled,
                        true,
                        |te| {
                            te.code_editor()
                                .desired_width(f32::INFINITY)
                                .layouter(&mut layouter)
                        },
                    );
                }
                ResponseTab::Headers => {
                    let Some(resp) = &effective_response else {
                        ui.weak("No response headers.");
                        return;
                    };
                    for (k, v) in &resp.headers {
                        ui.label(format!("{k}: {v}"));
                    }
                }
                ResponseTab::Request => {
                    let Some(req) = &effective_sent_request else {
                        ui.weak("No request has been sent yet.");
                        return;
                    };
                    ui.label(
                        egui::RichText::new(format!("{} {}", req.method, req.url))
                            .strong()
                            .monospace(),
                    );
                    ui.add_space(8.0);
                    ui.label("Headers actually sent:");
                    egui::Grid::new("sent_request_headers")
                        .num_columns(2)
                        .striped(true)
                        .show(ui, |ui| {
                            for (k, v) in &req.headers {
                                ui.monospace(k);
                                ui.monospace(v);
                                ui.end_row();
                            }
                        });
                    if let Some(body) = &req.body {
                        ui.add_space(8.0);
                        ui.label("Body actually sent:");
                        let sent_content_type = req
                            .headers
                            .iter()
                            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                            .map(|(_, v)| v.to_ascii_lowercase())
                            .unwrap_or_default();
                        let language = if sent_content_type.is_empty() {
                            syntax::sniff_language(body)
                        } else {
                            syntax::language_for_content_type(&sent_content_type)
                        };
                        let dark = ui.visuals().dark_mode;
                        let font_id = egui::TextStyle::Monospace.resolve(ui.style());
                        let freeze_wrap = self
                            .resize_settle_deadline
                            .is_some_and(|deadline| std::time::Instant::now() < deadline);
                        let mut layouter =
                            move |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
                                let wrap_width = effective_wrap_width(
                                    ui.ctx(),
                                    egui::Id::new("sent_request_body_wrap"),
                                    wrap_width,
                                    freeze_wrap,
                                );
                                let mut job = syntax::highlight_cached(
                                    ui.ctx(),
                                    egui::Id::new("sent_request_body_highlight"),
                                    dark,
                                    font_id.clone(),
                                    buf.as_str(),
                                    language,
                                    "",
                                );
                                job.wrap.max_width = wrap_width;
                                ui.fonts_mut(|f| f.layout_job(job))
                            };
                        let mut body_copy = body.clone();
                        // No Vim mode here on purpose — this is a read-only
                        // diagnostic replay of exactly what was sent, a
                        // lower-traffic view than the live response body,
                        // which already covers the read-only navigation
                        // case (see `Settings.vim_mode_enabled`'s own doc).
                        show_code_editor_with_links(
                            ui,
                            egui::Id::new("sent_request_body_viewer"),
                            body,
                            &mut body_copy,
                            false,
                            true,
                            |te| {
                                te.code_editor()
                                    .desired_width(f32::INFINITY)
                                    .layouter(&mut layouter)
                            },
                        );
                    }
                }
                ResponseTab::TestResults => {
                    let tab = &self.tabs[self.active_tab];
                    if let Some(err) = &tab.script_error {
                        ui.colored_label(egui::Color32::RED, format!("Script error: {err}"));
                        ui.add_space(8.0);
                    }
                    if tab.test_results.is_empty() {
                        ui.weak("No pm.test(...) assertions in the post-response script.");
                    } else {
                        for test in &tab.test_results {
                            ui.horizontal(|ui| {
                                if test.passed {
                                    ui.colored_label(egui::Color32::GREEN, "✔");
                                } else {
                                    ui.colored_label(egui::Color32::RED, "✘");
                                }
                                ui.label(&test.name);
                            });
                            if let Some(err) = &test.error {
                                ui.colored_label(egui::Color32::RED, format!("    {err}"));
                            }
                        }
                    }
                    if !tab.script_log.is_empty() {
                        ui.add_space(8.0);
                        ui.label("console.log output:");
                        for line in &tab.script_log {
                            ui.monospace(line);
                        }
                    }
                }
                ResponseTab::Cookies => {
                    let url = effective_sent_request
                        .as_ref()
                        .map(|r| r.url.as_str())
                        .filter(|u| !u.is_empty())
                        .or_else(|| {
                            let url = &self.tabs[self.active_tab].current_request.url;
                            if url.is_empty() {
                                None
                            } else {
                                Some(url.as_str())
                            }
                        })
                        .and_then(|u| reqwest::Url::parse(u).ok());
                    let Some(url) = url else {
                        ui.weak("Send a request first to see its cookies here.");
                        return;
                    };

                    let jar = self.cookie_jar.lock().unwrap();
                    let matching: Vec<_> = jar
                        .iter_unexpired()
                        .filter(|c| c.domain.matches(&url))
                        .collect();

                    if matching.is_empty() {
                        ui.weak(format!(
                            "No cookies for {}.",
                            url.host_str().unwrap_or(url.as_ref())
                        ));
                        return;
                    }

                    egui::Grid::new("response_cookies_grid")
                        .num_columns(5)
                        .striped(true)
                        .show(ui, |ui| {
                            ui.strong("Name");
                            ui.strong("Value");
                            ui.strong("Path");
                            ui.strong("Expires");
                            ui.strong("Flags");
                            ui.end_row();
                            for cookie in &matching {
                                ui.monospace(cookie.name());
                                ui.monospace(cookie.value());
                                let path: &str = cookie.path.as_ref();
                                ui.monospace(path);
                                let expires = match cookie.expires {
                                    cookie_store::CookieExpiration::SessionEnd => {
                                        "Session".to_string()
                                    }
                                    cookie_store::CookieExpiration::AtUtc(t) => t.to_string(),
                                };
                                ui.label(expires);
                                let mut flags = Vec::new();
                                if cookie.secure().unwrap_or(false) {
                                    flags.push("Secure");
                                }
                                if cookie.http_only().unwrap_or(false) {
                                    flags.push("HttpOnly");
                                }
                                ui.label(flags.join(", "));
                                ui.end_row();
                            }
                        });
                }
                ResponseTab::Diff => {
                    if self.tabs[self.active_tab]
                        .current_request
                        .saved_examples
                        .len()
                        < 2
                    {
                        ui.weak("Save at least 2 examples to compare them.");
                        return;
                    }

                    // Cloned (small, per-request N) rather than borrowed, so
                    // the later `self.tabs[self.active_tab].diff_examples = ...`
                    // write doesn't overlap with this read.
                    let examples = self.tabs[self.active_tab]
                        .current_request
                        .saved_examples
                        .clone();
                    let examples = &examples;
                    let mut left = self.tabs[self.active_tab]
                        .diff_examples
                        .map(|(l, _)| l)
                        .filter(|id| examples.iter().any(|e| e.id == *id))
                        .unwrap_or(examples[0].id);
                    let mut right = self.tabs[self.active_tab]
                        .diff_examples
                        .map(|(_, r)| r)
                        .filter(|id| examples.iter().any(|e| e.id == *id))
                        .unwrap_or(examples[examples.len().min(2) - 1].id);

                    ui.horizontal(|ui| {
                        ui.label("Left:");
                        egui::ComboBox::new("diff_left_combo", "")
                            .selected_text(
                                examples
                                    .iter()
                                    .find(|e| e.id == left)
                                    .map(|e| e.name.as_str())
                                    .unwrap_or("?"),
                            )
                            .show_ui(ui, |ui| {
                                for e in examples {
                                    ui.selectable_value(&mut left, e.id, &e.name);
                                }
                            });
                        ui.add_space(12.0);
                        ui.label("Right:");
                        egui::ComboBox::new("diff_right_combo", "")
                            .selected_text(
                                examples
                                    .iter()
                                    .find(|e| e.id == right)
                                    .map(|e| e.name.as_str())
                                    .unwrap_or("?"),
                            )
                            .show_ui(ui, |ui| {
                                for e in examples {
                                    ui.selectable_value(&mut right, e.id, &e.name);
                                }
                            });
                    });
                    self.tabs[self.active_tab].diff_examples = Some((left, right));
                    ui.add_space(8.0);

                    let left_example = examples.iter().find(|e| e.id == left);
                    let right_example = examples.iter().find(|e| e.id == right);
                    let (Some(l), Some(r)) = (left_example, right_example) else {
                        return;
                    };

                    let left_status = l.response.as_ref().map(|r| r.status);
                    let right_status = r.response.as_ref().map(|r| r.status);
                    let left_size = l.response.as_ref().map(|r| r.size_bytes);
                    let right_size = r.response.as_ref().map(|r| r.size_bytes);
                    if left_status != right_status || left_size != right_size {
                        // Plain ASCII "->" rather than "→": egui's bundled
                        // font doesn't cover U+2192, which rendered as an
                        // empty tofu box — same class of missing-glyph issue
                        // hit (and fixed the same way) in Phases 2 and 3.
                        ui.horizontal(|ui| {
                            ui.label(format!(
                                "Status: {} -> {}",
                                status_or_dash(left_status),
                                status_or_dash(right_status)
                            ));
                            ui.add_space(12.0);
                            ui.label(format!(
                                "Size: {} -> {} bytes",
                                left_size.map(|s| s.to_string()).unwrap_or("-".into()),
                                right_size.map(|s| s.to_string()).unwrap_or("-".into()),
                            ));
                        });
                        ui.add_space(8.0);
                    }

                    let left_body = l.response.as_ref().map(|r| r.body.as_str()).unwrap_or("");
                    let right_body = r.response.as_ref().map(|r| r.body.as_str()).unwrap_or("");
                    let diff = similar::TextDiff::from_lines(left_body, right_body);
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        for change in diff.iter_all_changes() {
                            let (color, prefix) = match change.tag() {
                                similar::ChangeTag::Delete => (egui::Color32::RED, "- "),
                                similar::ChangeTag::Insert => (egui::Color32::GREEN, "+ "),
                                similar::ChangeTag::Equal => (ui.visuals().text_color(), "  "),
                            };
                            ui.colored_label(
                                color,
                                egui::RichText::new(format!(
                                    "{prefix}{}",
                                    change.as_str().unwrap_or("").trim_end_matches('\n')
                                ))
                                .monospace(),
                            );
                        }
                    });
                }
            });
    }

    fn environment_editor(&mut self, ui: &mut egui::Ui, id: Uuid) {
        let Some(env) = self.data.environments.iter_mut().find(|e| e.id == id) else {
            ui.label("Environment not found.");
            return;
        };
        ui.horizontal(|ui| {
            ui.label("Name:");
            ui.text_edit_singleline(&mut env.name);
        });
        ui.separator();
        ui.label("Variables (use {{key}} in requests):");
        key_value_table(ui, "env_vars_table", &mut env.variables);
        if ui.button("Done").clicked() {
            self.central_view = CentralView::Request;
        }
        self.save();
    }

    /// Collection-level description + pre-request/test scripts — reuses the
    /// exact `script_editor`/`snippet_buttons` widgets the request editor's
    /// `PreRequestScript`/`TestsScript` tabs already use, just aimed at
    /// `Collection` fields instead of `RequestItem` ones. Mirrors
    /// `environment_editor`'s shape (find-by-id, fields, a "Done" button).
    fn collection_editor(&mut self, ui: &mut egui::Ui, id: Uuid) {
        let Some(collection) = self.data.collections.iter_mut().find(|c| c.id == id) else {
            ui.label("Collection not found.");
            return;
        };
        ui.heading(format!("Edit collection: {}", collection.name));
        ui.separator();
        ui.label("Description:");
        ui.add(
            egui::TextEdit::multiline(&mut collection.description)
                .hint_text("What is this collection for?")
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        ui.add_space(8.0);
        ui.label("Pre-request Script (runs before every request in this collection):");
        snippet_buttons(ui, PRE_REQUEST_SNIPPETS, &mut collection.pre_request_script);
        script_editor(
            ui,
            "collection_pre_request_script",
            &mut collection.pre_request_script,
            self.settings.vim_mode_enabled,
        );
        ui.add_space(8.0);
        ui.label("Tests Script (runs after every response in this collection):");
        snippet_buttons(ui, TEST_SNIPPETS, &mut collection.post_response_script);
        script_editor(
            ui,
            "collection_post_response_script",
            &mut collection.post_response_script,
            self.settings.vim_mode_enabled,
        );
        ui.add_space(8.0);
        if ui.button("Done").clicked() {
            self.central_view = CentralView::Request;
        }
        self.save();
    }

    /// Same idea as `collection_editor`, one level down — a folder's own
    /// description + pre-request/test scripts, which run for every request
    /// in that folder's subtree (see `container_scripts`/
    /// `model::container_scripts_for`).
    fn folder_editor(&mut self, ui: &mut egui::Ui, collection: Uuid, folder_path: &[Uuid]) {
        let Some(c) = self
            .data
            .collections
            .iter_mut()
            .find(|c| c.id == collection)
        else {
            ui.label("Collection not found.");
            return;
        };
        let Some(folder) = c.find_folder_mut(folder_path) else {
            ui.label("Folder not found.");
            return;
        };
        ui.heading(format!("Edit folder: {}", folder.name));
        ui.separator();
        ui.label("Description:");
        ui.add(
            egui::TextEdit::multiline(&mut folder.description)
                .hint_text("What is this folder for?")
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        ui.add_space(8.0);
        ui.label("Pre-request Script (runs before every request in this folder):");
        snippet_buttons(ui, PRE_REQUEST_SNIPPETS, &mut folder.pre_request_script);
        script_editor(
            ui,
            "folder_pre_request_script",
            &mut folder.pre_request_script,
            self.settings.vim_mode_enabled,
        );
        ui.add_space(8.0);
        ui.label("Tests Script (runs after every response in this folder):");
        snippet_buttons(ui, TEST_SNIPPETS, &mut folder.post_response_script);
        script_editor(
            ui,
            "folder_post_response_script",
            &mut folder.post_response_script,
            self.settings.vim_mode_enabled,
        );
        ui.add_space(8.0);
        if ui.button("Done").clicked() {
            self.central_view = CentralView::Request;
        }
        self.save();
    }

    /// Postman-style Console — a running, in-app log of every send (across
    /// every tab), including script `console.log` output and test results.
    /// Session-only (cleared here doesn't touch the durable `console.log`
    /// text file `push_console_entry` also writes to on every send).
    fn console_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Console");
            if ui.small_button("\u{d7}").on_hover_text("Close").clicked() {
                self.console_open = false;
            }
        });

        if self.console.is_empty() {
            ui.weak("Nothing sent yet this session.");
            return;
        }

        ui.horizontal(|ui| {
            if ui.button("Clear console").clicked() {
                self.console.clear();
            }
            ui.weak(format!(
                "Also logged to: {}",
                storage::console_log_path().display()
            ));
        });
        ui.separator();

        // Only jump the scroll position on the frame a new entry actually
        // arrives (not every frame) — otherwise this would fight a manual
        // scroll through older entries.
        let should_scroll_to_newest = self.console.len() != self.console_scrolled_len;
        self.console_scrolled_len = self.console.len();

        egui::ScrollArea::vertical().show(ui, |ui| {
            // Newest first, matching `self.console`'s own storage order (and
            // `data.history`'s "most recent at the top" convention) — a new
            // send should appear right at the top, not scrolled past.
            for (idx, entry) in self.console.iter().enumerate() {
                let row = console_entry_row(ui, entry);
                if idx == 0 && should_scroll_to_newest {
                    row.scroll_to_me(Some(egui::Align::TOP));
                }
                for line in &entry.script_log {
                    ui.monospace(format!("  console.log: {line}"));
                }
                if !entry.test_results.is_empty() {
                    let passed = entry.test_results.iter().filter(|t| t.passed).count();
                    let color = if passed == entry.test_results.len() {
                        egui::Color32::GREEN
                    } else {
                        egui::Color32::RED
                    };
                    ui.colored_label(
                        color,
                        format!("  tests: {passed}/{}", entry.test_results.len()),
                    );
                }
                ui.separator();
            }
        });
    }

    fn runner_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Collection Runner");
        ui.separator();

        let Some(_collection_id) = self.runner.collection else {
            ui.weak("Right-click a collection or folder in the sidebar and choose \"Run\"/\"Run folder\" to get started.");
            return;
        };
        ui.label(format!("Target: {}", self.runner.target_label));

        let running = self.runner.active_run_id.is_some();
        ui.add_enabled_ui(!running, |ui| {
            ui.horizontal(|ui| {
                ui.label("Iterations:");
                ui.add_enabled(
                    self.runner.data_rows.is_empty(),
                    egui::DragValue::new(&mut self.runner.iterations).range(1..=1000),
                );
                if !self.runner.data_rows.is_empty() {
                    ui.weak(format!(
                        "(overridden by {} data rows)",
                        self.runner.data_rows.len()
                    ));
                }
            });
            ui.horizontal(|ui| {
                ui.label("Delay between requests (ms):");
                ui.add(egui::DragValue::new(&mut self.runner.delay_ms).range(0..=60_000));
            });
            ui.horizontal(|ui| {
                ui.label("Data file (CSV/JSON):");
                ui.weak(self.runner.data_file_path.as_deref().unwrap_or("(none)"));
                if ui.small_button("Load…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("CSV/JSON", &["csv", "json"])
                        .pick_file()
                {
                    match Self::parse_runner_data_file(&path) {
                        Ok(rows) => {
                            self.runner.data_file_path = Some(path.display().to_string());
                            self.runner.data_rows = rows;
                        }
                        Err(e) => {
                            self.import_message = Some(format!("Could not load data file: {e}"))
                        }
                    }
                }
                if self.runner.data_file_path.is_some() && ui.small_button("Clear").clicked() {
                    self.runner.data_file_path = None;
                    self.runner.data_rows.clear();
                }
            });
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(!running, egui::Button::new("Run")).clicked() {
                self.start_run();
            }
            if running && ui.button("Stop").clicked() {
                self.cancel_run();
            }
            if ui.button("Done").clicked() {
                self.central_view = CentralView::Request;
            }
        });

        if running {
            let total = self.runner.total_requests.max(1);
            let fraction = self.runner.completed as f32 / total as f32;
            ui.add(egui::ProgressBar::new(fraction).show_percentage());
            ui.label(format!(
                "{}/{} requests completed",
                self.runner.completed, self.runner.total_requests
            ));
        }

        ui.add_space(8.0);
        ui.separator();

        if !self.runner.results.is_empty() {
            egui::ScrollArea::vertical()
                .id_salt("runner_results_scroll")
                .max_height(320.0)
                .show(ui, |ui| {
                    egui::Grid::new("runner_results_grid")
                        .num_columns(6)
                        .striped(true)
                        .show(ui, |ui| {
                            ui.strong("Iter");
                            ui.strong("Method");
                            ui.strong("Name");
                            ui.strong("Status");
                            ui.strong("Time");
                            ui.strong("Tests");
                            ui.end_row();
                            for result in &self.runner.results {
                                ui.label(format!("{}", result.iteration + 1));
                                ui.colored_label(
                                    theme::method_color(result.method),
                                    result.method.as_str(),
                                );
                                ui.label(&result.name);
                                if let Some(status) = result.status {
                                    ui.colored_label(
                                        theme::status_color(status),
                                        status.to_string(),
                                    );
                                } else {
                                    ui.colored_label(
                                        egui::Color32::RED,
                                        result.error.as_deref().unwrap_or("ERROR"),
                                    );
                                }
                                ui.label(
                                    result
                                        .duration_ms
                                        .map(|d| format!("{d} ms"))
                                        .unwrap_or_default(),
                                );
                                let passed =
                                    result.test_results.iter().filter(|t| t.passed).count();
                                let total = result.test_results.len();
                                if total > 0 {
                                    let color = if passed == total {
                                        egui::Color32::GREEN
                                    } else {
                                        egui::Color32::RED
                                    };
                                    ui.colored_label(color, format!("{passed}/{total}"));
                                } else {
                                    ui.weak("-");
                                }
                                ui.end_row();
                            }
                        });
                });
        }

        if !running && !self.runner.results.is_empty() {
            ui.add_space(8.0);
            ui.separator();
            let total_tests: usize = self
                .runner
                .results
                .iter()
                .map(|r| r.test_results.len())
                .sum();
            let passed_tests: usize = self
                .runner
                .results
                .iter()
                .map(|r| r.test_results.iter().filter(|t| t.passed).count())
                .sum();
            let total_duration: u128 = self
                .runner
                .results
                .iter()
                .filter_map(|r| r.duration_ms)
                .sum();
            let failed_requests = self
                .runner
                .results
                .iter()
                .filter(|r| r.status.is_none())
                .count();
            if self.runner.last_run_cancelled {
                ui.colored_label(egui::Color32::YELLOW, "Run stopped early.");
            }
            ui.label(format!(
                "{} requests run, {} failed to complete, {}/{} tests passed, {} ms total",
                self.runner.results.len(),
                failed_requests,
                passed_tests,
                total_tests,
                total_duration
            ));
            if ui.button("Copy results as JSON").clicked() {
                // The "exportable report" — kept as structured JSON rather
                // than a styled HTML report, matching the roadmap's own
                // note about not over-building any one phase.
                #[derive(serde::Serialize)]
                struct ExportRow<'a> {
                    iteration: usize,
                    method: &'a str,
                    name: &'a str,
                    status: Option<u16>,
                    duration_ms: Option<u128>,
                    tests_passed: usize,
                    tests_total: usize,
                    error: Option<&'a str>,
                }
                let rows: Vec<ExportRow> = self
                    .runner
                    .results
                    .iter()
                    .map(|r| ExportRow {
                        iteration: r.iteration,
                        method: r.method.as_str(),
                        name: &r.name,
                        status: r.status,
                        duration_ms: r.duration_ms,
                        tests_passed: r.test_results.iter().filter(|t| t.passed).count(),
                        tests_total: r.test_results.len(),
                        error: r.error.as_deref(),
                    })
                    .collect();
                if let Ok(json) = serde_json::to_string_pretty(&rows) {
                    ui.ctx().copy_text(json);
                }
            }
        }
    }

    fn settings_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.separator();

        ui.label("Theme");
        ui.horizontal(|ui| {
            for mode in ThemeMode::ALL {
                ui.selectable_value(&mut self.settings.theme, mode, mode.label());
            }
        });

        ui.add_space(12.0);
        ui.separator();
        ui.label("Editing");
        ui.checkbox(
            &mut self.settings.vim_mode_enabled,
            "Vim mode for text editors",
        );
        ui.weak(
            "Normal/Insert/Visual — h/j/k/l, w/b/e, 0/$, gg/G, i/a/o, d/y/p, v — \
             for the request body/script editors. Response body is \
             navigation + yank-to-copy only (it's read-only).",
        );

        ui.add_space(12.0);
        ui.separator();
        ui.label("Proxy");
        ui.checkbox(&mut self.settings.proxy.enabled, "Use custom proxy");
        if self.settings.proxy.enabled {
            ui.horizontal(|ui| {
                ui.label("HTTP proxy URL");
                ui.text_edit_singleline(&mut self.settings.proxy.http_proxy);
            });
            ui.horizontal(|ui| {
                ui.label("HTTPS proxy URL");
                ui.text_edit_singleline(&mut self.settings.proxy.https_proxy);
            });
            ui.horizontal(|ui| {
                ui.label("No-proxy hosts (comma-separated)");
                ui.text_edit_singleline(&mut self.settings.proxy.no_proxy);
            });
            ui.weak("Accepts http://, https://, or socks5:// URLs.");
        }

        ui.add_space(12.0);
        ui.separator();
        ui.label("TLS");
        ui.checkbox(
            &mut self.settings.tls.accept_invalid_certs,
            "Disable certificate verification",
        );
        if self.settings.tls.accept_invalid_certs {
            ui.colored_label(
                egui::Color32::RED,
                "Insecure: accepts any TLS certificate, including invalid/expired/self-signed ones from any host.",
            );
        }
        ui.horizontal(|ui| {
            ui.label("Custom CA certificate:");
            ui.weak(
                self.settings
                    .tls
                    .custom_ca_cert_path
                    .as_deref()
                    .unwrap_or("(none)"),
            );
            if ui.small_button("Choose file…").clicked()
                && let Some(path) = rfd::FileDialog::new().pick_file()
            {
                self.settings.tls.custom_ca_cert_path = Some(path.display().to_string());
            }
            if self.settings.tls.custom_ca_cert_path.is_some() && ui.small_button("Clear").clicked()
            {
                self.settings.tls.custom_ca_cert_path = None;
            }
        });
        ui.horizontal(|ui| {
            ui.label("Client certificate (mTLS):");
            ui.weak(
                self.settings
                    .tls
                    .client_cert_path
                    .as_deref()
                    .unwrap_or("(none)"),
            );
            if ui.small_button("Choose file…").clicked()
                && let Some(path) = rfd::FileDialog::new().pick_file()
            {
                self.settings.tls.client_cert_path = Some(path.display().to_string());
            }
            if self.settings.tls.client_cert_path.is_some() && ui.small_button("Clear").clicked() {
                self.settings.tls.client_cert_path = None;
            }
        });

        ui.add_space(12.0);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Save & Apply").clicked() {
                storage::save_settings(&self.settings);
                self.client = http_client::build_client(&self.settings, self.cookie_jar.clone());
                self.import_message = Some("Settings saved and applied.".to_string());
            }
            if ui.button("Done").clicked() {
                self.central_view = CentralView::Request;
            }
        });
        import_message_banner(ui, &mut self.import_message);
    }
}

fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("explorer").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();

    if let Err(e) = result {
        eprintln!("Failed to open link {url}: {e}");
    }
}

/// Shows a `TextEdit` (already configured by the caller) and, while Option/Alt
/// is held, turns any detected URL in `content` (a snapshot of the same text
/// the `TextEdit` is displaying) into a click-to-open link: pointer cursor on
/// hover, opens in the system browser on click.
/// `text`/`configure` build the actual `TextEdit` through
/// `vim_editor::vim_multiline_edit` (so this editor gets Vim mode when
/// `vim_enabled` — see `Settings.vim_mode_enabled`) rather than taking a
/// pre-built `TextEdit`, since the Vim layer needs to own the `.show()`
/// call itself (to intercept keys first, then read back the resulting
/// galley for this function's own Option/Alt+click-to-open-link handling).
#[allow(clippy::too_many_arguments)]
fn show_code_editor_with_links<'a>(
    ui: &mut egui::Ui,
    id: egui::Id,
    content: &str,
    text: &'a mut String,
    vim_enabled: bool,
    vim_readonly: bool,
    configure: impl FnOnce(egui::TextEdit<'a>) -> egui::TextEdit<'a>,
) {
    let links = syntax::detect_links(content);
    let (response, galley, galley_pos) =
        vim_editor::vim_multiline_edit(ui, id, text, vim_enabled, vim_readonly, configure);
    if links.is_empty() {
        return;
    }
    if !ui.input(|i| i.modifiers.alt) {
        return;
    }
    let Some(hover_pos) = response.hover_pos() else {
        return;
    };
    let rel_pos = hover_pos - galley_pos;
    let char_idx = galley.cursor_from_pos(rel_pos).index.0;
    let byte_idx = content
        .char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(content.len());
    if let Some(range) = links.iter().find(|r| r.contains(&byte_idx)) {
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
        if response.clicked() {
            open_in_browser(&content[range.clone()]);
        }
    }
}

/// Pure so it's directly unit-testable, per this codebase's preference for
/// small testable free functions over UI-code-embedded formatting logic.
fn status_or_dash(status: Option<u16>) -> String {
    status
        .map(|s| s.to_string())
        .unwrap_or_else(|| "-".to_string())
}

/// Maps this app's own persistable `ThemeMode` to the real
/// `egui::ThemePreference` — kept as a separate local type since
/// `ThemePreference` itself doesn't derive `Serialize`/`Deserialize` in
/// this build (see `ThemeMode`'s own doc comment in `model.rs`).
fn egui_theme_preference(mode: ThemeMode) -> egui::ThemePreference {
    match mode {
        ThemeMode::Light => egui::ThemePreference::Light,
        ThemeMode::Dark => egui::ThemePreference::Dark,
        ThemeMode::System => egui::ThemePreference::System,
    }
}

/// Postman-inspired visual identity: a shared brand accent color, shared
/// method/status color helpers (previously ad-hoc — a colored method badge
/// only existed in the tab bar, and status coloring was a bare
/// `Color32::GREEN/YELLOW/RED` literal repeated at 8+ separate call sites),
/// and the one place that nudges egui's global `Style` itself toward
/// Postman's own look.
mod theme {
    use super::Method;
    use egui::Color32;

    /// Postman's own real, public brand color.
    pub const ACCENT: Color32 = Color32::from_rgb(0xFF, 0x6C, 0x37);

    /// Per-method color, matching Postman's own per-method palette (green
    /// GET, orange/amber POST, blue PUT, purple PATCH, red DELETE) — used
    /// everywhere a method is displayed (tab bar, sidebar tree, history,
    /// command palette, collection runner), not just the tab bar as before
    /// this phase.
    pub fn method_color(method: Method) -> Color32 {
        match method {
            Method::Get => Color32::from_rgb(0x6b, 0xcb, 0x5f),
            Method::Post => Color32::from_rgb(0xf0, 0xad, 0x4e),
            Method::Put => Color32::from_rgb(0x5b, 0x9b, 0xd5),
            Method::Patch => Color32::from_rgb(0x9b, 0x59, 0xb6),
            Method::Delete => Color32::from_rgb(0xe0, 0x5d, 0x5d),
            Method::Head | Method::Options => Color32::from_rgb(0x95, 0xa5, 0xa6),
        }
    }

    /// A 4-way refinement of the old `< 300`/`< 400`/else 3-way bucket that
    /// used to be repeated as a bare `Color32::GREEN/YELLOW/RED` literal at
    /// every status-display call site — this distinguishes a 4xx client
    /// error from a 5xx server error (a common REST-client convention, not
    /// a claim of pixel-exact Postman hex values, which a static code/crate
    /// audit has no way to verify).
    pub fn status_color(status: u16) -> Color32 {
        match status {
            0..=299 => Color32::from_rgb(0x6b, 0xcb, 0x5f), // 2xx: green
            300..=399 => Color32::from_rgb(0x5b, 0x9b, 0xd5), // 3xx: blue
            400..=499 => Color32::from_rgb(0xf0, 0xad, 0x4e), // 4xx: orange
            _ => Color32::from_rgb(0xe0, 0x5d, 0x5d),       // 5xx+: red
        }
    }

    /// A background fill for a panel meant to read as a visually distinct
    /// "chrome" surface next to the main content area — Postman's own
    /// sidebar reads this way. Nudges the theme's own panel color a few
    /// shades darker via plain integer subtraction (`saturating_sub` clamps
    /// at black), which reads as a subtle recess in both dark and light
    /// mode without needing a separate hardcoded color per theme.
    pub fn recessed_panel_fill(base: Color32) -> Color32 {
        let shade = |c: u8| c.saturating_sub(10);
        Color32::from_rgb(shade(base.r()), shade(base.g()), shade(base.b()))
    }

    /// Nudges egui's global `Style` — both the dark and light variant at
    /// once, via `all_styles_mut` (confirmed present in the cached
    /// egui-0.35.0 source: it mutates `dark_style`/`light_style` in one
    /// call) — toward Postman's own look: the brand accent color on
    /// selection/hyperlinks/focus-and-hover outlines, and slightly rounder
    /// corners than egui's square-ish stock default. Called once per frame,
    /// right alongside the existing `ctx.set_theme(...)` call — cheap (no
    /// allocation beyond what `set_theme` itself already does every frame,
    /// since this runs before any widget in the current frame could be
    /// holding a live `Style` `Arc` clone) and idempotent, so no new
    /// one-shot "already styled" field is needed.
    pub fn apply_postman_visuals(ctx: &egui::Context) {
        ctx.all_styles_mut(|style| {
            style.visuals.selection.bg_fill = ACCENT;
            style.visuals.hyperlink_color = ACCENT;
            style.visuals.widgets.hovered.bg_stroke.color = ACCENT;
            style.visuals.widgets.active.bg_stroke.color = ACCENT;
            let radius = egui::CornerRadius::same(5);
            style.visuals.widgets.inactive.corner_radius = radius;
            style.visuals.widgets.hovered.corner_radius = radius;
            style.visuals.widgets.active.corner_radius = radius;
            style.visuals.widgets.open.corner_radius = radius;
            style.visuals.window_corner_radius = radius;
        });
    }
}

fn content_type_of(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Formats the response body according to its `Content-Type`: pretty-printed
/// JSON, indented XML/HTML, or the raw body for anything else (plain text,
/// CSS, images-as-text, etc. don't benefit from reformatting).
fn format_body(headers: &[(String, String)], body: &str) -> String {
    let ct = content_type_of(headers);
    if ct.contains("json") {
        return serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or_else(|| body.to_string());
    }
    if ct.contains("xml") || ct.contains("html") {
        return pretty_print_markup(body);
    }
    if ct.is_empty() {
        // No Content-Type header: fall back to sniffing the body itself.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
            return serde_json::to_string_pretty(&v).unwrap_or_else(|_| body.to_string());
        }
        let trimmed = body.trim_start();
        if trimmed.starts_with('<') {
            return pretty_print_markup(body);
        }
    }
    body.to_string()
}

/// Naive tag-based indenter for XML/HTML. Not a real parser: it just breaks
/// `><` boundaries onto new lines and indents by nesting depth, which is
/// enough to make a minified response readable in the viewer.
fn pretty_print_markup(input: &str) -> String {
    let flat: String = input.split_whitespace().collect::<Vec<_>>().join(" ");
    let with_breaks = flat.replace("><", ">\n<");

    let mut indent: usize = 0;
    let mut out = String::new();
    for line in with_breaks.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let is_closing = line.starts_with("</");
        let is_special = line.starts_with("<!") || line.starts_with("<?");
        let is_self_closing = line.ends_with("/>");
        let opens_and_closes_on_same_line = line.starts_with('<')
            && !is_closing
            && !is_special
            && !is_self_closing
            && line.matches('<').count() >= 2
            && line.contains("</");

        if is_closing && indent > 0 {
            indent -= 1;
        }
        out.push_str(&"  ".repeat(indent));
        out.push_str(line);
        out.push('\n');
        if line.starts_with('<')
            && !is_closing
            && !is_special
            && !is_self_closing
            && !opens_and_closes_on_same_line
        {
            indent += 1;
        }
    }
    out.trim_end().to_string()
}

const ALL_AUTH_KINDS: [AuthKind; 9] = [
    AuthKind::Inherit,
    AuthKind::None,
    AuthKind::Basic,
    AuthKind::Bearer,
    AuthKind::ApiKey,
    AuthKind::Digest,
    AuthKind::OAuth1,
    AuthKind::OAuth2,
    AuthKind::AwsSigV4,
];

fn auth_kind_label(kind: AuthKind) -> &'static str {
    match kind {
        AuthKind::Inherit => "Inherit Auth from Parent",
        AuthKind::None => "No Auth",
        AuthKind::Basic => "Basic Auth",
        AuthKind::Bearer => "Bearer Token",
        AuthKind::ApiKey => "API Key",
        AuthKind::Digest => "Digest Auth",
        AuthKind::OAuth1 => "OAuth 1.0",
        AuthKind::OAuth2 => "OAuth 2.0",
        AuthKind::AwsSigV4 => "AWS Signature",
    }
}

fn auth_param(auth: &AuthConfig, key: &str) -> String {
    auth.params
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| kv.value.clone())
        .unwrap_or_default()
}

/// Finds (or creates, defaulting enabled) the `KeyValue` for `key` in
/// `auth.params` and returns its value for in-place editing — the same
/// find-or-push-by-key shape `AuthConfig.params` is designed around (see
/// its doc comment in `model.rs`), just not exposed as a `Vec<KeyValue>`
/// table widget like headers/params, since each `AuthKind` has a fixed,
/// known set of fields rather than an open-ended list.
fn auth_param_mut<'a>(auth: &'a mut AuthConfig, key: &str) -> &'a mut String {
    if let Some(idx) = auth.params.iter().position(|kv| kv.key == key) {
        &mut auth.params[idx].value
    } else {
        auth.params.push(KeyValue {
            key: key.to_string(),
            value: String::new(),
            enabled: true,
        });
        &mut auth.params.last_mut().unwrap().value
    }
}

fn auth_field(ui: &mut egui::Ui, auth: &mut AuthConfig, key: &str, label: &str) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [140.0, ui.spacing().interact_size.y],
            egui::Label::new(label),
        );
        ui.text_edit_singleline(auth_param_mut(auth, key));
    });
}

/// The request editor's "Auth" tab. `effective` is the already-resolved
/// config (see `App::effective_auth`) — only used for the read-only
/// "resolves to" label when `auth.kind` is `Inherit`; every other kind edits
/// `auth` directly.
fn auth_editor(ui: &mut egui::Ui, auth: &mut AuthConfig, effective: &AuthConfig) {
    egui::ComboBox::from_id_salt("auth_kind_combo")
        .selected_text(auth_kind_label(auth.kind))
        .show_ui(ui, |ui| {
            for kind in ALL_AUTH_KINDS {
                ui.selectable_value(&mut auth.kind, kind, auth_kind_label(kind));
            }
        });
    ui.add_space(8.0);

    match auth.kind {
        AuthKind::Inherit => {
            let resolved = if effective.kind == AuthKind::Inherit {
                "No Auth (nothing set on any parent)".to_string()
            } else {
                auth_kind_label(effective.kind).to_string()
            };
            ui.weak(format!("Resolves to: {resolved}"));
        }
        AuthKind::None => {
            ui.weak("This request will send no authorization header.");
        }
        AuthKind::Basic => {
            auth_field(ui, auth, "username", "Username");
            auth_field(ui, auth, "password", "Password");
        }
        AuthKind::Bearer => {
            auth_field(ui, auth, "token", "Token");
        }
        AuthKind::ApiKey => {
            auth_field(ui, auth, "key", "Key");
            auth_field(ui, auth, "value", "Value");
            ui.horizontal(|ui| {
                ui.add_sized(
                    [140.0, ui.spacing().interact_size.y],
                    egui::Label::new("Add to"),
                );
                let in_query = auth_param(auth, "in") == "query";
                if ui.selectable_label(!in_query, "Header").clicked() {
                    *auth_param_mut(auth, "in") = "header".to_string();
                }
                if ui.selectable_label(in_query, "Query Params").clicked() {
                    *auth_param_mut(auth, "in") = "query".to_string();
                }
            });
        }
        AuthKind::Digest => {
            auth_field(ui, auth, "username", "Username");
            auth_field(ui, auth, "password", "Password");
            ui.weak(
                "Sent only if the server challenges with a 401 WWW-Authenticate: Digest response.",
            );
        }
        AuthKind::OAuth1 => {
            auth_field(ui, auth, "consumerKey", "Consumer Key");
            auth_field(ui, auth, "consumerSecret", "Consumer Secret");
            auth_field(ui, auth, "token", "Token");
            auth_field(ui, auth, "tokenSecret", "Token Secret");
            ui.weak("HMAC-SHA1 only.");
        }
        AuthKind::OAuth2 => {
            auth_field(ui, auth, "accessToken", "Access Token");
            ui.weak("Or leave blank and fetch one via Client Credentials:");
            auth_field(ui, auth, "tokenUrl", "Access Token URL");
            auth_field(ui, auth, "clientId", "Client ID");
            auth_field(ui, auth, "clientSecret", "Client Secret");
            auth_field(ui, auth, "scope", "Scope");
        }
        AuthKind::AwsSigV4 => {
            auth_field(ui, auth, "accessKey", "Access Key");
            auth_field(ui, auth, "secretKey", "Secret Key");
            auth_field(ui, auth, "region", "Region (default us-east-1)");
            auth_field(ui, auth, "service", "Service (default execute-api)");
            auth_field(ui, auth, "sessionToken", "Session Token");
        }
    }
}

/// Renders the auto-detected `:name` path variables for `url` (e.g. `:tenantId`
/// in `{{baseUrl}}/:tenantId/document-baskets`). Unlike `key_value_table`, the
/// key column isn't editable and the list itself is fully derived from the
/// URL text each frame — add/remove a `:segment` in the URL and this list
/// follows automatically, matching Postman's path-variable behavior.
fn path_params_editor(ui: &mut egui::Ui, url: &str, path_params: &mut Vec<KeyValue>) {
    let detected = model::extract_path_param_names(url);
    path_params.retain(|p| detected.contains(&p.key));
    for name in &detected {
        if !path_params.iter().any(|p| &p.key == name) {
            path_params.push(KeyValue {
                key: name.clone(),
                value: String::new(),
                enabled: true,
            });
        }
    }
    if detected.is_empty() {
        return;
    }

    ui.add_space(12.0);
    ui.label("Path Variables (from :name segments in the URL):");
    egui::Grid::new("path_params_table")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            for name in &detected {
                if let Some(p) = path_params.iter_mut().find(|p| &p.key == name) {
                    ui.monospace(format!(":{}", p.key));
                    ui.text_edit_singleline(&mut p.value);
                    ui.end_row();
                }
            }
        });
}

/// `query` must already be lowercased; empty matches everything.
/// While `freeze` is true, ignores `wrap_width` and keeps returning whatever
/// width was last recorded under `id` instead — so a body's syntax-highlight
/// layout job stays byte-for-byte identical frame to frame during an active
/// window resize (same text, same wrap width) and egui's own galley cache
/// can just reuse the previous result instead of re-shaping the whole body
/// on every single intermediate frame of the drag. Once `freeze` goes back
/// to false, the real (now-settled) width is adopted again.
fn effective_wrap_width(ctx: &egui::Context, id: egui::Id, wrap_width: f32, freeze: bool) -> f32 {
    if freeze && let Some(frozen) = ctx.data_mut(|d| d.get_temp::<f32>(id)) {
        return frozen;
    }
    ctx.data_mut(|d| d.insert_temp(id, wrap_width));
    wrap_width
}

/// Scores how well `query` fuzzy-matches `haystack` via `nucleo_matcher`'s
/// fzf-quality algorithm — non-contiguous subsequence matches (e.g. `"crq"`
/// matches `"Create Request"`), ranked by word-boundary/consecutive-
/// character/proximity bonuses, `Some(score)` (higher is better) or `None`
/// if it doesn't match at all. Replaces the old plain
/// `.to_lowercase().contains(query)` substring check this was built to fix
/// ("fuzzy chưa ngon như fzf"). An empty query matches everything with a
/// score of 0 — ties are broken by the caller's *stable* sort preserving
/// insertion order, so "show everything, unranked" when nothing's typed
/// yet still looks the same as before.
fn fuzzy_score(matcher: &mut nucleo_matcher::Matcher, haystack: &str, query: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(0);
    }
    let mut haystack_buf = Vec::new();
    let mut query_buf = Vec::new();
    let haystack = nucleo_matcher::Utf32Str::new(haystack, &mut haystack_buf);
    let query = nucleo_matcher::Utf32Str::new(query, &mut query_buf);
    matcher.fuzzy_match(haystack, query).map(u32::from)
}

/// Fuzzy-scores a request against `query` across name/URL/method, keeping
/// whichever field scores highest — so a method match like `"post"` isn't
/// buried under an unrelated name that scores lower (or doesn't match at
/// all).
fn fuzzy_score_request(
    matcher: &mut nucleo_matcher::Matcher,
    item: &RequestItem,
    query: &str,
) -> Option<u32> {
    [
        fuzzy_score(matcher, &item.name, query),
        fuzzy_score(matcher, &item.url, query),
        fuzzy_score(matcher, item.method.as_str(), query),
    ]
    .into_iter()
    .flatten()
    .max()
}

/// Recursively collects every request matching `query` from `folders` (and
/// their subfolders, arbitrarily deep), tagging each with the full folder
/// path it was found at and its fuzzy score — used by the command
/// palette's request search.
fn collect_matching_requests<'a>(
    folders: &'a [Folder],
    path: &mut Vec<Uuid>,
    query: &str,
    collection_id: Uuid,
    matcher: &mut nucleo_matcher::Matcher,
    out: &mut Vec<(u32, Uuid, Vec<Uuid>, &'a RequestItem)>,
) {
    for folder in folders {
        path.push(folder.id);
        for r in &folder.requests {
            if let Some(score) = fuzzy_score_request(matcher, r, query) {
                out.push((score, collection_id, path.clone(), r));
            }
        }
        collect_matching_requests(&folder.folders, path, query, collection_id, matcher, out);
        path.pop();
    }
}

const PRE_REQUEST_SNIPPETS: &[(&str, &str)] = &[
    (
        "pm.environment.set",
        "pm.environment.set(\"key\", \"value\")\n",
    ),
    ("pm.globals.set", "pm.globals.set(\"key\", \"value\")\n"),
    (
        "pm.collectionVariables.set",
        "pm.collectionVariables.set(\"key\", \"value\")\n",
    ),
    (
        "pm.request:setHeader",
        "pm.request:setHeader(\"key\", \"value\")\n",
    ),
    (
        "pm.sendRequest",
        "pm.sendRequest(\"https://example.com\", function(err, response)\n  console.log(err, response and response.status)\nend)\n",
    ),
    ("console.log", "console.log(\"...\")\n"),
];

const TEST_SNIPPETS: &[(&str, &str)] = &[
    (
        "pm.test",
        "pm.test(\"Status is 200\", function()\n  assert(pm.response.status == 200)\nend)\n",
    ),
    (
        "pm.environment.set",
        "pm.environment.set(\"key\", \"value\")\n",
    ),
    ("pm.globals.set", "pm.globals.set(\"key\", \"value\")\n"),
    (
        "pm.collectionVariables.set",
        "pm.collectionVariables.set(\"key\", \"value\")\n",
    ),
    ("pm.response:json()", "local data = pm.response:json()\n"),
    ("console.log", "console.log(\"...\")\n"),
];

/// A row of clickable snippet buttons above the script editor — Postman
/// shows an equivalent list next to its pre-request/test editors; this
/// appends the matching Lua `pm.*` boilerplate to the script on click
/// rather than replacing it, so it composes with whatever's already there.
fn snippet_buttons(ui: &mut egui::Ui, snippets: &[(&str, &str)], script: &mut String) {
    ui.horizontal_wrapped(|ui| {
        for (label, template) in snippets {
            if ui.small_button(*label).clicked() {
                if !script.is_empty() && !script.ends_with('\n') {
                    script.push('\n');
                }
                script.push_str(template);
            }
        }
    });
}

/// A plain monospace multiline editor for Lua scripts. No syntax
/// highlighting (`syntax.rs` only knows JSON/HTML/plain text) — good enough
/// for a first pass at pre-request/post-response scripting.
fn script_editor(ui: &mut egui::Ui, id_salt: &str, script: &mut String, vim_enabled: bool) {
    ui.push_id(id_salt, |ui| {
        vim_editor::vim_multiline_edit(
            ui,
            egui::Id::new(id_salt),
            script,
            vim_enabled,
            false,
            |te| {
                te.code_editor()
                    .desired_rows(12)
                    .desired_width(f32::INFINITY)
            },
        );
    });
}

/// Every hotkey-assignable character — `'0'..='9'` then `'a'..='z'`, the
/// same key space AeroSpace itself uses for workspace keys, *minus*
/// `h`/`j`/`k`/`l`: that whole row is reserved for AeroSpace-style
/// Option+H/L tab navigation (`j`/`k` aren't bound to anything yet, but are
/// held back rather than handed out, since the user explicitly wants that
/// whole row for navigation and just hasn't decided what `j`/`k` do yet).
const HOTKEY_CHARS: &str = "0123456789abcdefgimnopqrstuvwxyz";

/// Window width below which the sidebar auto-hides itself (see
/// `sidebar_auto_hidden`) — a UX heuristic tuned to roughly "the sidebar's
/// own width plus enough of the request editor to still be usable," not a
/// value derived from anything more precise.
const SIDEBAR_AUTO_HIDE_WIDTH: f32 = 640.0;

/// Maps a hotkey character to the `egui::Key` that must be pressed for it —
/// `None` for anything outside `HOTKEY_CHARS`.
fn key_for_char(c: char) -> Option<egui::Key> {
    match c {
        '0' => Some(egui::Key::Num0),
        '1' => Some(egui::Key::Num1),
        '2' => Some(egui::Key::Num2),
        '3' => Some(egui::Key::Num3),
        '4' => Some(egui::Key::Num4),
        '5' => Some(egui::Key::Num5),
        '6' => Some(egui::Key::Num6),
        '7' => Some(egui::Key::Num7),
        '8' => Some(egui::Key::Num8),
        '9' => Some(egui::Key::Num9),
        'a' => Some(egui::Key::A),
        'b' => Some(egui::Key::B),
        'c' => Some(egui::Key::C),
        'd' => Some(egui::Key::D),
        'e' => Some(egui::Key::E),
        'f' => Some(egui::Key::F),
        'g' => Some(egui::Key::G),
        'h' => Some(egui::Key::H),
        'i' => Some(egui::Key::I),
        'j' => Some(egui::Key::J),
        'k' => Some(egui::Key::K),
        'l' => Some(egui::Key::L),
        'm' => Some(egui::Key::M),
        'n' => Some(egui::Key::N),
        'o' => Some(egui::Key::O),
        'p' => Some(egui::Key::P),
        'q' => Some(egui::Key::Q),
        'r' => Some(egui::Key::R),
        's' => Some(egui::Key::S),
        't' => Some(egui::Key::T),
        'u' => Some(egui::Key::U),
        'v' => Some(egui::Key::V),
        'w' => Some(egui::Key::W),
        'x' => Some(egui::Key::X),
        'y' => Some(egui::Key::Y),
        'z' => Some(egui::Key::Z),
        _ => None,
    }
}

/// One-time upgrade path: before this feature existed, Alt+1..9 always
/// opened the first 9 top-level requests of a collection specifically
/// named "Saved Requests" (the old `App::open_saved_request`). Since
/// that's now superseded by general per-request hotkeys, this preserves
/// the exact same muscle memory for anyone who already relied on it — but
/// only if `hotkey_bindings` is still empty (a fresh install, or the first run
/// after upgrading; once the user assigns *any* hotkey, even to an
/// unrelated request, this never runs again).
fn migrate_number_shortcuts_to_hotkey_bindings(data: &mut AppData) {
    if !data.hotkey_bindings.is_empty() {
        return;
    }
    let Some(collection) = data.collections.iter().find(|c| c.name == "Saved Requests") else {
        return;
    };
    for (i, req) in collection.requests.iter().take(9).enumerate() {
        let key = char::from_digit(i as u32 + 1, 10).expect("0..9 always converts");
        data.hotkey_bindings.insert(key, req.id);
    }
}

/// One line of the Console — timestamp, colored method, url, colored
/// status/error, duration. Shared by `console_panel`'s compact single-line
/// view (the newest entry) and its expanded full-history list, so the two
/// never drift apart in what they show for the same entry.
fn console_entry_row(ui: &mut egui::Ui, entry: &ConsoleEntry) -> egui::Response {
    ui.horizontal(|ui| {
        ui.weak(entry.timestamp.format("%H:%M:%S").to_string());
        ui.colored_label(theme::method_color(entry.method), entry.method.as_str());
        ui.label(&entry.url);
        let status_text = match (entry.status, &entry.error) {
            (Some(status), _) => format!("{status}"),
            (None, Some(err)) => format!("ERROR: {err}"),
            (None, None) => "(no response)".to_string(),
        };
        let color = match entry.status {
            Some(s) => theme::status_color(s),
            None => egui::Color32::RED,
        };
        ui.colored_label(color, status_text);
        if let Some(ms) = entry.duration_ms {
            ui.weak(format!("{ms}ms"));
        }
    })
    .response
}

/// Recursively inserts every folder's own id (at every depth) into
/// `expanded` — the "Expand all" button's helper, since `flatten_visible_rows`
/// only descends into a folder that's already a member of the set.
fn insert_all_folder_ids(folders: &[Folder], expanded: &mut std::collections::HashSet<Uuid>) {
    for f in folders {
        expanded.insert(f.id);
        insert_all_folder_ids(&f.folders, expanded);
    }
}

/// Shared Ctrl+N/P cursor-move logic for a plain (non-tree) sidebar list —
/// History, Environments. Moves `*focused` up/down within `0..len`, gated
/// on nothing having keyboard focus and the command palette being closed
/// (same guard the tree's own Ctrl+N/P uses, and the same reasoning: a
/// text field's typing shouldn't be hijacked, and the palette already owns
/// these keys while open). Only one sidebar tab ever renders at a time
/// (`sidebar_tab`), which is what scopes these keys to "whichever list
/// you're currently looking at" — the same way clicking between panes in
/// a native macOS app changes which one arrow keys apply to, without
/// needing a separate explicit "focused panel" concept here.
/// Returns whether Enter was pressed this frame, so the caller decides
/// what "open the focused row" means for its own list.
fn handle_list_nav_keys(
    ui: &mut egui::Ui,
    search_open: bool,
    len: usize,
    focused: &mut Option<usize>,
) -> bool {
    if len == 0 || search_open || ui.ctx().memory(|m| m.focused().is_some()) {
        return false;
    }
    if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::N)) {
        *focused = Some(focused.map_or(0, |i| (i + 1).min(len - 1)));
    }
    if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::P)) {
        *focused = Some(focused.map_or(0, |i| i.saturating_sub(1)));
    }
    ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
}

/// A small clickable expand/collapse toggle rendered as a real triangle
/// icon — drawn with `Painter` shapes via egui's own
/// `collapsing_header::paint_default_icon` (the exact routine
/// `CollapsingHeader` uses internally for its own disclosure arrow), not a
/// font glyph. The earlier plain `"v"`/`">"` text was a deliberate
/// workaround for this bundled font's missing Unicode triangle glyphs
/// (`▶`/`▼`, tofu boxes confirmed in Phases 2/3/8/9/15) — painting the
/// triangle as vector shapes sidesteps the font question entirely, so this
/// finally gets a real icon instead of another glyph gamble.
fn expand_toggle_icon(ui: &mut egui::Ui, is_open: bool) -> egui::Response {
    let size = ui.spacing().icon_width.max(14.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let openness = if is_open { 1.0 } else { 0.0 };
        egui::containers::collapsing_header::paint_default_icon(ui, openness, &response);
    }
    response
}

/// Opens `add_contents` as a context menu when the pointer secondary-clicks
/// anywhere inside `response`'s rect — a replacement for
/// `Response::context_menu` for a response that doesn't (and, for the
/// click-overlap reason below, *shouldn't*) sense clicks itself.
/// `dnd_drop_zone`'s own response is allocated with `Sense::hover()`
/// (confirmed against the cached egui-0.35.0 source,
/// `Frame::allocate_space`), so `.context_menu()` never fires on it
/// directly. The first fix attempt — `.interact(Sense::click())` to
/// retroactively add click-sensing — was wrong and caused a real
/// regression, caught by re-running this row's own existing tests rather
/// than assumed safe: it registers the *entire* row as a new click-sensing
/// widget overlapping the toggle icon / name button already inside it, and
/// egui's own click arbitration only lets one overlapping widget "win" a
/// given click — which broke expand/collapse entirely. This instead reads
/// the raw secondary-click pointer state directly (a pure read, not a new
/// widget registration), so it can never compete with anything already
/// interactive inside the row.
fn row_context_menu(
    ui: &egui::Ui,
    response: &egui::Response,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let secondary_clicked = response.contains_pointer()
        && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary));
    egui::Popup::menu(response)
        .open_memory(secondary_clicked.then_some(egui::SetOpenCommand::Bool(true)))
        .at_pointer_fixed()
        .show(add_contents);
}

/// Draws one row of the flattened, virtualized collection tree — the
/// replacement for the old recursive `folder_contents` (which held live
/// `&mut Folder`/`&mut RequestItem` references and drew a nested
/// `CollapsingHeader` per folder). Every row is now just data
/// (`model::Row`), dispatched by kind to one of the three row-renderers
/// below. Toolbar actions that used to live inside a collection/folder's
/// `CollapsingHeader` body (+Request, +Folder, Delete, Export, Run) — only
/// reachable when expanded — moved into the row's own context menu instead
/// (alongside Rename/Duplicate/Delete, which were already there), so every
/// action is reachable regardless of expand state and every row stays a
/// single fixed height, which `show_rows`-based virtualization requires.
fn render_tree_row(
    ui: &mut egui::Ui,
    row: &model::Row,
    expanded: &mut std::collections::HashSet<Uuid>,
    active_id: Uuid,
    open_ids: &std::collections::HashSet<Uuid>,
    renaming: &mut Option<(Uuid, String)>,
    actions: &mut Vec<PendingAction>,
    focused: bool,
) {
    const INDENT_PX: f32 = 16.0;
    // Emacs-style keyboard navigation cursor (Ctrl+N/P) — an accent-colored
    // outline, not a fill: a fill sat behind the row's own widgets and
    // visually swallowed their normal mouse-hover highlight (reported: hover
    // "bị che" — covered). A stroke on a zero-margin `Frame` never paints
    // over the interior, so widget hover stays visible, and (matching the
    // active-tab chip's own convention in `tab_bar`) never changes the
    // row's height either — required since `show_rows` virtualizes on one
    // fixed height for every row.
    let mut frame = egui::Frame::new();
    if focused {
        frame = frame.stroke(egui::Stroke::new(1.0, theme::ACCENT));
    }
    frame.show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.add_space(row.depth as f32 * INDENT_PX);
            match &row.kind {
                model::RowKind::Collection { id } => {
                    render_collection_row(ui, *id, &row.name, expanded, renaming, actions);
                }
                model::RowKind::Folder {
                    collection,
                    parent_path,
                    id,
                } => {
                    render_folder_row(
                        ui,
                        *collection,
                        parent_path,
                        *id,
                        &row.name,
                        expanded,
                        renaming,
                        actions,
                    );
                }
                model::RowKind::Request {
                    collection,
                    folder_path,
                    id,
                } => {
                    render_request_row(
                        ui,
                        *collection,
                        folder_path,
                        *id,
                        &row.name,
                        row.method,
                        row.hotkey,
                        active_id,
                        open_ids,
                        renaming,
                        actions,
                    );
                }
            }
        });
    });
}

fn render_collection_row(
    ui: &mut egui::Ui,
    collection_id: Uuid,
    name: &str,
    expanded: &mut std::collections::HashSet<Uuid>,
    renaming: &mut Option<(Uuid, String)>,
    actions: &mut Vec<PendingAction>,
) {
    let (zone, dropped) = ui.dnd_drop_zone::<DragPayload, _>(egui::Frame::default(), |ui| {
        if renaming
            .as_ref()
            .is_some_and(|(id, _)| *id == collection_id)
        {
            let (_, buf) = renaming.as_mut().unwrap();
            let resp = ui.add(egui::TextEdit::singleline(buf).desired_width(f32::INFINITY));
            if resp.lost_focus() {
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let new_name = buf.trim().to_string();
                    if !new_name.is_empty() {
                        actions.push(PendingAction::Rename {
                            id: collection_id,
                            new_name,
                        });
                    }
                }
                *renaming = None;
            } else {
                resp.request_focus();
            }
        } else {
            let is_open = expanded.contains(&collection_id);
            // See `expand_toggle_icon`'s own doc comment for why this is a
            // painted triangle, not a font glyph.
            let toggle_clicked = expand_toggle_icon(ui, is_open).clicked();
            // The name itself is *also* clickable and toggles the same
            // state — `CollapsingHeader` (what this replaced) let you click
            // anywhere on the header row, not just a dedicated arrow, and
            // existing tests/muscle-memory rely on clicking the name text
            // directly (e.g. `get_by_label("Demo")`).
            let name_clicked = ui
                .add(
                    egui::Button::new(egui::RichText::new(name).strong())
                        .truncate()
                        .frame(false),
                )
                .clicked();
            if toggle_clicked || name_clicked {
                if is_open {
                    expanded.remove(&collection_id);
                } else {
                    expanded.insert(collection_id);
                }
            }
            // A visible quick-add, not just the context-menu entry below —
            // "+ Request"/"+ Folder" used to live in the `CollapsingHeader`
            // body (only reachable when expanded) and moved into the
            // context menu during the tree-virtualization rewrite (Phase
            // 15), which made it a real discoverability regression: no
            // visible affordance at all unless you already knew to
            // right-click. Right-aligned via `Layout::right_to_left` so it
            // sits at the row's edge regardless of the name's own width.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("+").on_hover_text("Add request").clicked() {
                    actions.push(PendingAction::AddRequest {
                        collection: collection_id,
                        folder_path: Vec::new(),
                    });
                }
            });
        }
    });

    if let Some(dropped) = dropped {
        match (*dropped).clone() {
            DragPayload::Request {
                collection: c,
                folder_path: from_path,
                id,
            } if c == collection_id => {
                actions.push(PendingAction::MoveRequest {
                    collection: c,
                    from_path,
                    id,
                    to_path: Vec::new(),
                    before_id: None,
                });
            }
            DragPayload::Folder {
                collection: c,
                parent_path: from_path,
                id,
            } if c == collection_id => {
                actions.push(PendingAction::MoveFolder {
                    collection: c,
                    from_path,
                    id,
                    to_path: Vec::new(),
                    before_id: None,
                });
            }
            // Cross-collection drag-and-drop isn't supported.
            _ => {}
        }
    }

    // Real, reported bug: `dnd_drop_zone`'s own response is allocated with
    // `Sense::hover()` (confirmed against the cached egui-0.35.0 source,
    // `Frame::allocate_space` — `ui.allocate_rect(rect, Sense::hover())`),
    // so `Flags::CLICKED` never gets set on it and `.context_menu()` (which
    // checks `response.secondary_clicked()`) could never fire — right-
    // clicking anywhere on the row silently did nothing, regardless of
    // where you clicked. `.interact(Sense::click())` re-registers the same
    // rect/id with clicks unioned in, which is what actually makes the
    // right-click get detected (same fix already used for the command
    // palette's hand-painted rows).
    row_context_menu(ui, &zone.response, |ui| {
        if ui.button("Rename").clicked() {
            *renaming = Some((collection_id, name.to_string()));
            ui.close();
        }
        if ui.button("Edit").clicked() {
            actions.push(PendingAction::EditCollection(collection_id));
            ui.close();
        }
        if ui.button("Duplicate").clicked() {
            actions.push(PendingAction::DuplicateCollection(collection_id));
            ui.close();
        }
        if ui.button("Delete").clicked() {
            actions.push(PendingAction::DeleteCollection(collection_id));
            ui.close();
        }
        ui.separator();
        if ui.button("Export").clicked() {
            actions.push(PendingAction::ExportCollection(collection_id));
            ui.close();
        }
        if ui.button("Run").clicked() {
            actions.push(PendingAction::RunCollection {
                collection: collection_id,
                name: name.to_string(),
            });
            ui.close();
        }
        ui.separator();
        if ui.button("+ Request").clicked() {
            actions.push(PendingAction::AddRequest {
                collection: collection_id,
                folder_path: Vec::new(),
            });
            ui.close();
        }
        if ui.button("+ Folder").clicked() {
            actions.push(PendingAction::AddFolder {
                collection: collection_id,
                folder_path: Vec::new(),
            });
            ui.close();
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn render_folder_row(
    ui: &mut egui::Ui,
    collection_id: Uuid,
    parent_path: &[Uuid],
    folder_id: Uuid,
    name: &str,
    expanded: &mut std::collections::HashSet<Uuid>,
    renaming: &mut Option<(Uuid, String)>,
    actions: &mut Vec<PendingAction>,
) {
    let mut child_path = parent_path.to_vec();
    child_path.push(folder_id);
    let payload = DragPayload::Folder {
        collection: collection_id,
        parent_path: parent_path.to_vec(),
        id: folder_id,
    };
    let drag_id = egui::Id::new(("folder-row", collection_id, parent_path.to_vec(), folder_id));

    let (zone, dropped) = ui.dnd_drop_zone::<DragPayload, _>(egui::Frame::default(), |ui| {
        if renaming.as_ref().is_some_and(|(id, _)| *id == folder_id) {
            let (_, buf) = renaming.as_mut().unwrap();
            let resp = ui.add(egui::TextEdit::singleline(buf).desired_width(160.0));
            if resp.lost_focus() {
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let new_name = buf.trim().to_string();
                    if !new_name.is_empty() {
                        actions.push(PendingAction::Rename {
                            id: folder_id,
                            new_name,
                        });
                    }
                }
                *renaming = None;
            } else {
                resp.request_focus();
            }
        } else {
            // A dedicated drag handle, separate from the expand toggle and
            // label — wrapping the whole row would intercept plain clicks
            // (the same click-through-drag-source bug fixed twice already
            // this project, Phases 3/8).
            ui.dnd_drag_source(drag_id, payload.clone(), |ui| {
                ui.weak("::");
            });
            let is_open = expanded.contains(&folder_id);
            let toggle_clicked = expand_toggle_icon(ui, is_open).clicked();
            // The name is also clickable and toggles the same state — see
            // the collection row's identical treatment for why.
            let name_clicked = ui
                .add(egui::Button::new(name).truncate().frame(false))
                .clicked();
            if toggle_clicked || name_clicked {
                if is_open {
                    expanded.remove(&folder_id);
                } else {
                    expanded.insert(folder_id);
                }
            }
            // See the collection row's identical addition for why "+" is
            // back as a visible button, not just the context-menu entry —
            // same reasoning for "🗑" (delete), added right after it:
            // requested directly once someone actually went looking for it
            // and found it only reachable via right-click.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, "🗑", &format!("Delete folder \"{name}\"")).clicked() {
                    actions.push(PendingAction::DeleteFolder {
                        collection: collection_id,
                        parent_path: parent_path.to_vec(),
                        id: folder_id,
                    });
                }
                if ui.small_button("+").on_hover_text("Add request").clicked() {
                    actions.push(PendingAction::AddRequest {
                        collection: collection_id,
                        folder_path: child_path.clone(),
                    });
                }
            });
        }
    });

    if let Some(dropped) = dropped {
        match (*dropped).clone() {
            DragPayload::Request {
                collection: c,
                folder_path: from_path,
                id,
            } if c == collection_id => {
                actions.push(PendingAction::MoveRequest {
                    collection: c,
                    from_path,
                    id,
                    to_path: child_path.clone(),
                    before_id: None,
                });
            }
            DragPayload::Folder {
                collection: c,
                parent_path: from_path,
                id,
            } if c == collection_id && id != folder_id => {
                actions.push(PendingAction::MoveFolder {
                    collection: c,
                    from_path,
                    id,
                    to_path: child_path.clone(),
                    before_id: None,
                });
            }
            _ => {} // cross-collection drops, and dropping a folder onto itself, are no-ops
        }
    }

    // Real, reported bug: `dnd_drop_zone`'s own response is allocated with
    // `Sense::hover()` (confirmed against the cached egui-0.35.0 source,
    // `Frame::allocate_space` — `ui.allocate_rect(rect, Sense::hover())`),
    // so `Flags::CLICKED` never gets set on it and `.context_menu()` (which
    // checks `response.secondary_clicked()`) could never fire — right-
    // clicking anywhere on the row silently did nothing, regardless of
    // where you clicked. `.interact(Sense::click())` re-registers the same
    // rect/id with clicks unioned in, which is what actually makes the
    // right-click get detected (same fix already used for the command
    // palette's hand-painted rows).
    row_context_menu(ui, &zone.response, |ui| {
        if ui.button("Rename").clicked() {
            *renaming = Some((folder_id, name.to_string()));
            ui.close();
        }
        if ui.button("Edit").clicked() {
            actions.push(PendingAction::EditFolder {
                collection: collection_id,
                folder_path: child_path.clone(),
            });
            ui.close();
        }
        if ui.button("Duplicate").clicked() {
            actions.push(PendingAction::DuplicateFolder {
                collection: collection_id,
                parent_path: parent_path.to_vec(),
                id: folder_id,
            });
            ui.close();
        }
        if ui.button("Delete").clicked() {
            actions.push(PendingAction::DeleteFolder {
                collection: collection_id,
                parent_path: parent_path.to_vec(),
                id: folder_id,
            });
            ui.close();
        }
        if ui.button("Run folder").clicked() {
            actions.push(PendingAction::RunFolder {
                collection: collection_id,
                folder_path: child_path.clone(),
                name: name.to_string(),
            });
            ui.close();
        }
        ui.separator();
        if ui.button("+ Request").clicked() {
            actions.push(PendingAction::AddRequest {
                collection: collection_id,
                folder_path: child_path.clone(),
            });
            ui.close();
        }
        if ui.button("+ Folder").clicked() {
            actions.push(PendingAction::AddFolder {
                collection: collection_id,
                folder_path: child_path.clone(),
            });
            ui.close();
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn render_request_row(
    ui: &mut egui::Ui,
    collection_id: Uuid,
    folder_path: &[Uuid],
    req_id: Uuid,
    name: &str,
    method: Option<Method>,
    hotkey: Option<char>,
    active_id: Uuid,
    open_ids: &std::collections::HashSet<Uuid>,
    renaming: &mut Option<(Uuid, String)>,
    actions: &mut Vec<PendingAction>,
) {
    let payload = DragPayload::Request {
        collection: collection_id,
        folder_path: folder_path.to_vec(),
        id: req_id,
    };
    let drag_id = egui::Id::new(("req-row", collection_id, folder_path.to_vec(), req_id));

    let (zone, dropped) = ui.dnd_drop_zone::<DragPayload, _>(egui::Frame::default(), |ui| {
        // A dedicated drag handle, separate from the selectable label below
        // — wrapping the whole row in `dnd_drag_source` (as an earlier
        // version did) intercepts plain clicks before the label's own
        // click-sense ever sees them.
        ui.dnd_drag_source(drag_id, payload.clone(), |ui| {
            ui.weak("::");
        });

        if renaming.as_ref().is_some_and(|(id, _)| *id == req_id) {
            let (_, buf) = renaming.as_mut().unwrap();
            let resp = ui.add(egui::TextEdit::singleline(buf).desired_width(160.0));
            if resp.lost_focus() {
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let new_name = buf.trim().to_string();
                    if !new_name.is_empty() {
                        actions.push(PendingAction::Rename {
                            id: req_id,
                            new_name,
                        });
                    }
                }
                *renaming = None;
            } else {
                resp.request_focus();
            }
        } else {
            if let Some(method) = method {
                ui.colored_label(theme::method_color(method), method.as_str());
            }
            if let Some(key) = hotkey {
                ui.weak(format!("[{key}]"));
            }
            // A small marker for "open in some other tab" — the active
            // tab's own row still gets the full `selectable_label`
            // highlight below. Real-world imported requests can have very
            // long names (and, occasionally, literal embedded newlines
            // from a multi-line Postman description mistakenly used as the
            // name) — sanitizing the newline and truncating with an
            // ellipsis keeps one long name from stretching the row past
            // the sidebar's width.
            let display_name = name.replace('\n', " ");
            let label = if req_id != active_id && open_ids.contains(&req_id) {
                format!("• {display_name}")
            } else {
                display_name
            };
            if ui
                .add(egui::Button::selectable(req_id == active_id, label).truncate())
                .clicked()
            {
                actions.push(PendingAction::Load {
                    collection: collection_id,
                    folder_path: folder_path.to_vec(),
                    id: req_id,
                });
            }
            // A visible quick-delete, not just the context-menu entry
            // below — requested directly once someone actually went
            // looking for it and found it only reachable via right-click
            // (the same discoverability fix already applied to the
            // collection/folder rows' own "+" button).
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, "🗑", &format!("Delete request \"{name}\"")).clicked() {
                    actions.push(PendingAction::DeleteRequest {
                        collection: collection_id,
                        folder_path: folder_path.to_vec(),
                        id: req_id,
                    });
                }
            });
        }
    });

    if let Some(dropped) = dropped
        && let DragPayload::Request {
            collection: c,
            folder_path: from_path,
            id,
        } = (*dropped).clone()
        && c == collection_id
    {
        actions.push(PendingAction::MoveRequest {
            collection: c,
            from_path,
            id,
            to_path: folder_path.to_vec(),
            before_id: Some(req_id),
        });
    }

    // Real, reported bug: `dnd_drop_zone`'s own response is allocated with
    // `Sense::hover()` (confirmed against the cached egui-0.35.0 source,
    // `Frame::allocate_space` — `ui.allocate_rect(rect, Sense::hover())`),
    // so `Flags::CLICKED` never gets set on it and `.context_menu()` (which
    // checks `response.secondary_clicked()`) could never fire — right-
    // clicking anywhere on the row silently did nothing, regardless of
    // where you clicked. `.interact(Sense::click())` re-registers the same
    // rect/id with clicks unioned in, which is what actually makes the
    // right-click get detected (same fix already used for the command
    // palette's hand-painted rows).
    row_context_menu(ui, &zone.response, |ui| {
        if ui.button("Rename").clicked() {
            *renaming = Some((req_id, name.to_string()));
            ui.close();
        }
        if ui.button("Duplicate").clicked() {
            actions.push(PendingAction::DuplicateRequest {
                collection: collection_id,
                folder_path: folder_path.to_vec(),
                id: req_id,
            });
            ui.close();
        }
        if ui.button("Delete").clicked() {
            actions.push(PendingAction::DeleteRequest {
                collection: collection_id,
                folder_path: folder_path.to_vec(),
                id: req_id,
            });
            ui.close();
        }
        ui.separator();
        if ui.button("Assign hotkey…").clicked() {
            actions.push(PendingAction::AssignHotkey(req_id));
            ui.close();
        }
    });
}

/// Recursive rename-by-id search through a folder subtree (used by
/// `App::apply_rename` once the collection-level and its-own-root-request
/// checks have missed) — `true` once found and renamed, `false` if `id`
/// doesn't match anything in this subtree.
fn rename_in_folders(folders: &mut [Folder], id: Uuid, new_name: &str) -> bool {
    for folder in folders {
        if folder.id == id {
            folder.name = new_name.to_string();
            return true;
        }
        if let Some(req) = folder.requests.iter_mut().find(|r| r.id == id) {
            req.name = new_name.to_string();
            return true;
        }
        if rename_in_folders(&mut folder.folders, id, new_name) {
            return true;
        }
    }
    false
}

/// An icon-only button (e.g. a "🗑" delete glyph) with a real accessible
/// name: without this, a screen reader has nothing to announce it by but the
/// raw emoji character, since egui derives a button's accessibility label
/// from its visible text by default.
fn icon_button(ui: &mut egui::Ui, icon: &str, accessible_label: &str) -> egui::Response {
    let response = ui.small_button(icon).on_hover_text(accessible_label);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, accessible_label)
    });
    response
}

/// A native "delete this?" confirmation — blocking, same established
/// pattern as `rfd::FileDialog` (and `close_tab_with_confirmation`'s own
/// dialog) elsewhere in this file. Used before every destructive delete
/// (collection/folder/request/environment/history) — deliberately *not*
/// used for a single key-value table row (removing one header/param is a
/// frequent, low-stakes, easily-redone edit, not the kind of data loss this
/// is for).
fn confirm_delete(what: &str) -> bool {
    rfd::MessageDialog::new()
        .set_title("Delete?")
        .set_description(format!("Delete {what}? This can't be undone."))
        .set_buttons(rfd::MessageButtons::YesNo)
        .show()
        == rfd::MessageDialogResult::Yes
}

/// One-line summary for `App::import_message` after an import: clean if
/// there were no non-fatal warnings, otherwise names the count and lists
/// them (they're short and few — an unsupported auth type, a script that
/// needed the JS-as-comment treatment, ...).
fn describe_import_result(what: &str, warnings: &[String]) -> String {
    if warnings.is_empty() {
        format!("Imported {what}")
    } else {
        format!(
            "Imported {what} with {} warning(s): {}",
            warnings.len(),
            warnings.join("; ")
        )
    }
}

/// Summarizes a batch of per-file import results (Postman/OpenAPI/environment
/// multi-file import) into one status message. Real bug report: joining
/// every file's own result with `" | "` (the original multi-file-import
/// behavior) reads fine for 2-3 files but produces a message long enough to
/// overflow the sidebar once someone imports many collections at once — so
/// a small batch still gets the old per-file detail (useful: it names each
/// imported collection), but a larger one collapses to a single count,
/// spelling out only the files that actually failed rather than every
/// success too.
fn describe_import_batch(results: Vec<String>) -> String {
    const SMALL_BATCH: usize = 3;
    let total = results.len();
    if total <= SMALL_BATCH {
        return results.join(" | ");
    }
    let failed: Vec<&String> = results
        .iter()
        .filter(|r| r.contains("failed") || r.contains("Could not read"))
        .collect();
    if failed.is_empty() {
        format!("Imported {total} files successfully.")
    } else {
        let ok = total - failed.len();
        let details = failed
            .iter()
            .map(|s| format!("- {s}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Imported {ok}/{total} files; {} failed:\n{details}",
            failed.len()
        )
    }
}

/// Renders the `import_message` status banner as a bounded, dismissible
/// element instead of a bare unwrapped label — a real bug report: importing
/// many files at once (see `describe_import_batch`) could still produce a
/// message wide/tall enough to overflow the sidebar, and there was no way
/// to dismiss it, so it just sat there indefinitely. Wraps to the available
/// width, caps its height with an internal scrollbar for a many-line
/// message, and a small "×" clears it.
fn import_message_banner(ui: &mut egui::Ui, message: &mut Option<String>) {
    let Some(msg) = message.clone() else {
        return;
    };
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.weak("Status");
            if ui.small_button("\u{d7}").on_hover_text("Dismiss").clicked() {
                *message = None;
            }
        });
        egui::ScrollArea::vertical()
            .max_height(80.0)
            .show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(&msg).weak()).wrap());
            });
    });
}

/// Parses Postman-style bulk-edit text — one `key: value` per line — back
/// into `KeyValue` rows. A line whose first non-whitespace characters are
/// `//` is a *disabled* entry (Postman's own bulk-edit convention for
/// "commented out"): the marker is stripped before parsing the rest of the
/// line normally. A line with no `:` becomes a key with an empty value
/// (matches typing just a key and nothing else). Blank lines are skipped
/// entirely, so round-tripping through `format_bulk_edit_text` and back
/// doesn't accumulate stray empty rows.
fn parse_bulk_edit_text(text: &str) -> Vec<KeyValue> {
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            let (enabled, rest) = match trimmed.strip_prefix("//") {
                Some(rest) => (false, rest.trim_start()),
                None => (true, trimmed),
            };
            let (key, value) = match rest.split_once(':') {
                Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
                None => (rest.to_string(), String::new()),
            };
            Some(KeyValue {
                key,
                value,
                enabled,
            })
        })
        .collect()
}

/// The inverse of `parse_bulk_edit_text` — one `key: value` line per item,
/// disabled entries prefixed with `// `.
fn format_bulk_edit_text(items: &[KeyValue]) -> String {
    items
        .iter()
        .map(|kv| {
            let line = format!("{}: {}", kv.key, kv.value);
            if kv.enabled {
                line
            } else {
                format!("// {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn key_value_table(ui: &mut egui::Ui, id_salt: &str, items: &mut Vec<KeyValue>) {
    // Bulk-edit mode + its raw-text buffer are kept in egui's own per-`Id`
    // persistent memory (`Context::data_mut`, same mechanism already used
    // for the response-body wrap-width cache above) rather than as new
    // `App`/`OpenTab` fields — this function is a free function called
    // from several unrelated tables (params/headers/form/env vars), each
    // needing its own independent toggle state keyed by its own
    // `id_salt`, and egui's `Id`-keyed memory is exactly what that's for.
    let bulk_mode_id = egui::Id::new((id_salt, "bulk_edit_mode"));
    let bulk_text_id = egui::Id::new((id_salt, "bulk_edit_text"));
    let mut bulk_mode = ui
        .ctx()
        .data_mut(|d| d.get_temp::<bool>(bulk_mode_id))
        .unwrap_or(false);

    let toggle_label = if bulk_mode { "Table Edit" } else { "Bulk Edit" };
    if ui.small_button(toggle_label).clicked() {
        if bulk_mode {
            // Leaving bulk mode: parse whatever's in the text buffer back
            // into real rows.
            let text = ui
                .ctx()
                .data_mut(|d| d.get_temp::<String>(bulk_text_id))
                .unwrap_or_default();
            *items = parse_bulk_edit_text(&text);
        } else {
            // Entering bulk mode: seed the text buffer from the current
            // rows so it starts showing exactly what's already there.
            let text = format_bulk_edit_text(items);
            ui.ctx().data_mut(|d| d.insert_temp(bulk_text_id, text));
        }
        bulk_mode = !bulk_mode;
        ui.ctx()
            .data_mut(|d| d.insert_temp(bulk_mode_id, bulk_mode));
    }

    if bulk_mode {
        let mut text = ui
            .ctx()
            .data_mut(|d| d.get_temp::<String>(bulk_text_id))
            .unwrap_or_default();
        let response = ui.add(
            egui::TextEdit::multiline(&mut text)
                .hint_text("key: value\n// disabled-key: value")
                .desired_rows(6)
                .desired_width(f32::INFINITY),
        );
        if response.changed() {
            ui.ctx().data_mut(|d| d.insert_temp(bulk_text_id, text));
        }
        return;
    }

    let mut remove_idx: Option<usize> = None;
    // Reserve space for the checkbox, delete button, and row spacing, then
    // split what's left between the two columns so they can never together
    // demand more than what's actually available. No fixed-pixel floor here:
    // this can run inside a resizable panel (e.g. the horizontal request/
    // response split), and content that overflows its allotted space would
    // make that panel grow a little more every single frame.
    //
    // Deliberately NOT an `egui::Grid`: inside a Grid, only the *last* column
    // gets its width computed from real remaining space — every interior
    // column (which is what our key/value fields would be, since the delete
    // button comes after them) has its width memoized from whatever it
    // rendered at on a previous frame and then clamped to that from then on,
    // so a wider `desired_width` here would silently never take effect.
    // Plain per-row `horizontal()` layouts don't have that memoization.
    let total_width = ui.available_width();
    let overhead = 90.0;
    let usable = (total_width - overhead).max(0.0);
    let key_width = usable * 0.35;
    let value_width = usable * 0.65;
    for (i, kv) in items.iter_mut().enumerate() {
        ui.push_id((id_salt, i), |ui| {
            ui.horizontal(|ui| {
                let row_label = if kv.key.is_empty() {
                    format!("row {}", i + 1)
                } else {
                    kv.key.clone()
                };
                let checkbox = ui.checkbox(&mut kv.enabled, "");
                checkbox.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::Checkbox,
                        true,
                        kv.enabled,
                        format!("Enable {row_label}"),
                    )
                });
                ui.add(egui::TextEdit::singleline(&mut kv.key).desired_width(key_width));
                ui.add(egui::TextEdit::singleline(&mut kv.value).desired_width(value_width));
                if icon_button(ui, "🗑", &format!("Remove {row_label}")).clicked() {
                    remove_idx = Some(i);
                }
            });
        });
    }
    if ui.button("+ Add").clicked() {
        items.push(KeyValue::new());
    }
    if let Some(i) = remove_idx {
        items.remove(i);
    }
}

/// Like `key_value_table`, but each row also carries a Text/File toggle for
/// `multipart/form-data` bodies. A `File` row shows a native file picker
/// instead of a value text field.
fn multipart_form_table(ui: &mut egui::Ui, id_salt: &str, items: &mut Vec<FormField>) {
    let mut remove_idx: Option<usize> = None;
    let total_width = ui.available_width();
    let overhead = 190.0;
    let usable = (total_width - overhead).max(0.0);
    let key_width = usable * 0.35;
    let value_width = usable * 0.65;
    for (i, field) in items.iter_mut().enumerate() {
        ui.push_id((id_salt, i), |ui| {
            ui.horizontal(|ui| {
                let row_label = if field.key.is_empty() {
                    format!("row {}", i + 1)
                } else {
                    field.key.clone()
                };
                let checkbox = ui.checkbox(&mut field.enabled, "");
                checkbox.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::Checkbox,
                        true,
                        field.enabled,
                        format!("Enable {row_label}"),
                    )
                });
                ui.add(egui::TextEdit::singleline(&mut field.key).desired_width(key_width));
                let type_combo = egui::ComboBox::from_id_salt((id_salt, i, "type"))
                    .selected_text(match field.field_type {
                        FormFieldType::Text => "Text",
                        FormFieldType::File => "File",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut field.field_type, FormFieldType::Text, "Text");
                        ui.selectable_value(&mut field.field_type, FormFieldType::File, "File");
                    });
                type_combo.response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::ComboBox,
                        true,
                        format!(
                            "Field type for {row_label}: {}",
                            match field.field_type {
                                FormFieldType::Text => "Text",
                                FormFieldType::File => "File",
                            }
                        ),
                    )
                });
                match field.field_type {
                    FormFieldType::Text => {
                        ui.add(
                            egui::TextEdit::singleline(&mut field.value).desired_width(value_width),
                        );
                    }
                    FormFieldType::File => {
                        if ui
                            .button("Choose file…")
                            .on_hover_text(format!("Choose a file for {row_label}"))
                            .clicked()
                            && let Some(path) = rfd::FileDialog::new().pick_file()
                        {
                            field.file_path = Some(path.display().to_string());
                        }
                        let label = field.file_path.as_deref().unwrap_or("No file selected");
                        ui.add(egui::Label::new(label).truncate());
                    }
                }
                if icon_button(ui, "🗑", &format!("Remove {row_label}")).clicked() {
                    remove_idx = Some(i);
                }
            });
        });
    }
    if ui.button("+ Add").clicked() {
        items.push(FormField::new());
    }
    if let Some(i) = remove_idx {
        items.remove(i);
    }
}

/// File picker for `BodyMode::Binary`: the whole request body is the chosen
/// file's raw bytes.
fn binary_body_picker(ui: &mut egui::Ui, path: &mut Option<String>) {
    ui.horizontal(|ui| {
        if ui.button("Choose file…").clicked()
            && let Some(picked) = rfd::FileDialog::new().pick_file()
        {
            *path = Some(picked.display().to_string());
        }
        match path {
            Some(p) => {
                ui.label(p.as_str());
                if icon_button(ui, "🗑", "Clear selected file").clicked() {
                    *path = None;
                }
            }
            None => {
                ui.weak("No file selected");
            }
        }
    });
    if path.is_some() {
        ui.weak("The file's contents are sent as-is as the request body.");
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx()
            .set_theme(egui_theme_preference(self.settings.theme));
        theme::apply_postman_visuals(ui.ctx());
        self.poll_responses(ui.ctx());
        self.poll_runner(ui.ctx());
        self.autosave_if_dirty();

        let screen_size = ui.ctx().content_rect().size();
        if self.last_screen_size != Some(screen_size) {
            self.last_screen_size = Some(screen_size);
            let settle = std::time::Duration::from_millis(200);
            self.resize_settle_deadline = Some(std::time::Instant::now() + settle);
            ui.ctx().request_repaint_after(settle);
        }
        // Leader-key palette trigger: Space (only when nothing has
        // keyboard focus — otherwise it's just a literal space in whatever
        // text field is focused), then a matching sequence from
        // `LEADER_CHORDS` (today just "sf"). Replaces the old Alt+Space/
        // Cmd+K bindings outright, per explicit user preference.
        if ui.ctx().memory(|m| m.focused().is_none()) {
            if self
                .leader_deadline
                .is_some_and(|deadline| std::time::Instant::now() > deadline)
            {
                self.leader_buffer.clear();
                self.leader_deadline = None;
            }
            if self.leader_buffer.is_empty() {
                if ui.input(|i| i.key_pressed(egui::Key::Space)) {
                    self.leader_buffer.push(' ');
                    self.leader_deadline =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(600));
                }
            } else {
                for c in ('a'..='z').chain('0'..='9') {
                    let Some(key) = key_for_char(c) else {
                        continue;
                    };
                    if !ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key)) {
                        continue;
                    }
                    let mut candidate = self.leader_buffer.clone();
                    candidate.push(c);
                    if let Some((_, action, _)) =
                        LEADER_CHORDS.iter().find(|(seq, _, _)| *seq == candidate)
                    {
                        match action {
                            LeaderAction::OpenPalette => {
                                self.search_open = !self.search_open;
                                if self.search_open {
                                    self.search_query.clear();
                                    self.search_needs_focus = true;
                                    self.palette_selected = 0;
                                    // "Search files" — requests only, no
                                    // commands, per explicit request: this
                                    // chord is for jumping straight to a
                                    // request, not a mixed action+request
                                    // list.
                                    self.palette_scope = PaletteScope::RequestsOnly;
                                }
                            }
                            LeaderAction::SelectEnvironment => {
                                self.search_open = true;
                                // Matches `palette_actions`' own
                                // `"Environment: {name}"` label format
                                // exactly, so the palette's fuzzy filter
                                // narrows down to just those entries.
                                self.search_query = "Environment:".to_string();
                                self.search_needs_focus = true;
                                self.palette_selected = 0;
                                // Needs the action entries visible at all —
                                // unlike `<leader>sf`/`<leader>sc`, this
                                // isn't a single-kind view.
                                self.palette_scope = PaletteScope::Combined;
                            }
                            LeaderAction::OpenCommandPalette => {
                                self.search_open = !self.search_open;
                                if self.search_open {
                                    self.search_query.clear();
                                    self.search_needs_focus = true;
                                    self.palette_selected = 0;
                                    self.palette_scope = PaletteScope::CommandsOnly;
                                }
                            }
                            LeaderAction::AssignHotkeyToActiveRequest => {
                                self.pending_hotkey_assignment =
                                    Some(self.active_tab().current_request.id);
                            }
                            LeaderAction::ToggleSidebar => self.toggle_sidebar(),
                        }
                        self.leader_buffer.clear();
                        self.leader_deadline = None;
                    } else if LEADER_CHORDS
                        .iter()
                        .any(|(seq, _, _)| seq.starts_with(candidate.as_str()))
                    {
                        self.leader_buffer = candidate;
                        self.leader_deadline =
                            Some(std::time::Instant::now() + std::time::Duration::from_millis(600));
                    } else {
                        // Not a prefix of anything recognized — abandon
                        // silently, matching how neovim drops an
                        // unrecognized leader sequence.
                        self.leader_buffer.clear();
                        self.leader_deadline = None;
                    }
                    break;
                }
            }
        }
        self.leader_which_key_hint(ui.ctx());
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) && self.renaming.is_some() {
            self.renaming = None;
        }
        // AeroSpace-style hotkeys: assign any request to a `0`-`9`/`a`-`z`
        // key (sidebar context menu, "Assign hotkey…"), then Option/Alt+key
        // jumps straight to it from anywhere in the app.
        if let Some(request_id) = self.pending_hotkey_assignment {
            // Capturing the *next raw key* (no modifier) while the
            // "press a key to assign" banner is showing — not a hotkey
            // trigger itself, so this branch and the execution one below
            // are mutually exclusive per frame.
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.pending_hotkey_assignment = None;
            } else {
                for c in HOTKEY_CHARS.chars() {
                    if let Some(key) = key_for_char(c)
                        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key))
                    {
                        // A request/key can only be single-mapped — drop
                        // any prior key this same request held before
                        // giving it the new one.
                        self.data.hotkey_bindings.retain(|_, id| *id != request_id);
                        self.data.hotkey_bindings.insert(c, request_id);
                        self.pending_hotkey_assignment = None;
                        // `App::save` (not `self.save()`): inside `impl
                        // eframe::App for App`'s own `fn ui`, a bare
                        // `self.save()` resolves to the trait's own
                        // `eframe::App::save(&mut self, storage)` (an
                        // unrelated eframe lifecycle hook this app doesn't
                        // use), not this file's own `App::save(&self)` —
                        // the fully-qualified call sidesteps the ambiguity.
                        App::save(self);
                        break;
                    }
                }
            }
        } else {
            for c in HOTKEY_CHARS.chars() {
                if let Some(key) = key_for_char(c)
                    && ui.input_mut(|i| i.consume_key(egui::Modifiers::ALT, key))
                    && let Some(&request_id) = self.data.hotkey_bindings.get(&c)
                {
                    self.open_request_by_id(request_id);
                    break;
                }
            }
        }
        // Cmd/Ctrl+T / Cmd/Ctrl+W are safe cross-platform (apps commonly
        // claim these, they aren't OS-reserved). Tab-cycling is deliberately
        // bound to Ctrl+Tab (not Cmd+Tab) on every platform: macOS reserves
        // Cmd+Tab system-wide for the application switcher, so it never
        // reaches an ordinary app window — Chrome/VS Code use Ctrl+Tab for
        // in-app tab-cycling on every platform for exactly this reason.
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::T)) {
            self.new_blank_tab();
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)) {
            self.close_tab_with_confirmation(self.active_tab);
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Tab)) {
            self.active_tab = (self.active_tab + 1) % self.tabs.len();
        }
        // AeroSpace's own `alt-h`/`alt-l` convention for "focus the
        // window to the left/right" — same idea, one tab over. `j`/`k`
        // (the rest of that row) are deliberately left unbound; see
        // `HOTKEY_CHARS`'s own comment for why.
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::ALT, egui::Key::H)) {
            self.active_tab = (self.active_tab + self.tabs.len() - 1) % self.tabs.len();
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::ALT, egui::Key::L)) {
            self.active_tab = (self.active_tab + 1) % self.tabs.len();
        }
        // Vim's own jumplist convention (Ctrl+O back, Ctrl+I forward) —
        // here, across previously active *tabs* rather than cursor
        // positions. See `jump_tab_history` for the walk/skip-stale logic
        // and `record_tab_jump_if_changed` (called at the very end of this
        // function) for how entries get added in the first place.
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::O)) {
            self.jump_tab_history(-1);
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::I)) {
            self.jump_tab_history(1);
        }
        // Matches the existing "Save" button/`save_current_request`, same
        // convention as every other editor's save shortcut.
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
            self.save_current_request();
        }
        // Auto-hide/auto-restore the sidebar on window width, edge-triggered
        // off the narrow/wide transition (not "every frame while narrow")
        // so a manual re-open while still narrow isn't immediately fought —
        // see `sidebar_auto_hidden`'s own doc comment. `screen_size` above
        // is already computed once per frame for the resize-settle check,
        // reused here rather than measuring the window a second time.
        let narrow = screen_size.x < SIDEBAR_AUTO_HIDE_WIDTH;
        if narrow && !self.sidebar_was_narrow && self.sidebar_visible {
            self.sidebar_visible = false;
            self.sidebar_auto_hidden = true;
        } else if !narrow && self.sidebar_was_narrow && self.sidebar_auto_hidden {
            self.sidebar_visible = true;
            self.sidebar_auto_hidden = false;
        }
        self.sidebar_was_narrow = narrow;
        self.top_bar(ui);
        if self.sidebar_visible {
            self.sidebar(ui);
        }
        // Declared before `self.central(ui)` — side/bottom panels must be
        // added before the central panel that fills whatever space is left,
        // matching real Postman's own Console: a collapsible drawer along
        // the bottom of the window, not a full-screen view.
        if self.console_open {
            egui::Panel::bottom("console_bottom_panel")
                .resizable(true)
                .default_size(220.0)
                .min_size(80.0)
                .show(ui, |ui| self.console_panel(ui));
        }
        self.central(ui);
        self.command_palette(ui.ctx());
        // Captures every way `active_tab` could have just changed this
        // frame (tab-bar click, sidebar click, hotkey jump, Option+H/L,
        // Ctrl+Tab, ...) into the jumplist in one place, rather than each
        // of those call sites needing to remember to record it themselves.
        self.record_tab_jump_if_changed();
        if self.has_savable_dirty_tab() {
            // Edits are pending but still within the throttle window: keep
            // repainting so the debounce timer actually elapses instead of
            // waiting for unrelated input to trigger the next frame.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test for the `egui_kittest` test harness itself: proves a real
    /// `App` — built from a fixture `AppData` via `with_data`, never touching
    /// the user's actual data directory — can run headlessly and produce a
    /// snapshot PNG for visual review. This is the seed for self-verifying
    /// future UI phases (multi-tab, nested folders, auth UI, ...) without a
    /// browser-automation tool, since this is a native app, not a web page.
    ///
    /// `#[ignore]`d for now: `wgpu`'s adapter availability on headless CI
    /// runners (especially Linux GitHub Actions, which may lack a GPU) is
    /// unverified — enabling this in CI is Phase 12's job, not Phase 1's.
    /// Run manually with `cargo test -- --ignored egui_kittest_smoke`.
    #[test]
    #[ignore]
    fn egui_kittest_smoke_renders_a_fixture_request() {
        let mut data = AppData::default();
        let mut collection = Collection::new("Demo");
        let mut req = RequestItem::new("Get Users");
        req.url = "https://example.com/users".to_string();
        collection.requests.push(req);
        data.collections.push(collection);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));

        // Not `harness.run()`: an unsaved `current_request` makes the app
        // request a repaint every ~100ms forever (the autosave-debounce
        // check in `App::ui` never resolves for `RequestOrigin::Unsaved`),
        // which under the harness's accelerated clock looks like a
        // perpetually-repainting UI and trips `run`'s max-steps guard. A
        // single `step()` is exactly what the library recommends for that.
        harness.step();
        harness.snapshot("phase1_smoke");
    }

    /// The hotkey-assignment banner names the request being assigned
    /// (rather than a generic "this request") — driven by
    /// `pending_hotkey_assignment` directly, which the new `<leader>ah`
    /// chord (see `leader_key_space_a_h_starts_hotkey_assignment_for_the_active_request`)
    /// sets for the active tab's request.
    #[test]
    #[ignore]
    fn egui_kittest_smoke_renders_hotkey_assignment_banner() {
        let data = AppData::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();

        let tab = harness.state_mut().active_tab_mut();
        tab.current_request.name = "Get Users".to_string();
        let request_id = tab.current_request.id;
        harness.state_mut().pending_hotkey_assignment = Some(request_id);
        // Two steps, not one: `egui::Panel::top`'s height is remembered
        // from the previous frame, so a banner that wasn't there a moment
        // ago doesn't get room until the *next* frame notices the taller
        // content — the same "layout needs one more frame to settle" class
        // of quirk this codebase has hit repeatedly (CollapsingHeader
        // animations, a freshly-opened Window, tree-row expand/collapse).
        harness.step();
        harness.step();
        harness.snapshot("phase17_hotkey_assignment_banner");
    }

    /// A tab whose request has an assigned hotkey shows the same `"[key]"`
    /// badge in its tab-bar chip that the sidebar row already shows —
    /// requested directly: the hotkey badge existed in the sidebar but not
    /// on the open-tab chip, so there was no way to see (or be reminded of)
    /// a request's hotkey once it was already open.
    #[test]
    #[ignore]
    fn egui_kittest_smoke_renders_tab_bar_hotkey_badge() {
        let mut data = AppData::default();
        let req = RequestItem::new("Get Users");
        let req_id = req.id;
        data.hotkey_bindings.insert('1', req_id);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();

        harness.state_mut().active_tab_mut().current_request = req;
        harness.step();
        harness.snapshot("phase17_tab_bar_hotkey_badge");
    }

    /// Phase 3 self-check: a 2-level-nested fixture (collection → folder →
    /// subfolder → request) renders as an indented tree with correct
    /// labels — the same self-verification workflow as `phase1_smoke`,
    /// exercising the new recursive `folder_contents` renderer.
    #[test]
    #[ignore]
    fn egui_kittest_smoke_renders_nested_folders() {
        use egui_kittest::kittest::Queryable;

        let mut data = AppData::default();
        let mut collection = Collection::new("Demo");
        collection.requests.push(RequestItem::new("Get Users"));

        let mut tokens = Folder::new("Tokens");
        tokens.requests.push(RequestItem::new("Refresh Token"));

        let mut auth = Folder::new("Auth");
        auth.requests.push(RequestItem::new("Login"));
        auth.folders.push(tokens);

        collection.folders.push(auth);
        data.collections.push(collection);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));

        harness.step(); // see the `phase1_smoke` comment above for why not `run()`

        // Collections/folders now start collapsed by default (a later fix
        // for a real lag/overflow report once a large real-world import
        // made "everything expanded" render hundreds of rows at once) — so
        // this test, whose whole point is demonstrating the nested-folder
        // indentation, has to click each header open first.
        harness.get_by_label("Demo").click();
        // Two steps, not one: `collections_sidebar` flattens the tree into
        // `rows` once at the top of the frame, *before* the `show_rows`
        // loop that actually processes this click and mutates
        // `expanded_nodes` — so the newly-expanded row(s) don't appear
        // until the *next* frame's flattening picks up the updated state.
        // Same class of "layout needs one more frame to settle" quirk this
        // codebase has already hit with `CollapsingHeader` animations and a
        // freshly-opened `egui::Window` (Phase 11) — not a real bug, just
        // an artifact of single-stepping a headless harness through what a
        // live, continuously-repainting app resolves in well under a frame.
        harness.step();
        harness.step();
        harness.get_all_by_label("Auth").next().unwrap().click();
        // Same one-more-frame reasoning as the "Demo" click above.
        harness.step();
        harness.step();
        harness.snapshot("phase3_nested_folders");
    }

    /// "Expand all"/"Collapse all" should reveal/hide a deeply-nested
    /// request in one click each, without needing to open every ancestor
    /// folder by hand — the actual feature this test exercises, not just a
    /// static render.
    #[test]
    #[ignore]
    fn expand_all_and_collapse_all_toggle_the_whole_tree() {
        use egui_kittest::kittest::Queryable;

        let mut data = AppData::default();
        let mut collection = Collection::new("Demo");

        let mut tokens = Folder::new("Tokens");
        tokens.requests.push(RequestItem::new("Refresh Token"));

        let mut auth = Folder::new("Auth");
        auth.folders.push(tokens);

        collection.folders.push(auth);
        data.collections.push(collection);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();

        // Collapsed by default: a request two folders deep isn't visible
        // (or clickable) at all yet.
        assert!(
            harness.query_by_label("Refresh Token").is_none(),
            "starts collapsed — the deeply-nested request shouldn't be visible yet"
        );

        harness.get_by_label("Expand all").click();
        harness.step();
        assert!(
            harness.query_by_label("Refresh Token").is_some(),
            "\"Expand all\" should reveal every nested folder in one click"
        );

        harness.get_by_label("Collapse all").click();
        harness.step();
        assert!(
            harness.query_by_label("Refresh Token").is_none(),
            "\"Collapse all\" should hide it again"
        );
    }

    /// The sidebar's "Add sample requests" button is the self-service
    /// replacement for hand-seeding a demo collection — clicking it should
    /// add a real `Collection::sample()` and persist it, same as any other
    /// mutating sidebar action.
    #[test]
    #[ignore]
    fn clicking_add_sample_requests_button_adds_the_collection() {
        use egui_kittest::kittest::Queryable;

        let data = AppData::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();
        assert!(harness.state().data.collections.is_empty());

        harness.get_by_label("🧪 Add sample requests").click();
        harness.step();

        assert_eq!(harness.state().data.collections.len(), 1);
        assert_eq!(harness.state().data.collections[0].name, "Sample Requests");
        // Also actually visible in the sidebar, not just in `self.data`.
        assert!(harness.query_by_label("Sample Requests").is_some());
    }

    /// Regression test for a real click-through-drag-source bug: an earlier
    /// version wrapped the *entire* request row in `dnd_drag_source`
    /// (`Ui::dnd_drag_source`'s own drag-sense `interact()` sits on top of
    /// the label's click-sense one and swallows the click), so clicking a
    /// request never loaded it into the editor. Fixed by giving each row a
    /// separate small drag handle instead (matching the folder handle,
    /// which never had this bug). This test drives an actual click through
    /// `kittest`, not just a static render, specifically to catch that class
    /// of regression again.
    #[test]
    #[ignore]
    fn clicking_a_request_row_loads_it_into_the_editor() {
        use egui_kittest::kittest::Queryable;

        let mut data = AppData::default();
        let mut collection = Collection::new("Demo");
        let req = RequestItem::new("Get Users");
        let req_id = req.id;
        collection.requests.push(req);
        data.collections.push(collection);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();
        assert_ne!(
            harness.state().active_tab().current_request.id,
            req_id,
            "starts on the fresh, unrelated default request"
        );

        // Collections start collapsed by default (see the `default_open`
        // comment near `collections_sidebar` for why) — expand "Demo" first
        // so "Get Users" is actually rendered to click.
        harness.get_by_label("Demo").click();
        // Two steps, not one: `collections_sidebar` flattens the tree into
        // `rows` once at the top of the frame, *before* the `show_rows`
        // loop that actually processes this click and mutates
        // `expanded_nodes` — so the newly-expanded row(s) don't appear
        // until the *next* frame's flattening picks up the updated state.
        // Same class of "layout needs one more frame to settle" quirk this
        // codebase has already hit with `CollapsingHeader` animations and a
        // freshly-opened `egui::Window` (Phase 11) — not a real bug, just
        // an artifact of single-stepping a headless harness through what a
        // live, continuously-repainting app resolves in well under a frame.
        harness.step();
        harness.step();
        harness.get_by_label("Get Users").click();
        harness.step();

        assert_eq!(
            harness.state().active_tab().current_request.id,
            req_id,
            "clicking the request row should load it into the editor (in a new tab)"
        );
    }

    /// Real, reported bug: right-clicking a request/folder/collection row
    /// did nothing at all — `dnd_drop_zone`'s own response is allocated
    /// with `Sense::hover()` (confirmed against the cached egui-0.35.0
    /// source), so it can never satisfy `.secondary_clicked()`, which
    /// `.context_menu()` depends on. Fixed by upgrading the sense via
    /// `.interact(Sense::click())` before attaching the context menu.
    /// Drives a real secondary click (not just calling the underlying
    /// action directly) so this actually exercises the fix, not just the
    /// menu-building code behind it.
    #[test]
    #[ignore]
    fn right_clicking_a_request_row_opens_its_context_menu() {
        use egui_kittest::kittest::Queryable;

        let mut data = AppData::default();
        let mut collection = Collection::new("Demo");
        collection.requests.push(RequestItem::new("Get Users"));
        data.collections.push(collection);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();

        harness.get_by_label("Demo").click();
        harness.step();
        harness.step(); // see `clicking_a_request_row_loads_it_into_the_editor`'s comment

        assert!(
            harness.query_by_label("Assign hotkey…").is_none(),
            "the context menu shouldn't be open yet"
        );

        harness.get_by_label("Get Users").click_secondary();
        harness.step();

        assert!(
            harness.query_by_label("Assign hotkey…").is_some(),
            "right-clicking the row should open its context menu"
        );
    }

    /// Phase 4 self-check: the new Auth tab renders its `AuthKind` combo and
    /// the Basic-auth username/password fields.
    #[test]
    #[ignore]
    fn egui_kittest_smoke_renders_auth_tab() {
        let data = AppData::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));

        let tab = harness.state_mut().active_tab_mut();
        tab.request_tab = RequestTab::Auth;
        tab.current_request.auth.kind = AuthKind::Basic;
        tab.current_request.auth.params = vec![
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
        ];

        harness.step();
        harness.snapshot("phase4_auth_tab_basic");
    }

    /// Phase 5 self-check: the Tests script tab shows its snippet button row
    /// above the editor.
    #[test]
    #[ignore]
    fn egui_kittest_smoke_renders_script_snippet_buttons() {
        let data = AppData::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));

        harness.state_mut().active_tab_mut().request_tab = RequestTab::TestsScript;

        harness.step();
        harness.snapshot("phase5_script_snippets");
    }

    /// Phase 6 self-check: the Settings panel renders its proxy/TLS
    /// sections with the values already set (so the warning label and
    /// populated cert-path fields are both visible in one snapshot).
    #[test]
    #[ignore]
    fn egui_kittest_smoke_renders_settings_panel() {
        let data = AppData::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));

        harness.state_mut().central_view = CentralView::Settings;
        harness.state_mut().settings.proxy.enabled = true;
        harness.state_mut().settings.proxy.http_proxy = "http://127.0.0.1:8080".to_string();
        harness.state_mut().settings.tls.accept_invalid_certs = true;
        harness.state_mut().settings.tls.custom_ca_cert_path =
            Some("/etc/ssl/my-ca.pem".to_string());

        harness.step();
        harness.snapshot("phase6_settings_panel");
    }

    fn example_with(name: &str, status: u16, body: &str) -> SavedExample {
        SavedExample {
            id: Uuid::new_v4(),
            name: name.to_string(),
            timestamp: chrono::Utc::now(),
            sent_request: Some(SentRequest {
                method: "GET".to_string(),
                url: "https://example.com/widget".to_string(),
                headers: vec![],
                body: None,
            }),
            response: Some(HttpResponse {
                status,
                status_text: "OK".to_string(),
                headers: vec![],
                body: body.to_string(),
                duration_ms: 10,
                size_bytes: body.len(),
                raw_bytes: Vec::new(),
            }),
            error: None,
        }
    }

    /// Phase 7: with no `viewing_example`, the effective_* helpers just
    /// forward to the live `response`/`sent_request`/`response_error`
    /// fields — no UI/kittest needed, these are plain data lookups.
    #[test]
    fn effective_helpers_default_to_the_live_response() {
        let mut app = App::with_data(AppData::default());
        app.active_tab_mut().response = Some(HttpResponse {
            status: 200,
            status_text: "OK".to_string(),
            headers: vec![],
            body: "live".to_string(),
            duration_ms: 1,
            size_bytes: 4,
            raw_bytes: Vec::new(),
        });
        let tab = app.active_tab();
        assert_eq!(App::effective_response(tab).unwrap().body, "live");
        assert!(App::effective_sent_request(tab).is_none());
        assert!(App::effective_error(tab).is_none());
    }

    #[test]
    fn effective_helpers_resolve_to_the_viewed_example_when_set() {
        let mut app = App::with_data(AppData::default());
        app.active_tab_mut().response = Some(HttpResponse {
            status: 200,
            status_text: "OK".to_string(),
            headers: vec![],
            body: "live".to_string(),
            duration_ms: 1,
            size_bytes: 4,
            raw_bytes: Vec::new(),
        });
        let example = example_with("Saved 1", 404, "not found");
        let example_id = example.id;
        let tab = app.active_tab_mut();
        tab.current_request.saved_examples.push(example);
        tab.viewing_example = Some(example_id);

        let tab = app.active_tab();
        assert_eq!(App::effective_response(tab).unwrap().body, "not found");
        assert_eq!(App::effective_response(tab).unwrap().status, 404);
        assert_eq!(
            App::effective_sent_request(tab).unwrap().url,
            "https://example.com/widget"
        );
    }

    #[test]
    fn effective_helpers_return_none_for_a_stale_viewing_example_id() {
        // e.g. the example was deleted out from under `viewing_example` —
        // should degrade to "nothing," never panic.
        let mut app = App::with_data(AppData::default());
        app.active_tab_mut().viewing_example = Some(Uuid::new_v4());
        let tab = app.active_tab();
        assert!(App::effective_response(tab).is_none());
        assert!(App::effective_sent_request(tab).is_none());
        assert!(App::effective_error(tab).is_none());
    }

    #[test]
    fn status_or_dash_formats_present_and_absent_status() {
        assert_eq!(status_or_dash(Some(200)), "200");
        assert_eq!(status_or_dash(None), "-");
    }

    /// Phase 7 self-check: the Cookies tab lists only cookies whose domain
    /// matches the current request's host, and the examples strip shows a
    /// saved example plus the Save-as-Example button.
    #[test]
    #[ignore]
    fn egui_kittest_smoke_renders_cookies_and_examples() {
        let mut data = AppData::default();
        let mut req = RequestItem::new("Get Widget");
        req.url = "https://example.com/widget".to_string();
        req.saved_examples
            .push(example_with("First try", 500, "boom"));
        let req_id = req.id;
        let mut collection = Collection::new("Demo");
        collection.requests.push(req);
        data.collections.push(collection);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));

        harness.step();
        {
            let state = harness.state_mut();
            let req = state
                .data
                .collections
                .iter()
                .flat_map(|c| c.requests.iter())
                .find(|r| r.id == req_id)
                .unwrap()
                .clone();
            let jar = state.cookie_jar.clone();
            let tab = state.active_tab_mut();
            tab.current_request = req;
            tab.response = Some(HttpResponse {
                status: 200,
                status_text: "OK".to_string(),
                headers: vec![],
                body: "{\"ok\":true}".to_string(),
                duration_ms: 5,
                size_bytes: 12,
                raw_bytes: Vec::new(),
            });
            tab.sent_request = Some(SentRequest {
                method: "GET".to_string(),
                url: "https://example.com/widget".to_string(),
                headers: vec![],
                body: None,
            });
            jar.lock()
                .unwrap()
                .parse(
                    "session=abc123; Domain=example.com; Path=/; Max-Age=3600",
                    &"https://example.com/widget".parse().unwrap(),
                )
                .unwrap();
            tab.response_tab = ResponseTab::Cookies;
        }
        harness.step();
        harness.snapshot("phase7_cookies_and_examples");
    }

    /// Phase 7 self-check: the Diff tab renders a line-level diff between
    /// two saved examples with different bodies.
    #[test]
    #[ignore]
    fn egui_kittest_smoke_renders_diff_tab() {
        let mut data = AppData::default();
        let mut req = RequestItem::new("Get Widget");
        req.url = "https://example.com/widget".to_string();
        req.saved_examples
            .push(example_with("Before", 200, "line one\nline two\n"));
        req.saved_examples.push(example_with(
            "After",
            200,
            "line one\nline TWO\nline three\n",
        ));
        let req_id = req.id;
        let mut collection = Collection::new("Demo");
        collection.requests.push(req);
        data.collections.push(collection);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));

        harness.step();
        {
            let state = harness.state_mut();
            let req = state
                .data
                .collections
                .iter()
                .flat_map(|c| c.requests.iter())
                .find(|r| r.id == req_id)
                .unwrap()
                .clone();
            let tab = state.active_tab_mut();
            tab.current_request = req;
            tab.response_tab = ResponseTab::Diff;
        }
        harness.step();
        harness.snapshot("phase7_diff_tab");
    }

    #[test]
    fn open_tab_new_unsaved_defaults() {
        let tab = OpenTab::new_unsaved();
        assert_eq!(tab.current_request.name, "New Request");
        assert!(matches!(tab.origin, RequestOrigin::Unsaved));
        // Known quirk, carried over unchanged from before tabs existed: a
        // never-saved tab always reads as "dirty" since `autosave_last_synced`
        // starts `None` (see `fn ui`'s repaint-for-debounce comment).
        assert!(tab.is_dirty());
    }

    #[test]
    fn open_tab_from_saved_is_not_dirty_immediately() {
        let req = RequestItem::new("Get Users");
        let collection_id = Uuid::new_v4();
        let req_id = req.id;
        let tab = OpenTab::from_saved(collection_id, Vec::new(), req);
        assert_eq!(tab.current_request.id, req_id);
        assert!(matches!(&tab.origin,
            RequestOrigin::Collection { collection, request, .. }
            if *collection == collection_id && *request == req_id));
        assert!(!tab.is_dirty());
    }

    #[test]
    fn open_request_in_tab_focuses_an_already_open_tab_instead_of_duplicating() {
        let mut app = App::with_data(AppData::default());
        let collection_id = Uuid::new_v4();
        let req = RequestItem::new("Get Users");

        app.open_request_in_tab(collection_id, Vec::new(), req.clone());
        assert_eq!(
            app.tabs.len(),
            2,
            "original blank tab + the newly opened one"
        );
        let opened_idx = app.active_tab;

        app.active_tab = 0; // switch away
        app.open_request_in_tab(collection_id, Vec::new(), req);
        assert_eq!(
            app.tabs.len(),
            2,
            "re-opening the same request should focus it, not duplicate it"
        );
        assert_eq!(app.active_tab, opened_idx);
    }

    #[test]
    fn close_tab_always_leaves_at_least_one_tab() {
        let mut app = App::with_data(AppData::default());
        assert_eq!(app.tabs.len(), 1);
        app.close_tab(0);
        assert_eq!(
            app.tabs.len(),
            1,
            "closing the last tab immediately opens a fresh blank one"
        );
        assert_eq!(app.active_tab, 0);
    }

    #[test]
    fn close_tab_clamps_active_tab_to_the_new_valid_range() {
        let mut app = App::with_data(AppData::default());
        app.new_blank_tab();
        app.new_blank_tab();
        assert_eq!(app.tabs.len(), 3);
        app.active_tab = 2;
        app.close_tab(2);
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(
            app.active_tab, 1,
            "active_tab clamps down when the last tab is closed"
        );
    }

    /// `theme::status_color`'s 4-bucket boundaries — specifically the
    /// off-by-one edges between buckets, since those are exactly where a
    /// `<`/`<=` mistake would hide.
    #[test]
    fn status_color_buckets_by_the_leading_digit_with_correct_edges() {
        assert_eq!(theme::status_color(200), theme::status_color(299));
        assert_ne!(theme::status_color(299), theme::status_color(300));
        assert_eq!(theme::status_color(300), theme::status_color(399));
        assert_ne!(theme::status_color(399), theme::status_color(400));
        assert_eq!(theme::status_color(400), theme::status_color(499));
        assert_ne!(theme::status_color(499), theme::status_color(500));
        assert_eq!(theme::status_color(500), theme::status_color(599));
    }

    /// `theme::method_color` gives every method a distinct color except
    /// HEAD/OPTIONS, which deliberately share one (matching the old
    /// `method_badge_color` this replaced — moving it into `theme` and
    /// reusing it more widely shouldn't change what color any method gets).
    #[test]
    fn method_color_matches_distinct_per_method_except_head_and_options() {
        use std::collections::HashSet;
        let colors: Vec<egui::Color32> = Method::ALL
            .iter()
            .map(|m| theme::method_color(*m))
            .collect();
        let distinct: HashSet<_> = colors.iter().map(|c| c.to_array()).collect();
        // 7 methods, HEAD and OPTIONS intentionally share a color -> 6 distinct.
        assert_eq!(distinct.len(), 6);
        assert_eq!(
            theme::method_color(Method::Head),
            theme::method_color(Method::Options)
        );
    }

    /// A small batch keeps the old per-file detail (naming each imported
    /// collection) — this is the case the original multi-file import
    /// feature was designed around, and it's short enough to never overflow
    /// the sidebar.
    #[test]
    fn describe_import_batch_joins_small_batches_verbatim() {
        let results = vec![
            "Imported Postman collection \"A\"".to_string(),
            "Imported Postman collection \"B\"".to_string(),
        ];
        assert_eq!(
            describe_import_batch(results),
            "Imported Postman collection \"A\" | Imported Postman collection \"B\""
        );
    }

    /// The real bug report this fixes: importing many files at once used to
    /// join every single result verbatim, producing a message long enough
    /// to overflow the sidebar. A large all-success batch now collapses to
    /// one short line instead.
    #[test]
    fn describe_import_batch_summarizes_a_large_all_success_batch() {
        let results: Vec<String> = (0..10)
            .map(|i| format!("Imported Postman collection \"Collection {i}\""))
            .collect();
        assert_eq!(
            describe_import_batch(results),
            "Imported 10 files successfully."
        );
    }

    /// A large batch with some failures still calls out exactly which files
    /// failed and why — the actually useful information — without also
    /// spelling out every one of the successes.
    #[test]
    fn describe_import_batch_lists_only_failures_in_a_large_batch() {
        let mut results: Vec<String> = (0..8)
            .map(|i| format!("Imported Postman collection \"Collection {i}\""))
            .collect();
        results.push("/tmp/broken.json: import failed: unexpected EOF".to_string());
        results.push("/tmp/missing.json: Could not read file: not found".to_string());
        let summary = describe_import_batch(results);
        assert!(summary.starts_with("Imported 8/10 files; 2 failed:"));
        assert!(summary.contains("- /tmp/broken.json: import failed: unexpected EOF"));
        assert!(summary.contains("- /tmp/missing.json: Could not read file: not found"));
    }

    /// Real, measured bug this guards against: a brand-new `App` always
    /// starts with exactly one `Unsaved`-origin tab, whose `is_dirty()` is
    /// permanently `true` (there's nowhere to autosave it to, so it never
    /// gets a synced snapshot to compare against). `has_savable_dirty_tab`
    /// must say `false` for that case — otherwise `fn ui` keeps requesting
    /// a repaint every ~100ms for the entire lifetime of the app, which a
    /// real `ps`/`vmmap` measurement showed costs ~20-24% CPU at true idle
    /// and prevents the GPU/graphics memory footprint from ever settling
    /// back down (~170MB+ vs. ~5MB once fixed) — confirmed empirically, not
    /// just reasoned about.
    #[test]
    fn has_savable_dirty_tab_ignores_the_permanently_dirty_unsaved_tab() {
        let app = App::with_data(AppData::default());
        assert!(matches!(app.active_tab().origin, RequestOrigin::Unsaved));
        assert!(
            app.active_tab().is_dirty(),
            "an Unsaved tab is permanently dirty by design"
        );
        assert!(
            !app.has_savable_dirty_tab(),
            "but that should never count as a reason to keep repainting"
        );
    }

    /// A genuinely dirty, collection-backed tab *should* count — this is
    /// the real case the repaint-for-debounce logic exists for.
    #[test]
    fn has_savable_dirty_tab_is_true_for_a_dirty_collection_backed_tab() {
        let mut data = AppData::default();
        let mut collection = Collection::new("Demo");
        let req = RequestItem::new("Get Users");
        let collection_id = collection.id;
        collection.requests.push(req.clone());
        data.collections.push(collection);

        let mut app = App::with_data(data);
        app.open_request_in_tab(collection_id, Vec::new(), req);
        assert!(
            !app.has_savable_dirty_tab(),
            "freshly opened, not yet edited"
        );

        app.active_tab_mut().current_request.name = "Get Users (edited)".to_string();
        assert!(
            app.has_savable_dirty_tab(),
            "an edited collection-backed tab has a real debounce timer worth waiting out"
        );
    }

    /// Pure test for `jump_tab_history`'s walk/skip-stale logic, no `egui::Ui`
    /// involved: 3 tabs, a jump history visiting all 3 in order, cursor at
    /// the end — Ctrl+O should step back one at a time, Ctrl+I should step
    /// forward, and both should refuse to walk off either end of the list.
    #[test]
    fn jump_tab_history_steps_back_and_forth_and_stops_at_the_ends() {
        let mut app = App::with_data(AppData::default());
        app.new_blank_tab();
        app.new_blank_tab();
        assert_eq!(app.tabs.len(), 3);
        let ids: Vec<Uuid> = app.tabs.iter().map(|t| t.current_request.id).collect();
        app.tab_jump_history = ids.clone();
        app.tab_jump_cursor = 2;
        app.active_tab = 2;

        app.jump_tab_history(-1);
        assert_eq!(app.tab_jump_cursor, 1);
        assert_eq!(app.active_tab, 1);

        app.jump_tab_history(-1);
        assert_eq!(app.tab_jump_cursor, 0);
        assert_eq!(app.active_tab, 0);

        app.jump_tab_history(-1); // nothing further back — no-op
        assert_eq!(app.tab_jump_cursor, 0);
        assert_eq!(app.active_tab, 0);

        app.jump_tab_history(1);
        assert_eq!(app.tab_jump_cursor, 1);
        assert_eq!(app.active_tab, 1);

        app.jump_tab_history(1);
        app.jump_tab_history(1); // nothing further forward — no-op
        assert_eq!(app.tab_jump_cursor, 2);
        assert_eq!(app.active_tab, 2);
    }

    /// A jump-history entry whose tab has since been closed is skipped over
    /// rather than getting the cursor stuck on a dead id — the middle tab
    /// closes, then Ctrl+O from the last entry should land on the first
    /// entry, not on the now-nonexistent middle one.
    #[test]
    fn jump_tab_history_skips_over_an_entry_whose_tab_was_closed() {
        let mut app = App::with_data(AppData::default());
        app.new_blank_tab();
        app.new_blank_tab();
        let ids: Vec<Uuid> = app.tabs.iter().map(|t| t.current_request.id).collect();
        app.tab_jump_history = ids.clone();
        app.tab_jump_cursor = 2;
        app.active_tab = 2;

        app.close_tab(1); // closes the middle tab (index 1, id ids[1])

        app.jump_tab_history(-1);
        assert_eq!(
            app.active_tab,
            app.tabs
                .iter()
                .position(|t| t.current_request.id == ids[0])
                .unwrap(),
            "the stale middle entry should be skipped, landing on the first tab"
        );
    }

    /// `record_tab_jump_if_changed`'s own predicate, independent of `fn
    /// ui`'s call site: recording is a no-op when the active tab already
    /// matches what's at the cursor (the "Ctrl+O/I just moved here"
    /// case), and truncates any forward history before appending when it
    /// doesn't (matching a browser's back/forward history semantics).
    #[test]
    fn record_tab_jump_if_changed_is_a_no_op_when_already_recorded_but_truncates_and_appends_otherwise()
     {
        let mut app = App::with_data(AppData::default());
        app.new_blank_tab();
        let id0 = app.tabs[0].current_request.id;
        let id1 = app.tabs[1].current_request.id;

        // First-ever call: self-initializes from empty history.
        app.active_tab = 0;
        app.record_tab_jump_if_changed();
        assert_eq!(app.tab_jump_history, vec![id0]);
        assert_eq!(app.tab_jump_cursor, 0);

        // Already recorded at the cursor: no-op.
        app.record_tab_jump_if_changed();
        assert_eq!(app.tab_jump_history, vec![id0]);

        // A genuinely new active tab: appended, cursor advances.
        app.active_tab = 1;
        app.record_tab_jump_if_changed();
        assert_eq!(app.tab_jump_history, vec![id0, id1]);
        assert_eq!(app.tab_jump_cursor, 1);

        // Simulate having gone back via Ctrl+O, then navigating somewhere
        // new by ordinary means — the discarded "forward" entry (id1)
        // should not survive.
        app.tab_jump_cursor = 0;
        app.active_tab = 0;
        app.new_blank_tab();
        let id2 = app.tabs[2].current_request.id;
        app.active_tab = 2;
        app.record_tab_jump_if_changed();
        assert_eq!(app.tab_jump_history, vec![id0, id2]);
        assert_eq!(app.tab_jump_cursor, 1);
    }

    /// Anyone who already relied on the old Alt+1..9-opens-"Saved Requests"
    /// shortcut should see the exact same requests still bound to the same
    /// keys once this feature replaces it — `App::with_data` (via
    /// `with_data_settings_and_jar`) runs this migration on construction.
    #[test]
    fn migrate_number_shortcuts_to_hotkey_bindings_preserves_alt_1_through_9() {
        let mut data = AppData::default();
        let mut collection = Collection::new("Saved Requests");
        let ids: Vec<Uuid> = (0..12)
            .map(|i| {
                let req = RequestItem::new(format!("Req {i}"));
                let id = req.id;
                collection.requests.push(req);
                id
            })
            .collect();
        data.collections.push(collection);

        let app = App::with_data(data);

        for (i, id) in ids.iter().enumerate().take(9) {
            let key = char::from_digit(i as u32 + 1, 10).unwrap();
            assert_eq!(
                app.data.hotkey_bindings.get(&key),
                Some(id),
                "request {i} should keep the Alt+{key} binding it had before"
            );
        }
        // Only the first 9 (Alt+1..9 only ever covered 9 slots) — the 10th
        // and beyond get no automatic binding.
        assert_eq!(app.data.hotkey_bindings.len(), 9);
    }

    #[test]
    fn migration_is_a_no_op_once_any_hotkey_already_exists() {
        let mut data = AppData::default();
        let mut collection = Collection::new("Saved Requests");
        collection.requests.push(RequestItem::new("Req"));
        data.collections.push(collection);
        // Simulates a user who already assigned one hotkey (to some
        // unrelated request) before this exact "Saved Requests" collection
        // existed in their data — the migration must not clobber it.
        let unrelated_id = Uuid::new_v4();
        data.hotkey_bindings.insert('z', unrelated_id);

        let app = App::with_data(data);

        assert_eq!(app.data.hotkey_bindings.len(), 1);
        assert_eq!(app.data.hotkey_bindings.get(&'z'), Some(&unrelated_id));
    }

    /// The concrete regression guard for the concurrency bug this phase
    /// fixes: previously there was only one global `in_flight_id`, so a
    /// second tab's send would silently drop the first tab's reply once it
    /// arrived (`id != self.in_flight_id` was true for it). Now each tab
    /// tracks its own `in_flight_id`, and `poll_responses` routes by
    /// scanning for the matching tab — this drives that real channel and
    /// asserts tab A's reply lands on tab A even though tab B is active
    /// when it arrives.
    #[test]
    fn poll_responses_routes_each_reply_to_the_tab_that_sent_it() {
        let mut app = App::with_data(AppData::default());
        app.new_blank_tab();
        assert_eq!(app.tabs.len(), 2);

        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        app.tabs[0].in_flight_id = id_a;
        app.tabs[0].is_loading = true;
        app.tabs[1].in_flight_id = id_b;
        app.tabs[1].is_loading = true;
        app.active_tab = 1; // tab B is active when tab A's reply arrives

        let response_a = HttpResponse {
            status: 200,
            status_text: "OK".to_string(),
            headers: vec![],
            body: "from A".to_string(),
            duration_ms: 1,
            size_bytes: 6,
            raw_bytes: Vec::new(),
        };
        let sent_a = SentRequest {
            method: "GET".to_string(),
            url: "https://a.example".to_string(),
            headers: vec![],
            body: None,
        };
        app.tx
            .send((
                id_a,
                RequestOutcome::Success {
                    request: sent_a,
                    response: response_a,
                },
            ))
            .unwrap();

        let ctx = egui::Context::default();
        app.poll_responses(&ctx);

        assert_eq!(
            app.tabs[0].response.as_ref().unwrap().body,
            "from A",
            "tab A's reply should land on tab A, not be dropped or misapplied to the active tab B"
        );
        assert!(!app.tabs[0].is_loading);
        assert!(
            app.tabs[1].response.is_none(),
            "tab B never got a reply, so its response stays empty"
        );
    }

    /// Every completed send — success or failure — should show up in the
    /// Console, newest first, regardless of which tab it came from.
    #[test]
    fn poll_responses_records_a_console_entry_for_every_send() {
        let mut app = App::with_data(AppData::default());
        let id = app.tabs[0].in_flight_id;
        app.tabs[0].is_loading = true;
        app.tabs[0].current_request.url = "https://example.com/ok".to_string();

        app.tx
            .send((
                id,
                RequestOutcome::Success {
                    request: SentRequest {
                        method: "GET".to_string(),
                        url: "https://example.com/ok".to_string(),
                        headers: vec![],
                        body: None,
                    },
                    response: HttpResponse {
                        status: 200,
                        status_text: "OK".to_string(),
                        headers: vec![],
                        body: "{}".to_string(),
                        duration_ms: 9,
                        size_bytes: 2,
                        raw_bytes: Vec::new(),
                    },
                },
            ))
            .unwrap();

        let ctx = egui::Context::default();
        app.poll_responses(&ctx);

        assert_eq!(app.console.len(), 1);
        assert_eq!(app.console[0].url, "https://example.com/ok");
        assert_eq!(app.console[0].status, Some(200));

        // A second send (a failure this time) is inserted at the front —
        // newest first, matching `data.history`'s own convention.
        let id2 = app.tabs[0].in_flight_id;
        app.tabs[0].is_loading = true;
        app.tabs[0].current_request.url = "https://example.com/bad".to_string();
        app.tx
            .send((
                id2,
                RequestOutcome::Error {
                    request: None,
                    message: "boom".to_string(),
                    duration_ms: None,
                },
            ))
            .unwrap();
        app.poll_responses(&ctx);

        assert_eq!(app.console.len(), 2);
        assert_eq!(app.console[0].url, "https://example.com/bad");
        assert_eq!(app.console[0].error.as_deref(), Some("boom"));
        assert_eq!(app.console[1].url, "https://example.com/ok");
    }

    /// Phase 8 self-check: clicking a sidebar request opens it in a new tab
    /// (rather than replacing whatever was open), and closing that tab
    /// leaves the original blank tab behind.
    #[test]
    #[ignore]
    fn clicking_a_sidebar_request_opens_a_new_tab() {
        use egui_kittest::kittest::Queryable;

        let mut data = AppData::default();
        let mut collection = Collection::new("Demo");
        collection.requests.push(RequestItem::new("Get Users"));
        data.collections.push(collection);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();
        assert_eq!(harness.state().tabs.len(), 1);

        // Collections start collapsed by default — expand "Demo" first.
        harness.get_by_label("Demo").click();
        // Two steps, not one: `collections_sidebar` flattens the tree into
        // `rows` once at the top of the frame, *before* the `show_rows`
        // loop that actually processes this click and mutates
        // `expanded_nodes` — so the newly-expanded row(s) don't appear
        // until the *next* frame's flattening picks up the updated state.
        // Same class of "layout needs one more frame to settle" quirk this
        // codebase has already hit with `CollapsingHeader` animations and a
        // freshly-opened `egui::Window` (Phase 11) — not a real bug, just
        // an artifact of single-stepping a headless harness through what a
        // live, continuously-repainting app resolves in well under a frame.
        harness.step();
        harness.step();
        harness.get_by_label("Get Users").click();
        harness.step();
        assert_eq!(
            harness.state().tabs.len(),
            2,
            "clicking a request opens it in a new tab"
        );

        let active = harness.state().active_tab;
        harness.state_mut().close_tab(active);
        harness.step();
        assert_eq!(
            harness.state().tabs.len(),
            1,
            "closing the only extra tab leaves just the original"
        );
    }

    /// Regression test for a real click-through-drag-source bug reported by
    /// the user against the tab bar (the same class of bug Phase 3 already
    /// fixed once for collection-tree rows): an earlier version of
    /// `tab_bar` wrapped the *entire* chip in `dnd_drag_source`, whose own
    /// drag-sense `interact()` sat on top of the label's click-sense one and
    /// swallowed the click — so clicking a background tab never switched to
    /// it. Fixed by giving each chip a separate small drag handle, matching
    /// every other drag-and-drop row in this app. This test drives a real
    /// click through `kittest`, not just a static render.
    #[test]
    #[ignore]
    fn clicking_a_background_tab_switches_to_it() {
        use egui_kittest::kittest::Queryable;

        let mut data = AppData::default();
        let mut collection = Collection::new("Demo");
        collection.requests.push(RequestItem::new("Get Users"));
        data.collections.push(collection);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();

        // Collections start collapsed by default — expand "Demo" first.
        harness.get_by_label("Demo").click();
        // Two steps, not one: `collections_sidebar` flattens the tree into
        // `rows` once at the top of the frame, *before* the `show_rows`
        // loop that actually processes this click and mutates
        // `expanded_nodes` — so the newly-expanded row(s) don't appear
        // until the *next* frame's flattening picks up the updated state.
        // Same class of "layout needs one more frame to settle" quirk this
        // codebase has already hit with `CollapsingHeader` animations and a
        // freshly-opened `egui::Window` (Phase 11) — not a real bug, just
        // an artifact of single-stepping a headless harness through what a
        // live, continuously-repainting app resolves in well under a frame.
        harness.step();
        harness.step();

        // Open "Get Users" in a second tab — it becomes active, leaving the
        // original "New Request" tab as the inactive, background one.
        harness.get_by_label("Get Users").click();
        harness.step();
        assert_eq!(
            harness.state().active_tab,
            1,
            "the newly-opened tab should be active"
        );

        // Click the background "New Request" tab's own label in the tab bar.
        harness.get_by_label("New Request").click();
        harness.step();

        assert_eq!(
            harness.state().active_tab,
            0,
            "clicking a background tab's label should switch to it"
        );
    }

    /// Phase 8 self-check: the Postman-styled tab bar renders multiple open
    /// tabs — method-colored badges, one active (highlighted), and the
    /// never-saved "New Request" tab's unsaved-dot indicator (a
    /// collection-backed tab's dot only shows for a throttled instant before
    /// autosave clears it, so it can't be captured in a single-frame
    /// snapshot the way the always-dirty Unsaved tab's can).
    #[test]
    #[ignore]
    fn egui_kittest_smoke_renders_tab_bar() {
        use egui_kittest::kittest::Queryable;

        let mut data = AppData::default();
        let mut collection = Collection::new("Demo");
        collection.requests.push(RequestItem::new("Get Users"));
        collection.requests.push(RequestItem::new("Create Order"));
        data.collections.push(collection);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();

        // Collections start collapsed by default — expand "Demo" first.
        harness.get_by_label("Demo").click();
        // Two steps, not one: `collections_sidebar` flattens the tree into
        // `rows` once at the top of the frame, *before* the `show_rows`
        // loop that actually processes this click and mutates
        // `expanded_nodes` — so the newly-expanded row(s) don't appear
        // until the *next* frame's flattening picks up the updated state.
        // Same class of "layout needs one more frame to settle" quirk this
        // codebase has already hit with `CollapsingHeader` animations and a
        // freshly-opened `egui::Window` (Phase 11) — not a real bug, just
        // an artifact of single-stepping a headless harness through what a
        // live, continuously-repainting app resolves in well under a frame.
        harness.step();
        harness.step();
        harness.get_by_label("Get Users").click();
        harness.step();
        harness.get_by_label("Create Order").click();
        harness.step();
        harness.snapshot("phase8_tab_bar");
    }

    /// Option+H/L (AeroSpace's own "focus left/right" convention) steps
    /// between open tabs, wrapping at both ends — driven by a real injected
    /// key event, not just calling the underlying field mutation directly.
    #[test]
    #[ignore]
    fn option_h_and_l_step_between_open_tabs() {
        let mut app = App::with_data(AppData::default());
        app.new_blank_tab();
        app.new_blank_tab();
        assert_eq!(app.tabs.len(), 3);
        app.active_tab = 1;

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| app);
        harness.step();

        harness.key_press_modifiers(egui::Modifiers::ALT, egui::Key::L);
        harness.step();
        assert_eq!(
            harness.state().active_tab,
            2,
            "Option+L moves to the next tab"
        );

        harness.key_press_modifiers(egui::Modifiers::ALT, egui::Key::L);
        harness.step();
        assert_eq!(
            harness.state().active_tab,
            0,
            "Option+L wraps around past the last tab"
        );

        harness.key_press_modifiers(egui::Modifiers::ALT, egui::Key::H);
        harness.step();
        assert_eq!(
            harness.state().active_tab,
            2,
            "Option+H wraps back around past the first tab"
        );

        harness.key_press_modifiers(egui::Modifiers::ALT, egui::Key::H);
        harness.step();
        assert_eq!(
            harness.state().active_tab,
            1,
            "Option+H moves to the previous tab"
        );
    }

    /// Emacs-style tree navigation: Ctrl+N/P move the cursor down/up through
    /// the flattened rows, Ctrl+F expands the row it's on, and Enter opens a
    /// focused request into a new tab.
    #[test]
    #[ignore]
    fn emacs_style_keys_navigate_expand_and_open_in_the_tree() {
        let mut data = AppData::default();
        let mut collection = Collection::new("Demo");
        collection.requests.push(RequestItem::new("Get Users"));
        let mut auth = Folder::new("Auth");
        auth.requests.push(RequestItem::new("Login"));
        collection.folders.push(auth);
        data.collections.push(collection);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();

        // Nothing focused yet: Ctrl+N puts the cursor on the only visible
        // row, the collapsed "Demo" collection.
        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::N);
        harness.step();
        assert_eq!(harness.state().tree_focused_index, Some(0));

        // Ctrl+F expands it — same two-step lag every expand/collapse
        // click already has (`flatten_visible_rows` is recomputed once at
        // the top of the next frame, not the one the click lands in).
        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::F);
        harness.step();
        harness.step();

        // Now: Demo(0), Get Users(1), Auth(2). Ctrl+N onto "Get Users" (a
        // request) previews it into a single reusable tab — VS Code-style,
        // not a permanent tab yet.
        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::N);
        harness.step();
        assert_eq!(harness.state().tabs.len(), 2, "opened a preview tab");
        let preview_idx = harness.state().active_tab;
        assert!(harness.state().tabs[preview_idx].is_preview);
        assert_eq!(
            harness.state().tabs[preview_idx].current_request.name,
            "Get Users"
        );

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::N);
        harness.step();
        assert_eq!(harness.state().tree_focused_index, Some(2));
        // "Auth" isn't a request — no new preview, the "Get Users" one
        // from a moment ago is left exactly as it was.
        assert_eq!(harness.state().tabs.len(), 2);

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::F);
        harness.step();
        harness.step();

        // Now: Demo(0), Get Users(1), Auth(2), Login(3). Stepping onto
        // "Login" *reuses* the same preview tab slot instead of opening a
        // third one.
        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::N);
        harness.step();
        assert_eq!(harness.state().tree_focused_index, Some(3));
        assert_eq!(
            harness.state().tabs.len(),
            2,
            "the preview tab was reused for Login, not a new one opened"
        );
        assert!(harness.state().tabs[harness.state().active_tab].is_preview);
        assert_eq!(
            harness.state().tabs[harness.state().active_tab]
                .current_request
                .name,
            "Login"
        );

        // Enter pins it — same tab, no longer a preview.
        harness.key_press(egui::Key::Enter);
        harness.step();

        assert_eq!(harness.state().tabs.len(), 2, "still the same 2 tabs");
        assert_eq!(
            harness.state().tabs[harness.state().active_tab]
                .current_request
                .name,
            "Login"
        );
        assert!(
            !harness.state().tabs[harness.state().active_tab].is_preview,
            "Enter pins the tab — no longer a preview"
        );

        // Ctrl+P moves back up.
        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::P);
        harness.step();
        assert_eq!(harness.state().tree_focused_index, Some(2));
    }

    /// The same Ctrl+N/P/Enter convention works on the History sidebar tab —
    /// scoped there for free since only one sidebar tab renders at a time
    /// (no separate "focused panel" concept needed).
    #[test]
    #[ignore]
    fn emacs_style_keys_navigate_and_open_in_history() {
        let mut data = AppData::default();
        for url in ["https://example.com/a", "https://example.com/b"] {
            let mut request = RequestItem::new(url);
            request.url = url.to_string();
            data.history.push(HistoryEntry {
                id: Uuid::new_v4(),
                timestamp: chrono::Utc::now(),
                method: Method::Get,
                url: url.to_string(),
                status: Some(200),
                request,
                sent_request: None,
                response: None,
                error: None,
                duration_ms: Some(10),
                test_results: vec![],
            });
        }
        let mut app = App::with_data(data);
        app.sidebar_tab = SidebarTab::History;
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| app);
        harness.step();

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::N);
        harness.step();
        assert_eq!(harness.state().history_focused_index, Some(0));
        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::N);
        harness.step();
        assert_eq!(harness.state().history_focused_index, Some(1));

        harness.key_press(egui::Key::Enter);
        harness.step();

        assert_eq!(harness.state().tabs.len(), 2, "Enter opened a new tab");
        assert_eq!(
            harness.state().tabs[harness.state().active_tab]
                .current_request
                .url,
            "https://example.com/b"
        );
    }

    /// Same convention on the Environments sidebar tab — Enter switches the
    /// active environment instead of opening a tab.
    #[test]
    #[ignore]
    fn emacs_style_keys_navigate_and_switch_environment() {
        let mut data = AppData::default();
        data.environments.push(Environment::new("Dev"));
        data.environments.push(Environment::new("Staging"));
        let staging_id = data.environments[1].id;
        let mut app = App::with_data(data);
        app.sidebar_tab = SidebarTab::Environments;
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| app);
        harness.step();

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::N);
        harness.step();
        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::N);
        harness.step();
        assert_eq!(harness.state().environments_focused_index, Some(1));

        harness.key_press(egui::Key::Enter);
        harness.step();

        assert_eq!(harness.state().active_environment, Some(staging_id));
    }

    /// Ctrl+O/Ctrl+I (Vim's own jumplist convention) walk back and forth
    /// across tabs actually visited, driven by real injected key events and
    /// real tab-bar clicks — not just calling `jump_tab_history` directly —
    /// so this also exercises `record_tab_jump_if_changed`'s end-of-frame
    /// wiring in `fn ui`.
    #[test]
    #[ignore]
    fn ctrl_o_and_ctrl_i_step_through_the_tab_visit_history() {
        use egui_kittest::kittest::Queryable;

        let mut data = AppData::default();
        let mut collection = Collection::new("Demo");
        collection.requests.push(RequestItem::new("Req A"));
        collection.requests.push(RequestItem::new("Req B"));
        collection.requests.push(RequestItem::new("Req C"));
        data.collections.push(collection);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step(); // records the initial blank tab as the first jump entry

        // Collections start collapsed by default — expand "Demo" first (see
        // `clicking_a_background_tab_switches_to_it`'s comment for why this
        // needs two steps, not one).
        harness.get_by_label("Demo").click();
        harness.step();
        harness.step();

        // Open each request in turn, via real sidebar clicks (not by poking
        // `active_tab` directly) — each resulting `active_tab` change gets
        // picked up by `record_tab_jump_if_changed` next frame.
        harness.get_by_label("Req A").click();
        harness.step();
        assert_eq!(harness.state().active_tab, 1);
        harness.get_by_label("Req B").click();
        harness.step();
        assert_eq!(harness.state().active_tab, 2);
        harness.get_by_label("Req C").click();
        harness.step();
        assert_eq!(harness.state().active_tab, 3);

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::O);
        harness.step();
        assert_eq!(
            harness.state().active_tab,
            2,
            "Ctrl+O steps back to Req B's tab"
        );

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::O);
        harness.step();
        assert_eq!(
            harness.state().active_tab,
            1,
            "Ctrl+O steps back to Req A's tab"
        );

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::I);
        harness.step();
        assert_eq!(
            harness.state().active_tab,
            2,
            "Ctrl+I steps forward again to Req B's tab"
        );
    }

    /// The sidebar auto-hides once the window narrows past
    /// `SIDEBAR_AUTO_HIDE_WIDTH`, and auto-restores once it's widened back
    /// out — driven by real `Harness::set_size` calls, not by poking
    /// `sidebar_visible` directly, so this exercises the actual width check
    /// in `fn ui`.
    #[test]
    #[ignore]
    fn sidebar_auto_hides_on_narrow_width_and_restores_when_widened_back() {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(AppData::default()));
        harness.step();
        assert!(
            harness.state().sidebar_visible,
            "starts visible at a normal width"
        );

        harness.set_size(egui::vec2(500.0, 750.0));
        harness.step();
        assert!(
            !harness.state().sidebar_visible,
            "auto-hides once the window narrows past the threshold"
        );
        assert!(harness.state().sidebar_auto_hidden);

        harness.set_size(egui::vec2(1100.0, 750.0));
        harness.step();
        assert!(
            harness.state().sidebar_visible,
            "auto-restores once the window is widened back out"
        );
        assert!(!harness.state().sidebar_auto_hidden);
    }

    #[test]
    fn parse_runner_data_file_reads_csv_rows_keyed_by_header() {
        let dir = std::env::temp_dir().join(format!("rustgirl_test_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.csv");
        std::fs::write(&path, "username,id\nalice,1\nbob,2\n").unwrap();

        let rows = App::parse_runner_data_file(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            vec![
                KeyValue {
                    key: "username".to_string(),
                    value: "alice".to_string(),
                    enabled: true
                },
                KeyValue {
                    key: "id".to_string(),
                    value: "1".to_string(),
                    enabled: true
                },
            ]
        );
        assert_eq!(rows[1][0].value, "bob");
    }

    #[test]
    fn parse_runner_data_file_reads_a_json_array_of_flat_objects() {
        let dir = std::env::temp_dir().join(format!("rustgirl_test_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.json");
        std::fs::write(
            &path,
            r#"[{"username": "alice", "id": 1}, {"username": "bob", "id": 2}]"#,
        )
        .unwrap();

        let rows = App::parse_runner_data_file(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(rows.len(), 2);
        let row0: std::collections::HashMap<_, _> = rows[0]
            .iter()
            .map(|kv| (kv.key.clone(), kv.value.clone()))
            .collect();
        assert_eq!(row0["username"], "alice");
        // Non-string JSON values (a number here) are stringified, not rejected.
        assert_eq!(row0["id"], "1");
    }

    #[test]
    fn start_run_is_a_no_op_without_a_configured_target() {
        let mut app = App::with_data(AppData::default());
        app.start_run();
        assert!(app.runner.active_run_id.is_none());
        assert_eq!(app.runner.total_requests, 0);
    }

    #[test]
    fn start_run_is_a_no_op_while_a_run_is_already_active() {
        let mut app = App::with_data(AppData::default());
        let mut collection = Collection::new("Demo");
        collection.requests.push(RequestItem::new("Get Users"));
        app.runner.collection = Some(collection.id);
        app.data.collections.push(collection);
        app.runner.active_run_id = Some(Uuid::new_v4()); // pretend a run is already in flight

        app.start_run();

        // Should have bailed out before touching anything else.
        assert_eq!(app.runner.total_requests, 0);
        assert!(app.runner.results.is_empty());
    }

    /// Drives the Runner's event-routing logic directly (no real HTTP, no
    /// background thread) — mirrors `poll_responses_routes_each_reply_to_the_tab_that_sent_it`'s
    /// approach in Phase 8: send synthetic events through the real channel,
    /// including one from a stale/superseded run, and confirm only the
    /// current run's events are applied.
    #[test]
    fn poll_runner_drops_stale_events_and_accumulates_current_ones() {
        let mut app = App::with_data(AppData::default());
        let run_id = Uuid::new_v4();
        app.runner.active_run_id = Some(run_id);
        app.runner.total_requests = 2;

        let stale_result = RunnerResult {
            iteration: 0,
            method: Method::Get,
            name: "stale".to_string(),
            status: Some(200),
            duration_ms: Some(1),
            test_results: vec![],
            error: None,
        };
        app.runner_tx
            .send(RunnerEvent::RequestFinished {
                run_id: Uuid::new_v4(),
                result: stale_result,
            })
            .unwrap();

        let result_a = RunnerResult {
            iteration: 0,
            method: Method::Get,
            name: "req a".to_string(),
            status: Some(200),
            duration_ms: Some(5),
            test_results: vec![TestResult {
                name: "ok".to_string(),
                passed: true,
                error: None,
            }],
            error: None,
        };
        app.runner_tx
            .send(RunnerEvent::RequestFinished {
                run_id,
                result: result_a,
            })
            .unwrap();
        let result_b = RunnerResult {
            iteration: 1,
            method: Method::Get,
            name: "req b".to_string(),
            status: None,
            duration_ms: None,
            test_results: vec![],
            error: Some("boom".to_string()),
        };
        app.runner_tx
            .send(RunnerEvent::RequestFinished {
                run_id,
                result: result_b,
            })
            .unwrap();
        app.runner_tx
            .send(RunnerEvent::RunFinished {
                run_id,
                cancelled: false,
            })
            .unwrap();

        let ctx = egui::Context::default();
        app.poll_runner(&ctx);

        assert_eq!(
            app.runner.results.len(),
            2,
            "the stale event should have been dropped"
        );
        assert_eq!(app.runner.completed, 2);
        assert!(
            app.runner.active_run_id.is_none(),
            "RunFinished should clear the active run"
        );
        assert!(!app.runner.last_run_cancelled);
        assert_eq!(app.runner.results[0].name, "req a");
        assert_eq!(app.runner.results[1].name, "req b");
    }

    /// Phase 9 self-check: the Runner panel with a target set, a finished
    /// run's results listed, and the summary footer visible.
    #[test]
    #[ignore]
    fn egui_kittest_smoke_renders_runner_panel() {
        let data = AppData::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();

        {
            let state = harness.state_mut();
            state.runner.collection = Some(Uuid::new_v4());
            state.runner.target_label = "Demo".to_string();
            state.runner.results = vec![
                RunnerResult {
                    iteration: 0,
                    method: Method::Get,
                    name: "Get Users".to_string(),
                    status: Some(200),
                    duration_ms: Some(12),
                    test_results: vec![TestResult {
                        name: "status is 200".to_string(),
                        passed: true,
                        error: None,
                    }],
                    error: None,
                },
                RunnerResult {
                    iteration: 0,
                    method: Method::Post,
                    name: "Create Order".to_string(),
                    status: Some(500),
                    duration_ms: Some(34),
                    test_results: vec![TestResult {
                        name: "status is 200".to_string(),
                        passed: false,
                        error: Some("expected 200, got 500".to_string()),
                    }],
                    error: None,
                },
            ];
            state.central_view = CentralView::Runner;
        }
        harness.step();
        harness.snapshot("phase9_runner_panel");
    }

    /// Phase 15 self-check: the Console panel lists seeded entries
    /// (success with script log + passing test, and a failure), newest
    /// first in storage but rendered oldest-first (top-to-bottom, like a
    /// real terminal log).
    #[test]
    #[ignore]
    fn egui_kittest_smoke_renders_console_panel() {
        let data = AppData::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();

        {
            let state = harness.state_mut();
            state.console = vec![
                ConsoleEntry {
                    timestamp: chrono::Utc::now(),
                    method: Method::Post,
                    url: "https://example.com/orders".to_string(),
                    status: None,
                    duration_ms: None,
                    error: Some("connection refused".to_string()),
                    script_log: vec![],
                    test_results: vec![],
                },
                ConsoleEntry {
                    timestamp: chrono::Utc::now(),
                    method: Method::Get,
                    url: "https://example.com/users".to_string(),
                    status: Some(200),
                    duration_ms: Some(12),
                    error: None,
                    script_log: vec!["fetched data".to_string()],
                    test_results: vec![TestResult {
                        name: "status is 200".to_string(),
                        passed: true,
                        error: None,
                    }],
                },
            ];
            state.console_open = true;
        }
        harness.step();
        harness.snapshot("phase15_console_panel");
    }

    /// Phase 17 real Postman-fidelity fix: Console used to be a
    /// `CentralView` that replaced the whole request/response area — real
    /// Postman's own Console is a collapsible drawer along the bottom of
    /// the window that coexists with it. Confirms both the console's own
    /// content and the request editor's Send button are visible in the same
    /// frame, not one replacing the other.
    #[test]
    #[ignore]
    fn console_drawer_coexists_with_the_request_editor_not_replacing_it() {
        use egui_kittest::kittest::Queryable;

        let data = AppData::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();

        harness.state_mut().console_open = true;
        harness.step();

        assert!(
            harness.query_by_label("Send").is_some(),
            "the request editor should still be visible with the console open"
        );
        // No sends happened in this test, so the drawer shows its
        // empty-state message rather than "Clear console" (only present
        // once there's an entry to clear) — either way proves the drawer
        // itself rendered alongside the request editor, which is this
        // test's actual point.
        assert!(
            harness
                .query_by_label("Nothing sent yet this session.")
                .is_some(),
            "the console drawer itself should be visible"
        );
    }

    /// Manual/CI-network verification of the actual threading design (the
    /// riskiest part of this phase): `start_run`'s background thread does a
    /// real HTTP round trip via `handle.block_on(...)` on a plain
    /// `std::thread`, never nested inside the tokio runtime's own worker
    /// threads. If that were wrong (e.g. `self.rt.spawn` had been used
    /// instead), a script calling `pm.sendRequest` would panic with "Cannot
    /// start a runtime from within a runtime" — this test would hang/panic
    /// instead of completing. `#[ignore]`d and run manually, same
    /// convention as `scripting::tests::pm_send_request_hits_a_real_server`.
    #[test]
    #[ignore]
    fn start_run_completes_a_real_request_without_panicking() {
        let mut data = AppData::default();
        let mut collection = Collection::new("Demo");
        let mut req = RequestItem::new("Get example.com");
        req.url = "https://example.com/".to_string();
        collection.requests.push(req);
        let collection_id = collection.id;
        data.collections.push(collection);

        let mut app = App::with_data(data);
        app.runner.collection = Some(collection_id);
        app.start_run();

        let ctx = egui::Context::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.runner.active_run_id.is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
            app.poll_runner(&ctx);
        }

        assert!(
            app.runner.active_run_id.is_none(),
            "run should have finished within the deadline"
        );
        assert_eq!(app.runner.results.len(), 1);
        assert_eq!(app.runner.results[0].status, Some(200));
    }

    /// Phase 10 self-check: the Code tab renders the target picker and a
    /// generated curl snippet (with Basic auth embedded) for a JSON-body
    /// POST request.
    #[test]
    #[ignore]
    fn egui_kittest_smoke_renders_code_tab() {
        let data = AppData::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));

        {
            let tab = harness.state_mut().active_tab_mut();
            tab.request_tab = RequestTab::Code;
            tab.current_request.method = Method::Post;
            tab.current_request.url = "https://api.example.com/{{path}}".to_string();
            tab.current_request.headers.push(KeyValue {
                key: "Accept".to_string(),
                value: "application/json".to_string(),
                enabled: true,
            });
            tab.current_request.body = model::RequestBody {
                mode: BodyMode::Json,
                raw: "{\"id\": {{userId}}}".to_string(),
                ..Default::default()
            };
            tab.current_request.auth = AuthConfig {
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
        }
        harness.step();
        harness.snapshot("phase10_code_tab");
    }

    #[test]
    fn palette_actions_include_environment_switches_and_theme_options() {
        let mut data = AppData::default();
        data.environments.push(Environment::new("Staging"));
        let app = App::with_data(data);
        let labels: Vec<String> = app
            .palette_actions()
            .into_iter()
            .map(|(label, _)| label)
            .collect();

        assert!(labels.contains(&"New Tab".to_string()));
        assert!(labels.contains(&"Theme: Dark".to_string()));
        assert!(labels.contains(&"Environment: Staging".to_string()));
        assert!(labels.contains(&"Environment: No Environment".to_string()));
    }

    #[test]
    fn palette_action_labels_filter_by_substring_like_request_matches_query() {
        let app = App::with_data(AppData::default());
        let actions = app.palette_actions();
        let query = "theme";
        let matching: Vec<&str> = actions
            .iter()
            .filter(|(label, _)| label.to_lowercase().contains(query))
            .map(|(label, _)| label.as_str())
            .collect();

        assert_eq!(matching.len(), 3, "Theme: Light/Dark/System");
        assert!(matching.iter().all(|label| label.starts_with("Theme:")));
    }

    /// The concrete fix for "fuzzy chưa ngon như fzf": a non-contiguous
    /// subsequence like `"crq"` should match `"Create Request"` (every
    /// character appears in order, just not adjacently) — something the
    /// old plain `.contains()` substring check could never do — and a
    /// tighter/earlier match should score higher than a looser one.
    #[test]
    fn fuzzy_score_matches_non_contiguous_subsequences_and_ranks_tighter_matches_higher() {
        let mut matcher = nucleo_matcher::Matcher::default();

        let tight = fuzzy_score(&mut matcher, "Create Request", "crq");
        assert!(
            tight.is_some(),
            "\"crq\" should subsequence-match \"Create Request\""
        );

        // A plain substring check would never match this at all — "xyz"
        // doesn't literally appear anywhere in "Create Request".
        assert!(fuzzy_score(&mut matcher, "Create Request", "xyz").is_none());

        // A name that starts with the query should score at least as well
        // as one where the same two letters are scattered further apart.
        let prefix_match = fuzzy_score(&mut matcher, "Create Request", "cr");
        let scattered_match = fuzzy_score(&mut matcher, "Collection Runner", "cr");
        assert!(
            prefix_match.unwrap() >= scattered_match.unwrap(),
            "a match starting right at the beginning should score at least \
             as well as one where the same letters are spread further apart"
        );
    }

    #[test]
    fn perform_palette_action_applies_the_expected_field_change() {
        let mut app = App::with_data(AppData::default());
        assert_eq!(app.tabs.len(), 1);

        app.perform_palette_action(PaletteAction::NewTab);
        assert_eq!(app.tabs.len(), 2, "NewTab should open a second tab");

        app.perform_palette_action(PaletteAction::SetTheme(ThemeMode::Dark));
        assert_eq!(app.settings.theme, ThemeMode::Dark);

        app.perform_palette_action(PaletteAction::SwitchSidebarTab(SidebarTab::History));
        assert!(app.sidebar_tab == SidebarTab::History);

        assert!(
            app.split_direction == SplitDirection::Vertical,
            "starts vertical"
        );
        app.perform_palette_action(PaletteAction::ToggleSplitDirection);
        assert!(app.split_direction == SplitDirection::Horizontal);
    }

    /// Phase 11 self-check: the Settings panel's new Theme section renders
    /// alongside the existing Proxy/TLS sections.
    #[test]
    #[ignore]
    fn egui_kittest_smoke_renders_theme_setting_in_settings_panel() {
        let data = AppData::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));

        harness.state_mut().central_view = CentralView::Settings;
        harness.state_mut().settings.theme = ThemeMode::Dark;

        harness.step();
        harness.snapshot("phase11_settings_theme");
    }

    /// Phase 11 self-check: the command palette shows both a matching
    /// action ("Theme: Dark") and a matching request in one combined list.
    #[test]
    #[ignore]
    fn egui_kittest_smoke_renders_command_palette_with_actions_and_requests() {
        let mut data = AppData::default();
        let mut collection = Collection::new("Demo");
        collection.requests.push(RequestItem::new("Theme Park API"));
        data.collections.push(collection);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();

        {
            let state = harness.state_mut();
            state.search_open = true;
            state.search_query = "theme".to_string();
        }
        // A freshly-opened `egui::Window` needs an extra frame to settle
        // its layout before it's actually painted — one `step()` after
        // setting `search_open` isn't enough (confirmed empirically: the
        // window was missing entirely from a single-step snapshot).
        harness.step();
        harness.step();
        harness.snapshot("phase11_command_palette");
    }

    /// The leader-key chord (space, then s, then f) is now the *only* way
    /// to open the command palette (Alt+Space/Cmd+K were removed outright)
    /// — driven by real injected key events, not by setting `search_open`
    /// directly.
    #[test]
    #[ignore]
    fn leader_key_space_s_f_opens_the_command_palette() {
        let data = AppData::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();
        assert!(!harness.state().search_open);

        harness.key_press(egui::Key::Space);
        harness.step();
        harness.key_press(egui::Key::S);
        harness.step();
        harness.key_press(egui::Key::F);
        harness.step();

        assert!(
            harness.state().search_open,
            "space, then s, then f should open the command palette"
        );
        assert_eq!(
            harness.state().palette_scope,
            PaletteScope::RequestsOnly,
            "<leader>sf is the requests-only view, not the combined one"
        );
    }

    /// `<leader>sf` shows *only* request entries — no `PaletteAction`s at
    /// all, even for a query that would otherwise fuzzy-match one — unlike
    /// `<leader>se`, which reuses this same window just pre-filtered by its
    /// own query text (both actions and requests still candidates there).
    #[test]
    #[ignore]
    fn leader_key_space_s_f_shows_requests_only_no_commands() {
        use egui_kittest::kittest::Queryable;

        let mut data = AppData::default();
        let mut collection = Collection::new("Demo");
        collection.requests.push(RequestItem::new("Theme Park API"));
        data.collections.push(collection);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();

        harness.key_press(egui::Key::Space);
        harness.step();
        harness.key_press(egui::Key::S);
        harness.step();
        harness.key_press(egui::Key::F);
        harness.step();

        // "theme" fuzzy-matches both the request "Theme Park API" and the
        // "Theme: Dark"/"Theme: Light"/"Theme: System" actions — the actual
        // case this flag exists to prevent.
        harness.state_mut().search_query = "theme".to_string();
        harness.step();
        harness.step();

        assert!(
            harness.query_by_label("Theme Park API").is_some(),
            "the matching request should still show up"
        );
        assert!(
            harness.query_by_label("> Theme: Dark").is_none(),
            "no action should show up in the requests-only view, even one that fuzzy-matches"
        );
    }

    /// An abandoned sequence (a key that isn't a prefix of any known chord)
    /// resets silently rather than leaving a stale partial match that a
    /// later, unrelated "s" or "f" keypress could accidentally complete.
    #[test]
    #[ignore]
    fn leader_key_sequence_aborts_on_a_non_matching_key() {
        let data = AppData::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();

        harness.key_press(egui::Key::Space);
        harness.step();
        harness.key_press(egui::Key::Z); // not "s" — not a prefix of anything
        harness.step();
        harness.key_press(egui::Key::S);
        harness.step();
        harness.key_press(egui::Key::F);
        harness.step();

        assert!(
            !harness.state().search_open,
            "an abandoned chord must not leave a stale partial match behind"
        );
    }

    /// Space, s, e opens the palette pre-filtered to just the
    /// "Environment: ..." entries — a quick keyboard-only environment
    /// switcher reusing the palette's own fuzzy list/selection instead of a
    /// second bespoke picker.
    #[test]
    #[ignore]
    fn leader_key_space_s_e_opens_the_palette_filtered_to_environments() {
        let mut data = AppData::default();
        data.environments.push(Environment::new("Staging"));
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();

        harness.key_press(egui::Key::Space);
        harness.step();
        harness.key_press(egui::Key::S);
        harness.step();
        harness.key_press(egui::Key::E);
        harness.step();

        assert!(harness.state().search_open, "space, s, e opens the palette");
        assert_eq!(
            harness.state().search_query,
            "Environment:",
            "pre-filled so the palette narrows to just the environment entries"
        );
    }

    /// `<leader>sc` opens the palette showing *only* `PaletteAction`s — no
    /// requests, even for a query that would otherwise fuzzy-match one —
    /// the commands-only counterpart to `<leader>sf`'s requests-only view.
    #[test]
    #[ignore]
    fn leader_key_space_s_c_shows_commands_only_no_requests() {
        use egui_kittest::kittest::Queryable;

        let mut data = AppData::default();
        let mut collection = Collection::new("Demo");
        collection.requests.push(RequestItem::new("New Tab Widget"));
        data.collections.push(collection);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();

        harness.key_press(egui::Key::Space);
        harness.step();
        harness.key_press(egui::Key::S);
        harness.step();
        harness.key_press(egui::Key::C);
        harness.step();

        assert_eq!(harness.state().palette_scope, PaletteScope::CommandsOnly);

        // "New Tab" fuzzy-matches both the "New Tab" action and the
        // "New Tab Widget" request — commands-only must show only the
        // former.
        harness.state_mut().search_query = "New Tab".to_string();
        harness.step();

        assert!(harness.query_by_label("> New Tab").is_some());
        assert!(harness.query_by_label("New Tab Widget").is_none());
    }

    /// The which-key-style hint popup appears while a leader chord is in
    /// progress and lists every candidate that still matches, then
    /// disappears once the chord resolves.
    #[test]
    #[ignore]
    fn leader_which_key_hint_shows_matching_candidates_while_typing() {
        use egui_kittest::kittest::Queryable;

        let data = AppData::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();

        harness.key_press(egui::Key::Space);
        harness.step();
        assert!(
            harness.query_by_label("Search requests").is_some(),
            "the hint should list every top-level chord right after Space"
        );
        assert!(harness.query_by_label("Toggle sidebar").is_some());

        harness.key_press(egui::Key::S);
        harness.step();
        assert!(
            harness.query_by_label("Search requests").is_some(),
            "still shows the s-prefixed chords"
        );
        assert!(
            harness.query_by_label("Toggle sidebar").is_none(),
            "no longer a candidate once 's' narrows away from it"
        );

        harness.key_press(egui::Key::F);
        harness.step();
        assert!(
            harness.query_by_label("Search requests").is_none(),
            "the hint disappears once the chord resolves"
        );
    }

    /// Space, a, h starts hotkey assignment for the *active tab's* request
    /// — a keyboard-only alternative to the sidebar's "Assign hotkey…"
    /// context-menu entry, for a request that's already open rather than
    /// visible in the sidebar tree.
    #[test]
    #[ignore]
    fn leader_key_space_a_h_starts_hotkey_assignment_for_the_active_request() {
        let data = AppData::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();
        let active_request_id = harness.state().active_tab().current_request.id;
        assert!(harness.state().pending_hotkey_assignment.is_none());

        harness.key_press(egui::Key::Space);
        harness.step();
        harness.key_press(egui::Key::A);
        harness.step();
        harness.key_press(egui::Key::H);
        harness.step();

        assert_eq!(
            harness.state().pending_hotkey_assignment,
            Some(active_request_id),
            "space, a, h should start hotkey assignment for the active tab's request"
        );
    }

    /// Space, u, s toggles the sidebar — a keyboard-only alternative to the
    /// top bar's "Hide/Show Sidebar" button, sharing the same
    /// `toggle_sidebar` (including clearing `sidebar_auto_hidden`, so it
    /// behaves identically to a manual click either way).
    #[test]
    #[ignore]
    fn leader_key_space_u_s_toggles_the_sidebar() {
        let data = AppData::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();
        assert!(harness.state().sidebar_visible);

        harness.key_press(egui::Key::Space);
        harness.step();
        harness.key_press(egui::Key::U);
        harness.step();
        harness.key_press(egui::Key::S);
        harness.step();

        assert!(
            !harness.state().sidebar_visible,
            "space, u, s should hide the sidebar"
        );

        harness.key_press(egui::Key::Space);
        harness.step();
        harness.key_press(egui::Key::U);
        harness.step();
        harness.key_press(egui::Key::S);
        harness.step();

        assert!(
            harness.state().sidebar_visible,
            "space, u, s again should show it again"
        );
    }

    /// The palette had no selection cursor at all before this — Enter
    /// always picked whatever entry happened to be first, regardless of
    /// what was visibly highlighted (nothing was). `Ctrl+N`/`Ctrl+P` now
    /// move one, wrapping at both ends.
    #[test]
    #[ignore]
    fn ctrl_n_and_ctrl_p_move_the_palette_selection_with_wraparound() {
        let mut data = AppData::default();
        let mut collection = Collection::new("Demo");
        collection.requests.push(RequestItem::new("Alpha"));
        collection.requests.push(RequestItem::new("Beta"));
        data.collections.push(collection);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();

        harness.state_mut().search_open = true;
        harness.step();
        harness.step();
        assert_eq!(harness.state().palette_selected, 0);

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::N);
        harness.step();
        assert_eq!(
            harness.state().palette_selected,
            1,
            "Ctrl+N moves to the next entry"
        );

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::P);
        harness.step();
        assert_eq!(
            harness.state().palette_selected,
            0,
            "Ctrl+P moves back to the previous entry"
        );

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::P);
        harness.step();
        assert!(
            harness.state().palette_selected > 0,
            "Ctrl+P from the first entry should wrap around to the last one"
        );
    }

    /// Real, reported bug: Ctrl+N/P moved the highlight but never scrolled
    /// the list, so once there were more entries than fit in the palette's
    /// fixed-height scroll area, the selection kept moving invisibly off-
    /// screen. Enough requests to overflow that height, then Ctrl+N well
    /// past the fold — reviewed as a snapshot (not a structural assertion:
    /// `egui::ScrollArea::show` lays out every row regardless of scroll
    /// position, so a plain "is this row's label queryable" check would
    /// pass whether or not the fix actually works — this is a genuinely
    /// visual concern).
    #[test]
    #[ignore]
    fn ctrl_n_scrolls_the_palette_to_keep_the_selection_visible() {
        let mut data = AppData::default();
        let mut collection = Collection::new("Demo");
        for i in 0..30 {
            collection
                .requests
                .push(RequestItem::new(format!("Request {i:02}")));
        }
        data.collections.push(collection);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));
        harness.step();

        harness.state_mut().search_open = true;
        harness.step();
        harness.step();

        for _ in 0..25 {
            harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::N);
            harness.step();
        }
        assert_eq!(harness.state().palette_selected, 25);
        harness.snapshot("phase17_palette_scroll_follows_selection");
    }

    /// The first explicit light-theme baseline in this project — every
    /// other snapshot relies on `egui_kittest`'s implicit dark-mode
    /// fallback (no test forces a theme). Forcing `Light` here catches any
    /// contrast/readability issue light mode exposes that dark mode didn't.
    #[test]
    #[ignore]
    fn egui_kittest_smoke_renders_settings_panel_in_light_theme() {
        let data = AppData::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));

        harness.ctx.set_theme(egui::ThemePreference::Light);
        harness.state_mut().central_view = CentralView::Settings;
        harness.state_mut().settings.theme = ThemeMode::Light;

        harness.step();
        harness.snapshot("phase11_settings_light_theme");
    }

    /// Phase 13 self-check: the folder editor (reached via the folder
    /// context menu's new "Edit" entry) renders its description field and
    /// the same `script_editor`/`snippet_buttons` widgets the request
    /// editor's script tabs already use — reused, not reinvented, for a
    /// folder's own pre-request/test scripts.
    #[test]
    #[ignore]
    fn egui_kittest_smoke_renders_folder_editor() {
        let mut data = AppData::default();
        let mut collection = Collection::new("Demo");
        let mut folder = Folder::new("Auth");
        folder.description = "Everything auth-related.".to_string();
        folder.pre_request_script = "pm.environment.set(\"x\", \"1\")\n".to_string();
        folder.post_response_script = "pm.test(\"ok\", function() end)\n".to_string();
        let folder_id = folder.id;
        collection.folders.push(folder);
        let collection_id = collection.id;
        data.collections.push(collection);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));

        harness.state_mut().central_view = CentralView::FolderEditor {
            collection: collection_id,
            folder_path: vec![folder_id],
        };

        harness.step();
        harness.snapshot("phase13_folder_editor");
    }

    #[test]
    fn parse_bulk_edit_text_reads_enabled_and_disabled_rows() {
        let parsed = parse_bulk_edit_text(
            "Content-Type: application/json\n// X-Debug: true\nAccept:\n\n  X-Trim : spaced  \n",
        );
        assert_eq!(
            parsed,
            vec![
                KeyValue {
                    key: "Content-Type".to_string(),
                    value: "application/json".to_string(),
                    enabled: true,
                },
                KeyValue {
                    key: "X-Debug".to_string(),
                    value: "true".to_string(),
                    enabled: false,
                },
                KeyValue {
                    key: "Accept".to_string(),
                    value: String::new(),
                    enabled: true,
                },
                KeyValue {
                    key: "X-Trim".to_string(),
                    value: "spaced".to_string(),
                    enabled: true,
                },
            ]
        );
    }

    #[test]
    fn format_bulk_edit_text_prefixes_disabled_rows_with_a_comment_marker() {
        let items = vec![
            KeyValue {
                key: "a".to_string(),
                value: "1".to_string(),
                enabled: true,
            },
            KeyValue {
                key: "b".to_string(),
                value: "2".to_string(),
                enabled: false,
            },
        ];
        assert_eq!(format_bulk_edit_text(&items), "a: 1\n// b: 2");
    }

    #[test]
    fn bulk_edit_text_round_trips_through_format_and_parse() {
        let items = vec![
            KeyValue {
                key: "key1".to_string(),
                value: "value1".to_string(),
                enabled: true,
            },
            KeyValue {
                key: "key2".to_string(),
                value: "value2".to_string(),
                enabled: false,
            },
        ];
        let text = format_bulk_edit_text(&items);
        assert_eq!(parse_bulk_edit_text(&text), items);
    }

    /// Phase 13 self-check: the Bulk Edit toggle swaps the Params table for
    /// a raw-text view showing the seeded rows, one `key: value` per line,
    /// the disabled row prefixed with `// ` — reviewed visually for any
    /// glyph/layout issue (plain ASCII throughout, so none expected, but
    /// verified rather than assumed, per this project's recurring lesson).
    #[test]
    #[ignore]
    fn egui_kittest_smoke_renders_bulk_edit_mode() {
        let data = AppData::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 750.0))
            .build_eframe(|_cc| App::with_data(data));

        {
            let tab = harness.state_mut().active_tab_mut();
            tab.current_request.params.push(KeyValue {
                key: "page".to_string(),
                value: "1".to_string(),
                enabled: true,
            });
            tab.current_request.params.push(KeyValue {
                key: "debug".to_string(),
                value: "true".to_string(),
                enabled: false,
            });
        }
        harness.step();

        // Flip the Params table into Bulk Edit mode by finding and clicking
        // its toggle button, same as a real user would.
        use egui_kittest::kittest::Queryable;
        harness.get_by_label("Bulk Edit").click();
        harness.step();
        // One more step: the toggle button's own label text is decided
        // before its `.clicked()` is known within a single immediate-mode
        // frame (the same widget draws before the click it just received is
        // processed), so the label reads "Table Edit" only from the frame
        // after the toggle actually took effect — an artifact of a
        // single-frame click-then-snapshot test, not of real usage (the app
        // repaints continuously, so a real user never sees a stale label).
        harness.step();
        harness.snapshot("phase13_bulk_edit");
    }
}
