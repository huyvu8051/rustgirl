use crate::http_client::{self, HttpResponse, RequestOutcome, SentRequest};
use crate::model::{
    self, AppData, BodyMode, Collection, Environment, Folder, HistoryEntry, KeyValue, Method,
    RequestItem,
};
use crate::storage;
use crate::syntax;
use eframe::egui;
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
    Body,
}

#[derive(PartialEq, Clone, Copy)]
enum ResponseTab {
    Body,
    Headers,
    Request,
}

#[derive(Clone, Copy)]
enum CentralView {
    Request,
    EnvironmentEditor(Uuid),
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
    Collection { collection: Uuid, folder: Option<Uuid>, request: Uuid },
}

pub struct App {
    data: AppData,
    rt: tokio::runtime::Runtime,
    client: reqwest::Client,
    tx: Sender<(Uuid, RequestOutcome)>,
    rx: Receiver<(Uuid, RequestOutcome)>,

    sidebar_tab: SidebarTab,
    central_view: CentralView,
    split_direction: SplitDirection,
    active_environment: Option<Uuid>,

    current_request: RequestItem,
    origin: RequestOrigin,
    in_flight_id: Uuid,

    request_tab: RequestTab,
    response_tab: ResponseTab,
    is_loading: bool,
    response: Option<HttpResponse>,
    response_error: Option<String>,
    sent_request: Option<SentRequest>,
    auto_format_response: bool,

    new_collection_name: String,
    new_environment_name: String,

    /// Snapshot of `current_request` as of the last disk write, so we can
    /// detect edits without re-saving on every unchanged frame.
    autosave_last_synced: Option<RequestItem>,
    /// Throttles autosave writes to disk so continuous typing doesn't hit
    /// the filesystem every frame.
    autosave_last_saved_at: Option<std::time::Instant>,

    /// Opt/Alt+Space quick-open: search every request by name/URL/method.
    search_open: bool,
    search_query: String,
    search_needs_focus: bool,

    /// Find-in-body text for the request/response body viewers.
    request_body_find: String,
    response_body_find: String,
}

impl App {
    pub fn new() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to create tokio runtime");
        let _guard = rt.enter();
        let client = reqwest::Client::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let data = storage::load();
        let active_environment = data.environments.first().map(|e| e.id);

        Self {
            data,
            rt,
            client,
            tx,
            rx,
            sidebar_tab: SidebarTab::Collections,
            central_view: CentralView::Request,
            split_direction: SplitDirection::Vertical,
            active_environment,
            current_request: RequestItem::new("New Request"),
            origin: RequestOrigin::Unsaved,
            in_flight_id: Uuid::nil(),
            request_tab: RequestTab::Params,
            response_tab: ResponseTab::Body,
            is_loading: false,
            response: None,
            response_error: None,
            sent_request: None,
            auto_format_response: true,
            new_collection_name: String::new(),
            new_environment_name: String::new(),
            autosave_last_synced: None,
            autosave_last_saved_at: None,
            search_open: false,
            search_query: String::new(),
            search_needs_focus: false,
            request_body_find: String::new(),
            response_body_find: String::new(),
        }
    }

    fn save(&self) {
        storage::save(&self.data);
    }

    fn active_env(&self) -> Option<Environment> {
        self.active_environment
            .and_then(|id| self.data.environments.iter().find(|e| e.id == id).cloned())
    }

    fn poll_responses(&mut self, ctx: &egui::Context) {
        while let Ok((id, outcome)) = self.rx.try_recv() {
            if id != self.in_flight_id {
                continue; // stale response from a superseded request
            }
            self.is_loading = false;
            match outcome {
                RequestOutcome::Success { request, response } => {
                    let entry = HistoryEntry {
                        id: Uuid::new_v4(),
                        timestamp: chrono::Utc::now(),
                        method: self.current_request.method,
                        url: self.current_request.url.clone(),
                        status: Some(response.status),
                        request: self.current_request.clone(),
                    };
                    self.data.history.insert(0, entry);
                    self.data.history.truncate(200);
                    self.sent_request = Some(request);
                    self.response = Some(response);
                    self.response_error = None;
                    self.save();
                }
                RequestOutcome::Error { request, message } => {
                    self.sent_request = request;
                    self.response = None;
                    self.response_error = Some(message);
                }
            }
        }
        if self.is_loading {
            ctx.request_repaint();
        }
    }

    fn send_current_request(&mut self) {
        let id = Uuid::new_v4();
        self.in_flight_id = id;
        self.is_loading = true;
        self.response = None;
        self.response_error = None;
        self.sent_request = None;

        let client = self.client.clone();
        let item = self.current_request.clone();
        let env = self.active_env();
        let tx = self.tx.clone();

        self.rt.spawn(async move {
            let outcome = http_client::send_request(client, item, env).await;
            let _ = tx.send((id, outcome));
        });
    }

    // ---------- UI: top bar ----------
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("RustGirl");
                ui.separator();
                ui.label("Environment:");
                let current_name = self
                    .active_env()
                    .map(|e| e.name)
                    .unwrap_or_else(|| "No Environment".to_string());
                egui::ComboBox::from_id_salt("active_env_combo")
                    .selected_text(current_name)
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

                ui.separator();
                let (icon, tooltip) = match self.split_direction {
                    SplitDirection::Vertical => ("⬍ Split", "Request on top, response below — click for side-by-side"),
                    SplitDirection::Horizontal => ("⬌ Split", "Request left, response right — click for stacked"),
                };
                if ui.button(icon).on_hover_text(tooltip).clicked() {
                    self.split_direction = match self.split_direction {
                        SplitDirection::Vertical => SplitDirection::Horizontal,
                        SplitDirection::Horizontal => SplitDirection::Vertical,
                    };
                }
            });
        });
    }

    // ---------- UI: sidebar ----------
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(280.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.sidebar_tab, SidebarTab::Collections, "Collections");
                    ui.selectable_value(&mut self.sidebar_tab, SidebarTab::Environments, "Environments");
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
            let submitted =
                field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let can_add = !self.new_collection_name.trim().is_empty();
            let clicked = ui
                .add_enabled(
                    can_add,
                    egui::Button::new("+ Collection").min_size(egui::vec2(ui.available_width(), 0.0)),
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
        ui.separator();

        let mut save_needed = false;
        let mut load_request: Option<(Uuid, Option<Uuid>, RequestItem)> = None;
        let mut delete_collection: Option<Uuid> = None;
        let focused_id = self.current_request.id;

        for collection in &mut self.data.collections {
            let collection_id = collection.id;
            egui::CollapsingHeader::new(&collection.name)
                .id_salt(collection_id)
                .default_open(true)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui.small_button("+ request").clicked() {
                            collection.requests.push(RequestItem::new("New Request"));
                            save_needed = true;
                        }
                        if ui.small_button("+ folder").clicked() {
                            collection.folders.push(Folder::new("New Folder"));
                            save_needed = true;
                        }
                        if ui.small_button("🗑").on_hover_text("Delete collection").clicked() {
                            delete_collection = Some(collection_id);
                        }
                    });

                    for req in &mut collection.requests {
                        ui.horizontal(|ui| {
                            ui.label(req.method.as_str());
                            if ui
                                .selectable_label(req.id == focused_id, &req.name)
                                .clicked()
                            {
                                load_request = Some((collection_id, None, req.clone()));
                            }
                        });
                    }

                    for folder in &mut collection.folders {
                        let folder_id = folder.id;
                        egui::CollapsingHeader::new(&folder.name)
                            .id_salt(folder_id)
                            .show(ui, |ui| {
                                if ui.small_button("+ request").clicked() {
                                    folder.requests.push(RequestItem::new("New Request"));
                                    save_needed = true;
                                }
                                for req in &mut folder.requests {
                                    ui.horizontal(|ui| {
                                        ui.label(req.method.as_str());
                                        if ui
                                            .selectable_label(req.id == focused_id, &req.name)
                                            .clicked()
                                        {
                                            load_request =
                                                Some((collection_id, Some(folder_id), req.clone()));
                                        }
                                    });
                                }
                            });
                    }
                });
        }

        if let Some(id) = delete_collection {
            self.data.collections.retain(|c| c.id != id);
            save_needed = true;
        }
        if let Some((collection, folder, req)) = load_request {
            self.current_request = req.clone();
            self.origin = RequestOrigin::Collection {
                collection,
                folder,
                request: req.id,
            };
            self.central_view = CentralView::Request;
            self.response = None;
            self.response_error = None;
            self.sent_request = None;
            self.autosave_last_synced = Some(self.current_request.clone());
        }
        if save_needed {
            self.save();
        }
    }

    fn environments_sidebar(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.new_environment_name)
                    .hint_text("Environment name")
                    .desired_width(f32::INFINITY),
            );
            let submitted =
                field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let can_add = !self.new_environment_name.trim().is_empty();
            let clicked = ui
                .add_enabled(
                    can_add,
                    egui::Button::new("+ Environment").min_size(egui::vec2(ui.available_width(), 0.0)),
                )
                .clicked();
            if can_add && (clicked || submitted) {
                self.data
                    .environments
                    .push(Environment::new(self.new_environment_name.trim().to_string()));
                self.new_environment_name.clear();
                self.save();
            }
        });
        ui.separator();

        let mut delete_env: Option<Uuid> = None;
        for env in &self.data.environments {
            ui.horizontal(|ui| {
                let is_active = self.active_environment == Some(env.id);
                if ui.selectable_label(is_active, &env.name).clicked() {
                    self.active_environment = Some(env.id);
                }
                if ui.small_button("edit").clicked() {
                    self.central_view = CentralView::EnvironmentEditor(env.id);
                }
                if ui.small_button("🗑").clicked() {
                    delete_env = Some(env.id);
                }
            });
        }
        if let Some(id) = delete_env {
            self.data.environments.retain(|e| e.id != id);
            if self.active_environment == Some(id) {
                self.active_environment = None;
            }
            self.save();
        }
    }

    fn history_sidebar(&mut self, ui: &mut egui::Ui) {
        if ui.button("Clear history").clicked() {
            self.data.history.clear();
            self.save();
        }
        ui.separator();
        let mut load: Option<RequestItem> = None;
        for entry in &self.data.history {
            let status = entry
                .status
                .map(|s| s.to_string())
                .unwrap_or_else(|| "-".to_string());
            let label = format!("{} {} [{}]", entry.method.as_str(), entry.url, status);
            if ui.selectable_label(false, label).clicked() {
                load = Some(entry.request.clone());
            }
        }
        if let Some(req) = load {
            self.current_request = req;
            self.origin = RequestOrigin::Unsaved;
            self.central_view = CentralView::Request;
            self.response = None;
            self.response_error = None;
            self.sent_request = None;
            self.autosave_last_synced = None;
            self.autosave_last_saved_at = None;
        }
    }

    // ---------- UI: Opt/Alt+Space quick-open ----------
    fn search_palette(&mut self, ctx: &egui::Context) {
        if !self.search_open {
            return;
        }

        let mut still_open = true;
        let mut load: Option<(Uuid, Option<Uuid>, RequestItem)> = None;
        let mut close = false;

        egui::Window::new("Search Requests")
            .id(egui::Id::new("search_palette_window"))
            .open(&mut still_open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 80.0))
            .default_width(520.0)
            .show(ctx, |ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.search_query)
                        .hint_text("Search by name, URL, or method\u{2026}")
                        .desired_width(f32::INFINITY),
                );
                if self.search_needs_focus {
                    response.request_focus();
                    self.search_needs_focus = false;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }

                ui.separator();

                let query = self.search_query.trim().to_lowercase();
                let mut matches: Vec<(Uuid, Option<Uuid>, &RequestItem)> = Vec::new();
                for c in &self.data.collections {
                    for r in &c.requests {
                        if request_matches_query(r, &query) {
                            matches.push((c.id, None, r));
                        }
                    }
                    for f in &c.folders {
                        for r in &f.requests {
                            if request_matches_query(r, &query) {
                                matches.push((c.id, Some(f.id), r));
                            }
                        }
                    }
                }
                matches.truncate(50);

                let enter_pressed =
                    response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if enter_pressed && let Some((cid, fid, r)) = matches.first() {
                    load = Some((*cid, *fid, (*r).clone()));
                }

                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        if matches.is_empty() {
                            ui.weak("No matching requests.");
                        }
                        for (cid, fid, r) in &matches {
                            let label = format!("{}   {}   —   {}", r.method.as_str(), r.name, r.url);
                            if ui.selectable_label(false, label).clicked() {
                                load = Some((*cid, *fid, (*r).clone()));
                            }
                        }
                    });
            });

        if let Some((collection, folder, req)) = load {
            self.current_request = req.clone();
            self.origin = RequestOrigin::Collection {
                collection,
                folder,
                request: req.id,
            };
            self.central_view = CentralView::Request;
            self.response = None;
            self.response_error = None;
            self.sent_request = None;
            self.autosave_last_synced = Some(self.current_request.clone());
            self.autosave_last_saved_at = None;
            close = true;
        }

        self.search_open = still_open && !close;
    }

    // ---------- UI: central ----------
    fn central(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| match self.central_view {
            CentralView::Request => match self.split_direction {
                SplitDirection::Vertical => {
                    let height = ui.available_height() * 0.55;
                    egui::Panel::top("request_editor_panel_v")
                        .resizable(true)
                        .default_size(height)
                        .min_size(280.0)
                        .show(ui, |ui| self.request_editor(ui));
                    egui::CentralPanel::default().show(ui, |ui| self.response_viewer(ui));
                }
                SplitDirection::Horizontal => {
                    let width = ui.available_width() * 0.5;
                    egui::Panel::left("request_editor_panel_h")
                        .resizable(true)
                        .default_size(width)
                        .min_size(320.0)
                        .show(ui, |ui| self.request_editor(ui));
                    egui::CentralPanel::default().show(ui, |ui| self.response_viewer(ui));
                }
            },
            CentralView::EnvironmentEditor(id) => self.environment_editor(ui, id),
        });
    }

    fn request_editor(&mut self, ui: &mut egui::Ui) {
        let send_shortcut =
            ui.input_mut(|i| i.consume_key(egui::Modifiers::ALT, egui::Key::Enter));
        if send_shortcut && !self.is_loading {
            self.send_current_request();
        }

        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.current_request.name);
            if ui.button("Save").clicked() {
                self.save_current_request();
            }
        });

        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("method_combo")
                .selected_text(self.current_request.method.as_str())
                .show_ui(ui, |ui| {
                    for m in Method::ALL {
                        ui.selectable_value(&mut self.current_request.method, m, m.as_str());
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.current_request.url)
                    .hint_text("https://api.example.com/{{path}}")
                    .desired_width(ui.available_width() - 80.0),
            );
            if ui
                .button("Send")
                .on_hover_text("Option+Enter")
                .clicked()
            {
                self.send_current_request();
            }
        });
        if self.is_loading {
            ui.spinner();
        }

        ui.separator();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.request_tab, RequestTab::Params, "Params");
            ui.selectable_value(&mut self.request_tab, RequestTab::Headers, "Headers");
            ui.selectable_value(&mut self.request_tab, RequestTab::Body, "Body");
        });
        ui.separator();

        egui::ScrollArea::vertical()
            .id_salt("request_editor_scroll")
            .show(ui, |ui| match self.request_tab {
                RequestTab::Params => {
                    ui.label("Query Params:");
                    key_value_table(ui, "params_table", &mut self.current_request.params);
                    let url = self.current_request.url.clone();
                    path_params_editor(ui, &url, &mut self.current_request.path_params);
                }
                RequestTab::Headers => key_value_table(ui, "headers_table", &mut self.current_request.headers),
                RequestTab::Body => {
                    let body = &mut self.current_request.body;
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut body.mode, BodyMode::None, "None");
                        ui.selectable_value(&mut body.mode, BodyMode::Json, "JSON");
                        ui.selectable_value(&mut body.mode, BodyMode::Raw, "Raw");
                        ui.selectable_value(&mut body.mode, BodyMode::Form, "Form");
                    });
                    if !matches!(body.mode, BodyMode::None | BodyMode::Form) {
                        ui.horizontal(|ui| {
                            ui.label("Find:");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.request_body_find)
                                    .hint_text("search in body")
                                    .desired_width(200.0),
                            );
                            if !self.request_body_find.is_empty() {
                                let n = syntax::find_matches(&body.raw, &self.request_body_find).len();
                                ui.weak(format!("{n} match{}", if n == 1 { "" } else { "es" }));
                            }
                        });
                    }
                    match body.mode {
                        BodyMode::None => {
                            ui.label("This request has no body.");
                        }
                        BodyMode::Json | BodyMode::Raw => {
                            let language = match body.mode {
                                BodyMode::Json => syntax::Language::Json,
                                _ => syntax::sniff_language(&body.raw),
                            };
                            let dark = ui.visuals().dark_mode;
                            let font_id = egui::TextStyle::Monospace.resolve(ui.style());
                            let search_query = self.request_body_find.clone();
                            let mut layouter =
                                move |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
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
                            let text_edit = egui::TextEdit::multiline(&mut body.raw)
                                .code_editor()
                                .desired_rows(10)
                                .desired_width(f32::INFINITY)
                                .layouter(&mut layouter);
                            show_code_editor_with_links(ui, &content_snapshot, text_edit);
                        }
                        BodyMode::Form => {
                            key_value_table(ui, "form_table", &mut body.form);
                        }
                    }
                }
            });
    }

    /// Writes `current_request` into its slot under `data.collections` if
    /// `origin` points at one. Returns `false` (a no-op) when the request
    /// hasn't been saved anywhere yet (`RequestOrigin::Unsaved`).
    fn sync_current_request_into_data(&mut self) -> bool {
        let (collection, folder, request) = match &self.origin {
            RequestOrigin::Collection {
                collection,
                folder,
                request,
            } => (*collection, *folder, *request),
            RequestOrigin::Unsaved => return false,
        };
        let req = self.current_request.clone();
        let Some(c) = self.data.collections.iter_mut().find(|c| c.id == collection) else {
            return false;
        };
        let list = match folder {
            None => &mut c.requests,
            Some(fid) => match c.folders.iter_mut().find(|f| f.id == fid) {
                Some(f) => &mut f.requests,
                None => &mut c.requests,
            },
        };
        if let Some(existing) = list.iter_mut().find(|r| r.id == request) {
            *existing = req;
        } else {
            list.push(req);
        }
        true
    }

    /// Explicit "Save" button: parks a never-saved request into a default
    /// "Saved Requests" collection, or otherwise just writes it back in place.
    fn save_current_request(&mut self) {
        if matches!(self.origin, RequestOrigin::Unsaved) {
            let req = self.current_request.clone();
            let target = if let Some(c) = self
                .data
                .collections
                .iter_mut()
                .find(|c| c.name == "Saved Requests")
            {
                c
            } else {
                self.data.collections.push(Collection::new("Saved Requests"));
                self.data.collections.last_mut().unwrap()
            };
            let target_id = target.id;
            target.requests.push(req.clone());
            self.origin = RequestOrigin::Collection {
                collection: target_id,
                folder: None,
                request: req.id,
            };
        } else {
            self.sync_current_request_into_data();
        }
        self.save();
        self.autosave_last_synced = Some(self.current_request.clone());
        self.autosave_last_saved_at = Some(std::time::Instant::now());
    }

    /// Called every frame: transparently persists edits to an already-saved
    /// request (one loaded from a collection) without needing the Save
    /// button. Writes are rate-limited so continuous typing doesn't hit disk
    /// every frame; a never-saved request is left alone since there's no
    /// collection slot to write into yet (use the Save button once to pick one).
    fn autosave_if_dirty(&mut self) {
        if matches!(self.origin, RequestOrigin::Unsaved) {
            return;
        }
        if self.autosave_last_synced.as_ref() == Some(&self.current_request) {
            return;
        }
        let now = std::time::Instant::now();
        let throttle = std::time::Duration::from_millis(400);
        let ready = self
            .autosave_last_saved_at
            .is_none_or(|t| now.duration_since(t) >= throttle);
        if ready {
            if self.sync_current_request_into_data() {
                self.save();
            }
            self.autosave_last_synced = Some(self.current_request.clone());
            self.autosave_last_saved_at = Some(now);
        }
    }

    fn response_viewer(&mut self, ui: &mut egui::Ui) {
        if self.response.is_none() && self.response_error.is_none() && self.sent_request.is_none() {
            ui.weak("Send a request to see the response here.");
            return;
        }

        if let Some(resp) = &self.response {
            ui.horizontal(|ui| {
                let color = if resp.status < 300 {
                    egui::Color32::GREEN
                } else if resp.status < 400 {
                    egui::Color32::YELLOW
                } else {
                    egui::Color32::RED
                };
                ui.colored_label(color, format!("Status: {} {}", resp.status, resp.status_text));
                ui.label(format!("Time: {} ms", resp.duration_ms));
                ui.label(format!("Size: {} bytes", resp.size_bytes));
            });
        } else if let Some(err) = &self.response_error {
            ui.colored_label(egui::Color32::RED, err);
        }
        ui.separator();

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.response_tab, ResponseTab::Body, "Body");
            ui.selectable_value(&mut self.response_tab, ResponseTab::Headers, "Headers");
            ui.selectable_value(&mut self.response_tab, ResponseTab::Request, "Request");
            if self.response_tab == ResponseTab::Body {
                ui.add_space(12.0);
                ui.checkbox(&mut self.auto_format_response, "Auto format")
                    .on_hover_text("Formats JSON/XML/HTML based on the response's Content-Type");
            }
        });
        ui.separator();

        egui::ScrollArea::vertical()
            .id_salt("response_scroll")
            .show(ui, |ui| match self.response_tab {
                ResponseTab::Body => {
                    let Some(resp) = &self.response else {
                        ui.weak("No response body.");
                        return;
                    };
                    let text = if self.auto_format_response {
                        format_body(&resp.headers, &resp.body)
                    } else {
                        resp.body.clone()
                    };
                    let content_type = content_type_of(&resp.headers);
                    let language = if content_type.is_empty() {
                        syntax::sniff_language(&text)
                    } else {
                        syntax::language_for_content_type(&content_type)
                    };

                    ui.horizontal(|ui| {
                        ui.label("Find:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.response_body_find)
                                .hint_text("search in body")
                                .desired_width(200.0),
                        );
                        if !self.response_body_find.is_empty() {
                            let n = syntax::find_matches(&text, &self.response_body_find).len();
                            ui.weak(format!("{n} match{}", if n == 1 { "" } else { "es" }));
                        }
                    });

                    let dark = ui.visuals().dark_mode;
                    let font_id = egui::TextStyle::Monospace.resolve(ui.style());
                    let search_query = self.response_body_find.clone();
                    let mut layouter =
                        move |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
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
                    let text_edit = egui::TextEdit::multiline(&mut text_copy)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .layouter(&mut layouter);
                    show_code_editor_with_links(ui, &text, text_edit);
                }
                ResponseTab::Headers => {
                    let Some(resp) = &self.response else {
                        ui.weak("No response headers.");
                        return;
                    };
                    for (k, v) in &resp.headers {
                        ui.label(format!("{k}: {v}"));
                    }
                }
                ResponseTab::Request => {
                    let Some(req) = &self.sent_request else {
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
                        let mut layouter =
                            move |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
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
                        let text_edit = egui::TextEdit::multiline(&mut body_copy)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .layouter(&mut layouter);
                        show_code_editor_with_links(ui, body, text_edit);
                    }
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
fn show_code_editor_with_links(ui: &mut egui::Ui, content: &str, text_edit: egui::TextEdit<'_>) {
    let links = syntax::detect_links(content);
    let output = text_edit.show(ui);
    if links.is_empty() {
        return;
    }
    if !ui.input(|i| i.modifiers.alt) {
        return;
    }
    let Some(hover_pos) = output.response.hover_pos() else {
        return;
    };
    let rel_pos = hover_pos - output.galley_pos;
    let char_idx = output.galley.cursor_from_pos(rel_pos).index.0;
    let byte_idx = content
        .char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(content.len());
    if let Some(range) = links.iter().find(|r| r.contains(&byte_idx)) {
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
        if output.response.clicked() {
            open_in_browser(&content[range.clone()]);
        }
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
fn request_matches_query(item: &RequestItem, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    item.name.to_lowercase().contains(query)
        || item.url.to_lowercase().contains(query)
        || item.method.as_str().to_lowercase().contains(query)
}

fn key_value_table(ui: &mut egui::Ui, id_salt: &str, items: &mut Vec<KeyValue>) {
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
                ui.checkbox(&mut kv.enabled, "");
                ui.add(egui::TextEdit::singleline(&mut kv.key).desired_width(key_width));
                ui.add(egui::TextEdit::singleline(&mut kv.value).desired_width(value_width));
                if ui.small_button("🗑").clicked() {
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

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_responses(ui.ctx());
        self.autosave_if_dirty();
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::ALT, egui::Key::Space)) {
            self.search_open = !self.search_open;
            if self.search_open {
                self.search_query.clear();
                self.search_needs_focus = true;
            }
        }
        self.top_bar(ui);
        self.sidebar(ui);
        self.central(ui);
        self.search_palette(ui.ctx());
        if self.autosave_last_synced.as_ref() != Some(&self.current_request) {
            // Edits are pending but still within the throttle window: keep
            // repainting so the debounce timer actually elapses instead of
            // waiting for unrelated input to trigger the next frame.
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}
