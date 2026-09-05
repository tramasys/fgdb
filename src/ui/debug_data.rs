use super::*;

use std::{
    rc::Weak,
    sync::mpsc::{self, TryRecvError},
};

const PRETTY_PRINTER_PAGE_SIZE: usize = 150;
const DEBUG_DATA_RESULT_PAGE_SIZE: usize = 100;
const DEBUG_DATA_SEARCH_DELAY: Duration = Duration::from_millis(75);
const MAX_DEBUG_DATA_ACTIVITY_BYTES: usize = 32 * 1024;
const MAX_DEBUG_DATA_ACTIVITY_EVENTS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DebugDataActivityKind {
    Progress,
    Success,
    Warning,
    Error,
}

impl DebugDataActivityKind {
    fn label(self) -> &'static str {
        match self {
            Self::Progress => "STARTED",
            Self::Success => "DONE",
            Self::Warning => "NOTICE",
            Self::Error => "ERROR",
        }
    }

    fn css_class(self) -> &'static str {
        match self {
            Self::Progress => "activity-progress",
            Self::Success => "activity-success",
            Self::Warning => "activity-warning",
            Self::Error => "activity-error",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DebugDataActivity {
    kind: DebugDataActivityKind,
    message: Rc<str>,
    time: String,
    occurrences: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DebugDataAction {
    Refresh,
    SetDebuginfodEnabled(bool),
    SetDebuginfodUrls(String),
    SetPrettyPrinting(bool),
    ShowSourceFiles,
    ReloadSourceFiles,
    ShowMoreModules,
    ShowMoreSources,
    ShowMorePrettyPrinters,
    LoadPrettyPrinters,
    LoadPrettyPrinterScript(PathBuf),
    AddSourceDirectory(PathBuf),
    RemoveSourceDirectory(String),
    AddSubstitution { from: String, to: String },
    RemoveSubstitution(String),
    RetrySymbols(Option<String>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct DebugDataState {
    pub(super) refreshing: bool,
    pub(super) debuginfod_status: String,
    pub(super) debuginfod_urls: String,
    pub(super) source_directories: Vec<String>,
    pub(super) substitutions: Vec<(String, String)>,
    pub(super) source_files_ready: bool,
    pub(super) source_files_loading: bool,
    pub(super) source_files_visible: bool,
    pub(super) source_files_error: Option<String>,
    module_limit: usize,
    source_limit: usize,
    pretty_printers: Rc<Vec<PrettyPrinterScope>>,
    pretty_printer_limit: usize,
    pretty_printers_ready: bool,
    pretty_printers_loading: bool,
    pretty_printer_generation: u64,
    pretty_printer_error: Option<String>,
    gcc_pretty_printer_directory: Option<PathBuf>,
    configured_pretty_printer_paths: Vec<PathBuf>,
    runtime_pretty_printer_paths: Vec<PathBuf>,
    pretty_printer_script_loading: bool,
    safe_mode: bool,
    activity: Vec<DebugDataActivity>,
}

impl DebugDataState {
    pub(super) fn from_launch_config(config: &crate::config::LaunchConfig) -> Self {
        Self {
            gcc_pretty_printer_directory: config
                .gcc_pretty_printer_directory()
                .map(Path::to_path_buf),
            configured_pretty_printer_paths: config.pretty_printer_paths.clone(),
            safe_mode: config.safe_mode,
            ..Self::default()
        }
    }
}

#[derive(Clone)]
pub(super) struct DebugDataView {
    pub(super) window: gtk::Window,
    pub(super) refresh: gtk::Button,
    pub(super) overview: gtk::Box,
    pub(super) modules: gtk::Box,
    pub(super) sources: gtk::Box,
    pub(super) printers: gtk::Box,
    pub(super) activity: gtk::Box,
    pub(super) module_search: gtk::Entry,
    pub(super) source_search: gtk::Entry,
    pub(super) printer_search: gtk::Entry,
    pub(super) printer_path: gtk::Entry,
    pub(super) printer_browse: gtk::Button,
    pub(super) printer_load: gtk::Button,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrettyPrinterScope {
    name: String,
    direct_printers: Vec<String>,
    providers: Vec<PrettyPrinterProvider>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrettyPrinterProvider {
    name: String,
    printers: Vec<String>,
}

fn defer_debug_data_action(
    handler: &Rc<RefCell<Option<DebugDataActionHandler>>>,
    action: DebugDataAction,
) {
    let handler = handler.borrow().clone();

    if let Some(handler) = handler {
        glib::idle_add_local_once(move || handler(action));
    }
}

fn dispatch_pretty_printer_path(
    entry: &gtk::Entry,
    handler: &Rc<RefCell<Option<DebugDataActionHandler>>>,
) {
    let path = entry.text();
    let path = path.trim();

    if !path.is_empty() {
        defer_debug_data_action(
            handler,
            DebugDataAction::LoadPrettyPrinterScript(PathBuf::from(path)),
        );
    }
}

fn connect_debug_data_search(search: &gtk::Entry, weak_ui: Weak<Ui>, render: fn(&Ui)) {
    let generation = Rc::new(Cell::new(0_u64));

    search.connect_changed(move |_| {
        let next_generation = generation.get().wrapping_add(1);
        generation.set(next_generation);
        let generation = Rc::clone(&generation);
        let weak_ui = weak_ui.clone();

        glib::timeout_add_local_once(DEBUG_DATA_SEARCH_DELAY, move || {
            if generation.get() == next_generation
                && let Some(ui) = weak_ui.upgrade()
            {
                render(&ui);
            }
        });
    });
}

impl Ui {
    pub(crate) fn set_debug_data_action_handler(
        &self,
        handler: impl Fn(DebugDataAction) + 'static,
    ) {
        self.debug_data_action_handler
            .replace(Some(Rc::new(handler)));
    }

    pub(crate) fn connect_debug_data_actions(self: &Rc<Self>) {
        let weak_ui = Rc::downgrade(self);

        self.debug_data_button.connect_clicked(move |_| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };

            ui.present_debug_data();

            if !ui.debug_data_state.borrow().refreshing {
                ui.dispatch_debug_data_action(DebugDataAction::Refresh);
            }
        });
    }

    fn dispatch_debug_data_action(&self, action: DebugDataAction) {
        defer_debug_data_action(&self.debug_data_action_handler, action);
    }

    pub(crate) fn begin_debug_data_refresh(self: &Rc<Self>) -> u64 {
        let generation = self.debug_data_generation.get().wrapping_add(1);
        self.debug_data_generation.set(generation);
        self.debug_data_state.borrow_mut().refreshing = true;
        self.refresh_module_debug_metadata(true);
        self.render_debug_data_overview();

        generation
    }

    pub(crate) fn debug_data_refresh_is_current(&self, generation: u64) -> bool {
        self.debug_data_generation.get() == generation
    }

    pub(crate) fn finish_debug_data_refresh(&self, generation: u64) {
        if self.debug_data_refresh_is_current(generation) {
            self.debug_data_state.borrow_mut().refreshing = false;
            self.render_debug_data_overview();
        }
    }

    pub(crate) fn set_debug_data_debuginfod(&self, generation: u64, status: String, urls: String) {
        if !self.debug_data_refresh_is_current(generation) {
            return;
        }

        let mut state = self.debug_data_state.borrow_mut();
        let modules_changed = state.debuginfod_status != status;
        let overview_changed = modules_changed || state.debuginfod_urls != urls;
        state.debuginfod_status = status;
        state.debuginfod_urls = urls;
        drop(state);

        if overview_changed {
            self.render_debug_data_overview();
        }

        if modules_changed {
            self.render_debug_data_modules();
        }
    }

    pub(crate) fn set_debug_data_sources(
        &self,
        generation: u64,
        directories: Vec<String>,
        substitutions: Vec<(String, String)>,
    ) {
        if !self.debug_data_refresh_is_current(generation) {
            return;
        }

        let mut state = self.debug_data_state.borrow_mut();

        let changed =
            state.source_directories != directories || state.substitutions != substitutions;

        if changed {
            state.source_directories = directories;
            state.substitutions = substitutions;
        }

        drop(state);

        if changed {
            self.render_debug_data_sources();
        }
    }

    pub(crate) fn begin_debug_data_pretty_printer_refresh(&self) -> Option<u64> {
        let mut state = self.debug_data_state.borrow_mut();

        if state.pretty_printers_loading {
            return None;
        }

        state.pretty_printer_generation = state.pretty_printer_generation.wrapping_add(1);
        let generation = state.pretty_printer_generation;
        state.pretty_printers_loading = true;
        state.pretty_printer_error = None;
        drop(state);
        self.render_debug_data_printers();

        Some(generation)
    }

    pub(crate) fn debug_data_pretty_printer_refresh_is_current(&self, generation: u64) -> bool {
        self.debug_data_state.borrow().pretty_printer_generation == generation
    }

    pub(crate) fn finish_debug_data_pretty_printer_refresh(
        &self,
        generation: u64,
        printers: Result<Vec<String>, String>,
    ) {
        let mut state = self.debug_data_state.borrow_mut();

        if state.pretty_printer_generation != generation {
            return;
        }

        state.pretty_printers_loading = false;

        match printers {
            Ok(printers) => {
                let printers = Rc::new(parse_pretty_printer_scopes(&printers));

                if state.pretty_printers != printers {
                    state.pretty_printers = printers;
                    state.pretty_printer_limit = PRETTY_PRINTER_PAGE_SIZE;
                }

                state.pretty_printers_ready = true;
                state.pretty_printer_error = None;
            }
            Err(error) => state.pretty_printer_error = Some(error),
        }

        drop(state);
        self.render_debug_data_printers();
    }

    pub(crate) fn debug_data_pretty_printers_were_requested(&self) -> bool {
        let state = self.debug_data_state.borrow();

        state.pretty_printers_ready
            || state.pretty_printers_loading
            || state.pretty_printer_error.is_some()
    }

    pub(crate) fn begin_pretty_printer_script_load(&self, path: &Path) -> Result<(), String> {
        let mut state = self.debug_data_state.borrow_mut();

        if state.pretty_printer_script_loading {
            return Err(String::from(
                "Another pretty-printer script is still loading",
            ));
        }

        if state
            .runtime_pretty_printer_paths
            .iter()
            .any(|loaded| loaded == path)
        {
            return Err(String::from(
                "This pretty-printer script is already loaded for the current GDB session",
            ));
        }

        state.pretty_printer_script_loading = true;
        drop(state);
        self.render_debug_data_printers();

        Ok(())
    }

    pub(crate) fn finish_pretty_printer_script_load(&self, path: PathBuf, loaded: bool) {
        let mut state = self.debug_data_state.borrow_mut();
        state.pretty_printer_script_loading = false;

        if loaded && !state.runtime_pretty_printer_paths.contains(&path) {
            state.runtime_pretty_printer_paths.push(path);
        }

        drop(state);

        if loaded && let Some(view) = self.debug_data_view.borrow().as_ref() {
            view.printer_path.set_text("");
        }

        self.render_debug_data_printers();
    }

    pub(crate) fn reset_runtime_pretty_printer_scripts(&self) {
        let mut state = self.debug_data_state.borrow_mut();
        let reload_registry = state.pretty_printers_ready
            || state.pretty_printers_loading
            || state.pretty_printer_error.is_some();
        state.runtime_pretty_printer_paths.clear();
        state.pretty_printer_script_loading = false;
        state.pretty_printers = Rc::new(Vec::new());
        state.pretty_printers_ready = false;
        state.pretty_printers_loading = false;
        state.pretty_printer_error = None;
        state.pretty_printer_generation = state.pretty_printer_generation.wrapping_add(1);
        drop(state);
        self.render_debug_data_printers();

        if reload_registry {
            self.dispatch_debug_data_action(DebugDataAction::LoadPrettyPrinters);
        }
    }

    pub(crate) fn add_debug_data_progress(&self, message: impl Into<String>) {
        self.record_debug_data_activity(DebugDataActivityKind::Progress, message);
    }

    pub(crate) fn add_debug_data_success(&self, message: impl Into<String>) {
        self.record_debug_data_activity(DebugDataActivityKind::Success, message);
    }

    pub(crate) fn add_debug_data_warning(&self, message: impl Into<String>) {
        self.record_debug_data_activity(DebugDataActivityKind::Warning, message);
    }

    pub(crate) fn add_debug_data_error(&self, message: impl Into<String>) {
        self.record_debug_data_activity(DebugDataActivityKind::Error, message);
    }

    pub(crate) fn record_performance_notice(&self, notice: crate::performance::PerformanceNotice) {
        const NOTICE_COOLDOWN: Duration = Duration::from_secs(5);
        let now = Instant::now();
        let key = format!("{:?}:{}", notice.outcome, notice.operation);
        let mut recent = self.performance_notice_times.borrow_mut();
        recent.retain(|_, recorded| now.saturating_duration_since(*recorded) < NOTICE_COOLDOWN);

        if recent
            .get(&key)
            .is_some_and(|recorded| now.saturating_duration_since(*recorded) < NOTICE_COOLDOWN)
        {
            return;
        }

        recent.insert(key, now);
        drop(recent);
        self.add_debug_data_warning(notice.message());
    }

    pub(crate) fn record_ui_render_duration(&self, operation: &str, started_at: Instant) {
        let elapsed = Instant::now().saturating_duration_since(started_at);

        let adjustment = self
            .adaptive_render_budgets
            .borrow_mut()
            .observe(operation, elapsed);

        if let Some(notice) = crate::performance::duration_notice(
            operation,
            elapsed,
            crate::performance::UI_RENDER_BUDGET,
        ) {
            self.record_performance_notice(notice);
        }

        if let Some(adjustment) = adjustment {
            self.record_performance_notice(crate::performance::PerformanceNotice {
                outcome: crate::performance::BudgetOutcome::Deferred,
                operation: operation.to_owned(),
                detail: format!(
                    "adaptive widget page changed from {} to {} entries after a {} ms render",
                    adjustment.previous,
                    adjustment.current,
                    elapsed.as_millis()
                ),
            });
        }
    }

    pub(crate) fn adaptive_render_limit(
        &self,
        operation: &str,
        default: usize,
        minimum: usize,
    ) -> usize {
        self.adaptive_render_budgets
            .borrow_mut()
            .limit(operation, default, minimum)
    }

    fn record_debug_data_activity(&self, kind: DebugDataActivityKind, message: impl Into<String>) {
        let mut message = message.into();

        if message.len() > MAX_DEBUG_DATA_ACTIVITY_BYTES {
            message.truncate(message.floor_char_boundary(MAX_DEBUG_DATA_ACTIVITY_BYTES));
            message.push_str("\n… output truncated in the activity view");
        }

        let time = debug_data_activity_time();
        let mut state = self.debug_data_state.borrow_mut();
        append_debug_data_activity(&mut state.activity, kind, message, time);
        drop(state);
        self.render_debug_data_activity();
    }

    pub(crate) fn show_more_debug_data_pretty_printers(&self) {
        let mut state = self.debug_data_state.borrow_mut();

        state.pretty_printer_limit = state
            .pretty_printer_limit
            .saturating_add(PRETTY_PRINTER_PAGE_SIZE);

        drop(state);
        self.render_debug_data_printers();
    }

    pub(crate) fn show_more_debug_data_modules(&self) {
        let mut state = self.debug_data_state.borrow_mut();

        state.module_limit = state
            .module_limit
            .saturating_add(DEBUG_DATA_RESULT_PAGE_SIZE);

        drop(state);
        self.render_debug_data_modules();
    }

    pub(super) fn reset_debug_data_module_paging(&self) {
        self.debug_data_state.borrow_mut().module_limit = DEBUG_DATA_RESULT_PAGE_SIZE;
    }

    pub(crate) fn show_more_debug_data_sources(&self) {
        let mut state = self.debug_data_state.borrow_mut();

        state.source_limit = state
            .source_limit
            .saturating_add(DEBUG_DATA_RESULT_PAGE_SIZE);

        drop(state);
        self.render_debug_data_sources();
    }

    fn reset_debug_data_module_limit(&self) {
        self.debug_data_state.borrow_mut().module_limit = DEBUG_DATA_RESULT_PAGE_SIZE;
        self.render_debug_data_modules();
    }

    fn reset_debug_data_source_limit(&self) {
        self.debug_data_state.borrow_mut().source_limit = DEBUG_DATA_RESULT_PAGE_SIZE;
        self.render_debug_data_sources();
    }

    fn reset_debug_data_pretty_printer_limit(&self) {
        self.debug_data_state.borrow_mut().pretty_printer_limit = PRETTY_PRINTER_PAGE_SIZE;
        self.render_debug_data_printers();
    }

    pub(crate) fn source_directories_for_debug_data(&self) -> Vec<String> {
        self.debug_data_state.borrow().source_directories.clone()
    }

    pub(crate) fn debuginfod_status_for_debug_data(&self) -> String {
        self.debug_data_state.borrow().debuginfod_status.clone()
    }

    pub(crate) fn add_runtime_source_directory(&self, path: PathBuf) {
        if !self.source_roots.borrow().contains(&path) {
            self.source_roots.borrow_mut().push(path.clone());
        }

        if !self.source_tree_roots.borrow().contains(&path) {
            self.source_tree_roots.borrow_mut().push(path);
        }

        self.invalidate_source_discovery();
    }

    pub(crate) fn remove_runtime_source_directory(&self, path: &str) {
        let path = Path::new(path);
        self.source_roots.borrow_mut().retain(|root| root != path);

        self.source_tree_roots
            .borrow_mut()
            .retain(|root| root != path);

        self.invalidate_source_discovery();
    }

    pub(crate) fn invalidate_source_discovery(&self) {
        self.resolved_source_paths.borrow_mut().clear();
        self.source_loaded_cache.borrow_mut().take();
        self.source_loaded_search.borrow_mut().take();
        self.source_tree_cache.borrow_mut().take();
        self.source_tree_search.borrow_mut().take();
        self.source_index.borrow_mut().take();
        self.source_tree.file_routes.borrow_mut().clear();
        self.source_tree_generation.fetch_add(1, Ordering::Relaxed);

        self.source_tree_render_generation
            .fetch_add(1, Ordering::Relaxed);

        // A worker for the old generation may still complete, but it must not
        // keep a new generation from starting its own index.
        self.source_tree_indexing.set(false);
    }

    pub(crate) fn cache_loaded_source_files(&self, files: &[SourceFile]) {
        let files_changed = self.loaded_source_files.borrow().as_slice() != files;

        if files_changed {
            self.loaded_source_files.replace(files.to_vec());
        }

        let state_changed = {
            let mut state = self.debug_data_state.borrow_mut();
            let changed = !state.source_files_ready || state.source_files_loading;
            state.source_files_ready = true;
            state.source_files_loading = false;
            state.source_files_error = None;

            if files_changed {
                state.source_limit = DEBUG_DATA_RESULT_PAGE_SIZE;
            }

            changed
        };

        if files_changed || state_changed {
            self.render_debug_data_overview();
            self.render_debug_data_sources();
        }
    }

    pub(super) fn begin_loaded_source_files_request(&self) -> bool {
        let mut state = self.debug_data_state.borrow_mut();

        if state.source_files_loading {
            return false;
        }

        state.source_files_loading = true;
        state.source_files_error = None;
        drop(state);
        self.render_debug_data_overview();
        self.render_debug_data_sources();

        true
    }

    pub(crate) fn fail_loaded_source_files_request(&self, generation: u64, message: String) {
        if !self.loaded_source_files_request_is_current(generation) {
            return;
        }

        let mut state = self.debug_data_state.borrow_mut();
        state.source_files_loading = false;
        state.source_files_error = Some(message.clone());
        drop(state);

        self.add_debug_data_warning(format!(
            "Loaded-source discovery is unavailable: {message}. Retry from the Sources tab"
        ));

        self.render_debug_data_overview();
        self.render_debug_data_sources();
    }

    pub(crate) fn loaded_source_files_request_is_current(&self, generation: u64) -> bool {
        self.source_loaded_generation.load(Ordering::Relaxed) == generation
    }

    pub(crate) fn show_debug_data_source_files(&self) -> bool {
        let mut state = self.debug_data_state.borrow_mut();
        let needs_load = !state.source_files_ready;

        if !state.source_files_visible {
            state.source_files_visible = true;
            drop(state);
            self.render_debug_data_sources();
        }

        needs_load
    }

    fn present_debug_data(self: &Rc<Self>) {
        if let Some(view) = self.debug_data_view.borrow().as_ref() {
            view.window.present();
            return;
        }

        let window = gtk::Window::builder()
            .title("fgdb debug data")
            .transient_for(&self.window)
            .modal(false)
            .hide_on_close(true)
            .default_width(980)
            .default_height(700)
            .build();

        window.add_css_class("debug-data-window");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_margin_top(10);
        root.set_margin_bottom(10);
        root.set_margin_start(10);
        root.set_margin_end(10);
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        heading.add_css_class("debug-data-heading");
        let title = gtk::Label::new(Some("Debug data"));
        title.add_css_class("title-2");
        title.add_css_class("debug-data-title");
        title.set_halign(gtk::Align::Start);
        title.set_hexpand(true);
        heading.append(&title);
        let refresh = gtk::Button::with_label("Refresh");
        refresh.add_css_class("inline-action");
        let weak_ui = Rc::downgrade(self);

        refresh.connect_clicked(move |button| {
            button.set_sensitive(false);

            if let Some(ui) = weak_ui.upgrade() {
                ui.dispatch_debug_data_action(DebugDataAction::Refresh);
            }
        });

        heading.append(&refresh);
        root.append(&heading);
        let notebook = gtk::Notebook::new();
        notebook.set_vexpand(true);
        let overview = debug_data_page();
        let module_search = debug_data_search("Filter module, path, build ID, or symbol state");
        let modules = debug_data_page_with_search(&module_search);
        let source_search = debug_data_search("Filter loaded source files");
        let sources = debug_data_page_with_search(&source_search);
        let printer_search = debug_data_search("Filter scope, provider, or printer name");
        let printers = debug_data_page_with_search(&printer_search);
        let printer_path = gtk::Entry::builder()
            .placeholder_text("/path/to/pretty-printer.py")
            .primary_icon_name("document-open-symbolic")
            .hexpand(true)
            .build();

        printer_path.add_css_class("debug-data-printer-path-input");
        let printer_browse = gtk::Button::with_label("Browse…");
        printer_browse.add_css_class("inline-action");
        let printer_load = gtk::Button::with_label("Load");
        printer_load.add_css_class("inline-action");
        printer_load.set_sensitive(false);
        let load_for_entry = printer_load.clone();

        printer_path.connect_changed(move |entry| {
            load_for_entry.set_sensitive(entry.is_sensitive() && !entry.text().trim().is_empty());
        });

        let activity = debug_data_page();
        append_debug_data_page(&notebook, &overview, "Overview");
        append_debug_data_page(&notebook, &modules, "Modules");
        append_debug_data_page(&notebook, &sources, "Sources");
        append_debug_data_page(&notebook, &printers, "Pretty Printers");
        append_debug_data_page(&notebook, &activity, "Activity");
        root.append(&notebook);
        window.set_child(Some(&root));

        self.debug_data_view.replace(Some(DebugDataView {
            window: window.clone(),
            refresh,
            overview,
            modules,
            sources,
            printers,
            activity,
            module_search: module_search.clone(),
            source_search: source_search.clone(),
            printer_search: printer_search.clone(),
            printer_path: printer_path.clone(),
            printer_browse: printer_browse.clone(),
            printer_load: printer_load.clone(),
        }));

        let handler = Rc::clone(&self.debug_data_action_handler);
        let path_for_load = printer_path.clone();

        printer_load.connect_clicked(move |_| {
            dispatch_pretty_printer_path(&path_for_load, &handler);
        });

        let handler = Rc::clone(&self.debug_data_action_handler);

        printer_path.connect_activate(move |entry| {
            dispatch_pretty_printer_path(entry, &handler);
        });

        let parent = window.clone();

        printer_browse.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::builder()
                .title("Load pretty-printer script")
                .modal(true)
                .build();

            let parent = parent.clone();
            let printer_path = printer_path.clone();

            glib::spawn_future_local(async move {
                let Ok(file) = dialog.open_future(Some(&parent)).await else {
                    return;
                };

                if let Some(path) = file.path() {
                    printer_path.set_text(&path.to_string_lossy());
                }
            });
        });

        connect_debug_data_search(
            &module_search,
            Rc::downgrade(self),
            Ui::reset_debug_data_module_limit,
        );

        connect_debug_data_search(
            &source_search,
            Rc::downgrade(self),
            Ui::reset_debug_data_source_limit,
        );

        connect_debug_data_search(
            &printer_search,
            Rc::downgrade(self),
            Ui::reset_debug_data_pretty_printer_limit,
        );

        let weak_ui = Rc::downgrade(self);

        notebook.connect_switch_page(move |_, page_widget, page| {
            clear_label_selections_after_switch(page_widget);

            if page == 3
                && let Some(ui) = weak_ui.upgrade()
                && !ui.debug_data_pretty_printers_were_requested()
            {
                ui.dispatch_debug_data_action(DebugDataAction::LoadPrettyPrinters);
            }
        });

        self.render_all_debug_data();
        window.present();
    }

    fn render_all_debug_data(&self) {
        self.render_debug_data_overview();
        self.render_debug_data_modules();
        self.render_debug_data_sources();
        self.render_debug_data_printers();
        self.render_debug_data_activity();
    }

    pub(super) fn render_debug_data_overview(&self) {
        let Some(view) = self.debug_data_view.borrow().as_ref().cloned() else {
            return;
        };

        clear_debug_data_box(&view.overview);

        let (refreshing, debuginfod_status, debuginfod_urls, sources_ready, sources_loading) = {
            let state = self.debug_data_state.borrow();

            (
                state.refreshing,
                state.debuginfod_status.clone(),
                state.debuginfod_urls.clone(),
                state.source_files_ready,
                state.source_files_loading,
            )
        };

        view.refresh.set_sensitive(!refreshing);
        view.modules.set_sensitive(!refreshing);
        view.sources.set_sensitive(!refreshing);
        view.printers.set_sensitive(!refreshing);
        let capabilities = self.model.gdb_capabilities();

        let loaded = self
            .latest_modules
            .borrow()
            .iter()
            .filter(|module| module.symbols_loaded)
            .count();

        let modules = self.latest_modules.borrow().len();

        view.overview.append(&debug_data_fact(
            "Debugger",
            &capabilities.version.as_deref().map_or_else(
                || String::from("GDB version unavailable"),
                |version| format!("GDB {version}"),
            ),
        ));

        view.overview.append(&debug_data_fact(
            "Capabilities",
            &capabilities.compatibility_summary(),
        ));

        view.overview.append(&debug_data_fact(
            "Modules",
            &format!(
                "{loaded} with symbols · {} missing",
                modules.saturating_sub(loaded)
            ),
        ));

        let loaded_sources = if sources_loading {
            String::from("Loading…")
        } else if sources_ready {
            self.loaded_source_files.borrow().len().to_string()
        } else {
            String::from("Not loaded")
        };

        view.overview
            .append(&debug_data_fact("Loaded sources", &loaded_sources));

        let status = if debuginfod_status.is_empty() {
            "Unknown"
        } else {
            debuginfod_status.as_str()
        };

        view.overview.append(&debug_data_fact("Debuginfod", status));

        if debuginfod_status == "ask" {
            view.overview.append(&muted_label(
                "Choose Enable or Disable before retrying symbols so GDB cannot block on a hidden confirmation prompt.",
            ));
        }

        let debuginfod_actions = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        debuginfod_actions.add_css_class("debug-data-control-card");

        for (label, enabled) in [("Enable", true), ("Disable", false)] {
            let button = gtk::Button::with_label(label);
            button.add_css_class("inline-action");
            button.set_sensitive(!refreshing);
            let handler = Rc::clone(&self.debug_data_action_handler);

            button.connect_clicked(move |button| {
                button.set_sensitive(false);
                defer_debug_data_action(&handler, DebugDataAction::SetDebuginfodEnabled(enabled));
            });

            debuginfod_actions.append(&button);
        }

        let urls = gtk::Entry::builder()
            .placeholder_text("DEBUGINFOD_URLS")
            .text(&debuginfod_urls)
            .hexpand(true)
            .build();

        debuginfod_actions.append(&urls);
        let apply_urls = gtk::Button::with_label("Apply URLs");
        apply_urls.add_css_class("inline-action");
        apply_urls.set_sensitive(!refreshing);
        let handler = Rc::clone(&self.debug_data_action_handler);

        apply_urls.connect_clicked(move |button| {
            button.set_sensitive(false);

            defer_debug_data_action(
                &handler,
                DebugDataAction::SetDebuginfodUrls(urls.text().trim().to_owned()),
            );
        });

        debuginfod_actions.append(&apply_urls);
        view.overview.append(&debuginfod_actions);
        let printer_actions = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        printer_actions.add_css_class("debug-data-control-card");

        let printer_label = gtk::Label::new(Some(if capabilities.pretty_printing {
            "Dynamic pretty printing is enabled"
        } else {
            "Dynamic pretty printing is disabled or unavailable"
        }));

        printer_label.set_halign(gtk::Align::Start);
        printer_label.set_hexpand(true);
        printer_actions.append(&printer_label);

        for (label, enabled) in [("Enable printers", true), ("Disable printers", false)] {
            let button = gtk::Button::with_label(label);
            button.add_css_class("inline-action");
            button.set_sensitive(!refreshing);
            let handler = Rc::clone(&self.debug_data_action_handler);

            button.connect_clicked(move |button| {
                button.set_sensitive(false);
                defer_debug_data_action(&handler, DebugDataAction::SetPrettyPrinting(enabled));
            });

            printer_actions.append(&button);
        }

        view.overview.append(&printer_actions);
        view.overview.append(&debug_data_section("GDB/MI FEATURES"));

        if capabilities.features.is_empty() {
            view.overview
                .append(&muted_label(if capabilities.features_known {
                    "GDB returned an empty feature list"
                } else {
                    "GDB did not expose a feature list"
                }));
        } else {
            view.overview
                .append(&wrapping_value(&capabilities.features.join("  ·  ")));
        }

        if refreshing {
            view.overview
                .prepend(&muted_label("Refreshing debugger diagnostics…"));
        }
    }

    pub(super) fn render_debug_data_modules(&self) {
        let Some(view) = self.debug_data_view.borrow().as_ref().cloned() else {
            return;
        };

        let render_started = Instant::now();
        clear_page_after_search(&view.modules);
        let query = view.module_search.text().trim().to_ascii_lowercase();
        let terms = query.split_whitespace().collect::<Vec<_>>();
        let metadata = self.module_debug_metadata.borrow();
        let modules = self.latest_modules.borrow();

        let (can_retry_symbols, render_limit) = {
            let debug_data = self.debug_data_state.borrow();

            (
                debug_data.debuginfod_status != "ask",
                debug_data.module_limit.max(DEBUG_DATA_RESULT_PAGE_SIZE),
            )
        };

        let mut shown = 0_usize;
        let mut matching = 0_usize;

        for module in modules.iter() {
            let path_text = module.host_name.as_deref().unwrap_or(&module.target_name);
            let path = Path::new(path_text);
            let details = metadata.get(path);

            let build_id = details
                .and_then(|details| details.build_id.as_deref())
                .unwrap_or("");

            let status = if module.symbols_loaded {
                "symbols loaded"
            } else {
                "missing symbols"
            };

            if !terms.iter().all(|term| {
                text_matches(&module.target_name, term)
                    || text_matches(path_text, term)
                    || text_matches(build_id, term)
                    || text_matches(status, term)
            }) {
                continue;
            }

            matching += 1;

            if shown >= render_limit {
                continue;
            }

            shown += 1;
            let row = gtk::Box::new(gtk::Orientation::Vertical, 3);
            row.add_css_class("debug-data-row");
            let heading = gtk::Box::new(gtk::Orientation::Horizontal, 7);
            let name = gtk::Label::new(path.file_name().and_then(|name| name.to_str()));
            name.add_css_class("module-name");
            name.set_halign(gtk::Align::Start);
            name.set_hexpand(true);
            heading.append(&name);

            let state = gtk::Label::new(Some(if module.symbols_loaded {
                "SYMBOLS"
            } else {
                "NO SYMBOLS"
            }));

            state.add_css_class(if module.symbols_loaded {
                "module-symbols-loaded"
            } else {
                "module-symbols-missing"
            });

            heading.append(&state);
            let retry = gtk::Button::with_label("Retry");
            retry.add_css_class("inline-action");
            retry.set_sensitive(can_retry_symbols);
            let handler = Rc::clone(&self.debug_data_action_handler);
            let target = module.target_name.clone();

            retry.connect_clicked(move |button| {
                button.set_sensitive(false);

                defer_debug_data_action(
                    &handler,
                    DebugDataAction::RetrySymbols(Some(target.clone())),
                );
            });

            heading.append(&retry);
            row.append(&heading);
            row.append(&selectable_value(&path.display().to_string()));

            if let Some(details) = details {
                if let Some(build_id) = details.build_id.as_deref() {
                    row.append(&debug_data_fact("Build ID", build_id));
                }

                if let Some(debuglink) = details.debuglink.as_deref() {
                    let value = details.debuglink_crc.map_or_else(
                        || debuglink.to_owned(),
                        |crc| format!("{debuglink} · CRC {crc:08x}"),
                    );

                    row.append(&debug_data_fact("Debuglink", &value));
                }

                let debug_file = details.separate_debug_file.as_ref().map_or_else(
                    || {
                        if details.embedded_debug_info {
                            String::from("Embedded in module")
                        } else {
                            String::from("Not found")
                        }
                    },
                    |path| path.display().to_string(),
                );

                row.append(&debug_data_fact("Debug file", &debug_file));

                if let Some(message) = details.error.as_deref().or(details.suggestion.as_deref()) {
                    row.append(&muted_label(message));
                }
            } else {
                row.append(&muted_label("Inspecting ELF metadata…"));
            }

            view.modules.append(&row);
        }

        if matching == 0 {
            view.modules.append(&muted_label(if modules.is_empty() {
                "Modules appear after an executable or core is loaded"
            } else {
                "No modules match the filter"
            }));
        }

        if shown < matching {
            let remaining = matching - shown;

            let show_more = gtk::Button::with_label(&format!(
                "Show {} more module{}",
                remaining.min(DEBUG_DATA_RESULT_PAGE_SIZE),
                if remaining == 1 { "" } else { "s" }
            ));

            show_more.add_css_class("inline-action");
            show_more.set_halign(gtk::Align::Center);
            let handler = Rc::clone(&self.debug_data_action_handler);

            show_more.connect_clicked(move |button| {
                button.set_sensitive(false);
                defer_debug_data_action(&handler, DebugDataAction::ShowMoreModules);
            });

            view.modules.append(&show_more);
        }

        if !modules.is_empty() {
            let retry_all = gtk::Button::with_label("Retry all missing symbols");
            retry_all.add_css_class("inline-action");
            retry_all.set_halign(gtk::Align::Start);
            retry_all.set_sensitive(can_retry_symbols);
            let handler = Rc::clone(&self.debug_data_action_handler);

            retry_all.connect_clicked(move |button| {
                button.set_sensitive(false);
                defer_debug_data_action(&handler, DebugDataAction::RetrySymbols(None));
            });

            view.modules.append(&retry_all);
        }

        self.record_ui_render_duration("Debug Data modules", render_started);
    }

    fn render_debug_data_sources(&self) {
        let Some(view) = self.debug_data_view.borrow().as_ref().cloned() else {
            return;
        };

        let render_started = Instant::now();
        clear_page_after_search(&view.sources);

        let (
            source_directories,
            substitutions,
            files_ready,
            files_loading,
            files_visible,
            source_error,
            render_limit,
        ) = {
            let state = self.debug_data_state.borrow();

            (
                state.source_directories.clone(),
                state.substitutions.clone(),
                state.source_files_ready,
                state.source_files_loading,
                state.source_files_visible,
                state.source_files_error.clone(),
                state.source_limit.max(DEBUG_DATA_RESULT_PAGE_SIZE),
            )
        };

        view.sources
            .append(&debug_data_section("SOURCE DIRECTORIES"));

        for directory in &source_directories {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
            row.add_css_class("debug-data-control-card");
            let value = selectable_value(directory);
            value.set_hexpand(true);
            row.append(&value);
            let remove = gtk::Button::with_label("Remove");
            remove.add_css_class("inline-action");
            let handler = Rc::clone(&self.debug_data_action_handler);
            let directory = directory.clone();
            remove.set_sensitive(!directory.starts_with('$'));

            remove.connect_clicked(move |button| {
                button.set_sensitive(false);

                defer_debug_data_action(
                    &handler,
                    DebugDataAction::RemoveSourceDirectory(directory.clone()),
                );
            });

            row.append(&remove);
            view.sources.append(&row);
        }

        let add_source = gtk::Button::with_label("Add source directory");
        add_source.add_css_class("inline-action");
        add_source.set_halign(gtk::Align::Start);
        let parent = view.window.clone();
        let handler = Rc::clone(&self.debug_data_action_handler);

        add_source.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::builder()
                .title("Add source directory")
                .modal(true)
                .build();

            let parent = parent.clone();
            let handler = Rc::clone(&handler);

            glib::spawn_future_local(async move {
                let Ok(folder) = dialog.select_folder_future(Some(&parent)).await else {
                    return;
                };

                let Some(path) = folder.path() else {
                    return;
                };

                defer_debug_data_action(&handler, DebugDataAction::AddSourceDirectory(path));
            });
        });

        view.sources.append(&add_source);
        view.sources.append(&debug_data_section("SUBSTITUTE PATH"));

        for (from, to) in &substitutions {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
            row.add_css_class("debug-data-control-card");
            let mapping = selectable_value(&format!("{from}  →  {to}"));
            mapping.set_hexpand(true);
            row.append(&mapping);
            let remove = gtk::Button::with_label("Remove");
            remove.add_css_class("inline-action");
            let handler = Rc::clone(&self.debug_data_action_handler);
            let from = from.clone();

            remove.connect_clicked(move |button| {
                button.set_sensitive(false);

                defer_debug_data_action(
                    &handler,
                    DebugDataAction::RemoveSubstitution(from.clone()),
                );
            });

            row.append(&remove);
            view.sources.append(&row);
        }

        let substitution = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        substitution.add_css_class("debug-data-control-card");

        let from = gtk::Entry::builder()
            .placeholder_text("Compiled path")
            .hexpand(true)
            .build();

        let to = gtk::Entry::builder()
            .placeholder_text("Local path")
            .hexpand(true)
            .build();

        substitution.append(&from);
        substitution.append(&to);
        let add = gtk::Button::with_label("Add");
        add.add_css_class("inline-action");
        let handler = Rc::clone(&self.debug_data_action_handler);

        add.connect_clicked(move |button| {
            let from_value = from.text().trim().to_owned();
            let to_value = to.text().trim().to_owned();

            if !from_value.is_empty() && !to_value.is_empty() {
                button.set_sensitive(false);

                defer_debug_data_action(
                    &handler,
                    DebugDataAction::AddSubstitution {
                        from: from_value,
                        to: to_value,
                    },
                );
            }
        });

        substitution.append(&add);
        view.sources.append(&substitution);
        view.sources.append(&debug_data_section("LOADED SOURCES"));

        view.source_search
            .set_sensitive(files_ready && files_visible && !files_loading);

        view.source_search
            .set_placeholder_text(Some(if files_ready && files_visible {
                "Filter loaded source files"
            } else {
                "Show source files to search"
            }));

        let source_actions = gtk::Box::new(gtk::Orientation::Horizontal, 7);
        source_actions.add_css_class("debug-data-control-card");
        let source_count = self.loaded_source_files.borrow().len();

        let source_status = if files_loading {
            String::from("Asking GDB for its source-file list…")
        } else if let Some(error) = source_error.as_deref() {
            if files_ready {
                format!("Refresh failed. Showing the previous source-file list: {error}")
            } else {
                format!("Source-file list unavailable: {error}")
            }
        } else if files_ready && files_visible {
            format!(
                "{} source file{} loaded",
                source_count,
                if source_count == 1 { "" } else { "s" }
            )
        } else if files_ready {
            format!("{source_count} source files available. Hidden until requested")
        } else {
            String::from("Source files are loaded only when requested")
        };

        let source_status = muted_label(&source_status);
        source_status.set_hexpand(true);
        source_actions.append(&source_status);

        let load_sources = gtk::Button::with_label(if files_loading {
            "Loading source files…"
        } else if source_error.is_some() {
            "Retry source files"
        } else if files_ready && files_visible {
            "Reload source files"
        } else if files_ready {
            "Show source files"
        } else {
            "Load source files"
        });

        load_sources.add_css_class("inline-action");
        load_sources.set_sensitive(!files_loading);
        let handler = Rc::clone(&self.debug_data_action_handler);

        let action = if files_ready && files_visible {
            DebugDataAction::ReloadSourceFiles
        } else {
            DebugDataAction::ShowSourceFiles
        };

        load_sources.connect_clicked(move |button| {
            button.set_sensitive(false);
            defer_debug_data_action(&handler, action.clone());
        });

        source_actions.append(&load_sources);
        view.sources.append(&source_actions);

        if !files_ready || !files_visible {
            self.record_ui_render_duration("Debug Data sources", render_started);
            return;
        }

        let query = view.source_search.text().trim().to_ascii_lowercase();
        let files = self.loaded_source_files.borrow();
        let mut shown = 0_usize;
        let mut matching = 0_usize;

        for file in files.iter() {
            let path = file.source_path();

            if !text_matches(path, &query) {
                continue;
            }

            matching += 1;

            if shown >= render_limit {
                continue;
            }

            shown += 1;
            let source = selectable_value(path);
            source.add_css_class("debug-data-source");
            view.sources.append(&source);
        }

        if matching == 0 {
            view.sources.append(&muted_label(if files.is_empty() {
                "No source files have been reported by GDB"
            } else {
                "No loaded source files match the filter"
            }));
        }

        if shown < matching {
            let remaining = matching - shown;

            let show_more = gtk::Button::with_label(&format!(
                "Show {} more source{}",
                remaining.min(DEBUG_DATA_RESULT_PAGE_SIZE),
                if remaining == 1 { "" } else { "s" }
            ));

            show_more.add_css_class("inline-action");
            show_more.set_halign(gtk::Align::Center);
            let handler = Rc::clone(&self.debug_data_action_handler);

            show_more.connect_clicked(move |button| {
                button.set_sensitive(false);
                defer_debug_data_action(&handler, DebugDataAction::ShowMoreSources);
            });

            view.sources.append(&show_more);
        }

        self.record_ui_render_duration("Debug Data sources", render_started);
    }

    fn render_debug_data_printers(&self) {
        let Some(view) = self.debug_data_view.borrow().as_ref().cloned() else {
            return;
        };

        let render_started = Instant::now();
        clear_page_after_search(&view.printers);

        let (
            scopes,
            render_limit,
            ready,
            loading,
            error,
            gcc_directory,
            configured_paths,
            runtime_paths,
            script_loading,
            safe_mode,
        ) = {
            let state = self.debug_data_state.borrow();

            (
                Rc::clone(&state.pretty_printers),
                state.pretty_printer_limit.max(PRETTY_PRINTER_PAGE_SIZE),
                state.pretty_printers_ready,
                state.pretty_printers_loading,
                state.pretty_printer_error.clone(),
                state.gcc_pretty_printer_directory.clone(),
                state.configured_pretty_printer_paths.clone(),
                state.runtime_pretty_printer_paths.clone(),
                state.pretty_printer_script_loading,
                state.safe_mode,
            )
        };

        view.printer_search.set_sensitive(ready && !loading);
        let printer_supported = self.model.gdb_capabilities().pretty_printing;
        view.printer_path
            .set_sensitive(printer_supported && !script_loading && !loading);
        view.printer_browse
            .set_sensitive(printer_supported && !script_loading && !loading);
        view.printer_load.set_sensitive(
            printer_supported
                && !script_loading
                && !loading
                && !view.printer_path.text().trim().is_empty(),
        );

        view.printers.append(&pretty_printer_loader_panel(
            &view,
            &scopes,
            gcc_directory.as_deref(),
            &configured_paths,
            &runtime_paths,
            script_loading,
            safe_mode,
            printer_supported,
        ));

        if loading {
            view.printers.append(&muted_label(if scopes.is_empty() {
                "Loading pretty-printers from GDB…"
            } else {
                "Refreshing pretty-printers from GDB…"
            }));
        }

        if let Some(error) = error.as_deref() {
            let warning = wrapping_value(error);
            warning.add_css_class("warning");
            view.printers.append(&warning);
            let retry = gtk::Button::with_label("Retry loading pretty-printers");
            retry.add_css_class("inline-action");
            retry.set_halign(gtk::Align::Start);
            retry.set_sensitive(!loading);
            let handler = Rc::clone(&self.debug_data_action_handler);

            retry.connect_clicked(move |button| {
                button.set_sensitive(false);
                defer_debug_data_action(&handler, DebugDataAction::LoadPrettyPrinters);
            });

            view.printers.append(&retry);
        }

        if !ready && !loading {
            if scopes.is_empty() && error.is_none() {
                view.printers.append(&muted_label(
                    "Pretty-printers are loaded when this tab is opened",
                ));
            }

            self.record_ui_render_duration("Debug Data pretty-printers", render_started);
            return;
        }

        if scopes.is_empty() {
            view.printers
                .append(&muted_label("No pretty-printers were reported by GDB"));

            self.record_ui_render_duration("Debug Data pretty-printers", render_started);
            return;
        }

        let query = view.printer_search.text().trim().to_ascii_lowercase();
        let total_printers = scopes.iter().map(pretty_printer_count).sum::<usize>();
        let summary = gtk::Box::new(gtk::Orientation::Horizontal, 7);
        summary.add_css_class("debug-data-printer-summary");

        let summary_text = gtk::Label::new(Some(&format!(
            "{total_printers} registered printer{}",
            if total_printers == 1 { "" } else { "s" }
        )));

        summary_text.add_css_class("debug-data-printer-summary-count");
        summary_text.set_halign(gtk::Align::Start);
        summary.append(&summary_text);

        let scope_count = muted_label(&format!(
            "across {} scope{}",
            scopes.len(),
            if scopes.len() == 1 { "" } else { "s" }
        ));

        scope_count.set_hexpand(true);
        summary.append(&scope_count);
        view.printers.append(&summary);
        let mut remaining = render_limit;
        let mut matching_printers = 0_usize;
        let mut matching_scopes = 0_usize;
        let mut shown_scopes = Vec::new();

        for scope in scopes.iter() {
            let (filtered, matches) = filter_pretty_printer_scope(scope, &query, &mut remaining);
            matching_printers += matches;
            matching_scopes += usize::from(matches > 0);

            if let Some(filtered) = filtered {
                shown_scopes.push((filtered, matches));
            }
        }

        let shown_printers = shown_scopes
            .iter()
            .map(|(scope, _)| pretty_printer_count(scope))
            .sum::<usize>();

        for (scope, matching_in_scope) in &shown_scopes {
            view.printers.append(&pretty_printer_scope_card(
                &scope.name,
                &scope.direct_printers,
                &scope.providers,
                pretty_printer_count(scope),
                *matching_in_scope,
            ));
        }

        if matching_printers == 0 {
            view.printers
                .append(&muted_label("No pretty-printers match the filter"));
        } else if !query.is_empty() || shown_printers < matching_printers {
            let matches = muted_label(&format!(
                "Showing {shown_printers} of {matching_printers} matching printer{} in {} scope{}",
                if shown_printers == 1 { "" } else { "s" },
                matching_scopes,
                if matching_scopes == 1 { "" } else { "s" }
            ));

            matches.add_css_class("debug-data-printer-match-count");
            view.printers.insert_child_after(&matches, Some(&summary));
        }

        if shown_printers < matching_printers {
            let remaining = matching_printers - shown_printers;

            let show_more = gtk::Button::with_label(&format!(
                "Show {} more printer{}",
                remaining.min(PRETTY_PRINTER_PAGE_SIZE),
                if remaining == 1 { "" } else { "s" }
            ));

            show_more.add_css_class("inline-action");
            show_more.set_halign(gtk::Align::Center);
            let handler = Rc::clone(&self.debug_data_action_handler);

            show_more.connect_clicked(move |button| {
                button.set_sensitive(false);
                defer_debug_data_action(&handler, DebugDataAction::ShowMorePrettyPrinters);
            });

            view.printers.append(&show_more);
        }

        self.record_ui_render_duration("Debug Data pretty-printers", render_started);
    }

    fn render_debug_data_activity(&self) {
        let Some(view) = self.debug_data_view.borrow().as_ref().cloned() else {
            return;
        };

        clear_debug_data_box(&view.activity);
        let activity = self.debug_data_state.borrow().activity.clone();

        view.activity
            .append(&debug_data_activity_summary(&activity));

        if activity.is_empty() {
            let empty = muted_label("Symbol downloads and diagnostic errors appear here");
            empty.add_css_class("debug-data-activity-empty");
            view.activity.append(&empty);
            return;
        }

        let feed = gtk::Box::new(gtk::Orientation::Vertical, 0);
        feed.add_css_class("debug-data-activity-feed");

        for event in activity.iter().rev() {
            feed.append(&debug_data_activity_row(event));
        }

        view.activity.append(&feed);
    }

    pub(crate) fn refresh_module_debug_metadata(self: &Rc<Self>, force: bool) {
        let generation = self
            .module_debug_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);

        self.module_debug_force_pending
            .fetch_or(force, Ordering::Release);

        let mut seen = HashSet::new();

        let paths = self
            .latest_modules
            .borrow()
            .iter()
            .map(|module| PathBuf::from(module.host_name.as_deref().unwrap_or(&module.target_name)))
            .filter(|path| seen.insert(path.clone()))
            .collect::<Vec<_>>();

        self.refresh_module_debug_metadata_batch(Arc::new(paths), generation, force, 0);
    }

    fn refresh_module_debug_metadata_batch(
        self: &Rc<Self>,
        paths: Arc<Vec<PathBuf>>,
        generation: u64,
        force: bool,
        offset: usize,
    ) {
        if paths.is_empty() {
            self.module_debug_force_pending
                .store(false, Ordering::Release);

            if !self.module_debug_metadata.borrow().is_empty() {
                self.module_debug_metadata.borrow_mut().clear();
                self.render_debug_data_modules();
            }

            return;
        }

        if self.module_debug_worker_active.swap(true, Ordering::AcqRel) {
            return;
        }

        let force = self
            .module_debug_force_pending
            .swap(false, Ordering::AcqRel)
            || force;

        if offset == 0 {
            let live_paths = paths.iter().collect::<HashSet<_>>();
            self.module_debug_metadata
                .borrow_mut()
                .retain(|path, _| live_paths.contains(path));
        }

        let current_generation = Arc::clone(&self.module_debug_generation);
        let total_paths = paths.len().saturating_sub(offset);
        let scan_paths = Arc::clone(&paths);

        // The cursor belongs to this generation, including forced scans of
        // already cached modules. Cache presence is not a progress marker.
        let paths = paths
            .iter()
            .skip(offset)
            .take(crate::performance::MODULE_METADATA_FILE_BUDGET)
            .cloned()
            .collect::<Vec<_>>();

        let cached = {
            let cache = self.module_debug_metadata.borrow();
            paths
                .iter()
                .filter_map(|path| cache.get(path).map(|value| (path.clone(), value.clone())))
                .collect::<HashMap<_, _>>()
        };

        let (sender, receiver) = mpsc::channel();
        let queued_generation = Arc::clone(&current_generation);

        if let Err(error) = crate::background::submit_cancellable_with_priority(
            crate::background::Priority::Background,
            move || queued_generation.load(Ordering::Relaxed) == generation,
            move || {
                let mut metadata = HashMap::with_capacity(paths.len());
                let started_at = Instant::now();
                let mut time_budget_exhausted = false;
                let is_current = || current_generation.load(Ordering::Relaxed) == generation;

                for path in paths {
                    if current_generation.load(Ordering::Relaxed) != generation {
                        return;
                    }

                    if !metadata.is_empty()
                        && started_at.elapsed() >= crate::performance::MODULE_METADATA_TIME_BUDGET
                    {
                        time_budget_exhausted = true;
                        break;
                    }

                    let file = std::fs::metadata(&path).ok();

                    let unchanged = file.as_ref().and_then(|file| {
                        let modified = file.modified().ok();

                        cached.get(&path).filter(|cached| {
                            modified.is_some()
                                && cached.file_size == Some(file.len())
                                && cached.modified == modified
                        })
                    });

                    let details = match unchanged {
                        Some(cached) if force && cached.error.is_some() => {
                            crate::debug_info::inspect_module_while(&path, &is_current)
                        }
                        Some(cached) if force => {
                            crate::debug_info::refresh_module_debug_file_while(
                                cached.clone(),
                                &is_current,
                            )
                        }
                        Some(cached) => cached.clone(),
                        None => crate::debug_info::inspect_module_while(&path, &is_current),
                    };

                    metadata.insert(path, details);
                }

                if current_generation.load(Ordering::Relaxed) == generation {
                    let _ = sender.send((metadata, total_paths, time_budget_exhausted));
                }
            },
        ) {
            self.module_debug_worker_active
                .store(false, Ordering::Release);

            self.module_debug_force_pending
                .fetch_or(force, Ordering::Release);

            self.record_performance_notice(crate::performance::PerformanceNotice {
                outcome: crate::performance::BudgetOutcome::Rejected,
                operation: String::from("module debug metadata"),
                detail: error.to_string(),
            });

            if error == crate::background::SubmitError::QueueFull {
                let weak = Rc::downgrade(self);
                glib::timeout_add_local_once(Duration::from_millis(100), move || {
                    if let Some(ui) = weak.upgrade()
                        && ui.module_debug_generation.load(Ordering::Relaxed) == generation
                    {
                        ui.refresh_module_debug_metadata_batch(
                            scan_paths, generation, force, offset,
                        );
                    }
                });
            }

            return;
        }

        let weak_ui = Rc::downgrade(self);

        glib::timeout_add_local(Duration::from_millis(25), move || {
            let Some(ui) = weak_ui.upgrade() else {
                return glib::ControlFlow::Break;
            };

            match receiver.try_recv() {
                Ok((metadata, total_paths, time_budget_exhausted)) => {
                    ui.module_debug_worker_active
                        .store(false, Ordering::Release);

                    if ui.module_debug_generation.load(Ordering::Relaxed) == generation {
                        let inspected = metadata.len();

                        let changed = metadata.iter().any(|(path, details)| {
                            ui.module_debug_metadata.borrow().get(path) != Some(details)
                        });

                        if changed {
                            ui.module_debug_metadata.borrow_mut().extend(metadata);
                            ui.render_debug_data_modules();
                        }

                        if inspected < total_paths {
                            ui.record_performance_notice(
                                crate::performance::PerformanceNotice {
                                    outcome: if time_budget_exhausted {
                                        crate::performance::BudgetOutcome::Deferred
                                    } else {
                                        crate::performance::BudgetOutcome::Partial
                                    },
                                    operation: String::from("module debug metadata"),
                                    detail: if time_budget_exhausted {
                                        format!(
                                            "inspected {inspected} of {total_paths} pending modules before the time budget. The scan will continue in the background"
                                        )
                                    } else {
                                        format!(
                                            "inspected {inspected} of {total_paths} pending modules. The next bounded batch will continue automatically"
                                        )
                                    },
                                },
                            );
                        }

                        if inspected > 0 && inspected < total_paths {
                            ui.refresh_module_debug_metadata_batch(
                                Arc::clone(&scan_paths),
                                generation,
                                force,
                                offset + inspected,
                            );
                            return glib::ControlFlow::Break;
                        }
                    }

                    if ui.module_debug_generation.load(Ordering::Relaxed) != generation {
                        ui.refresh_module_debug_metadata(force);
                    }

                    glib::ControlFlow::Break
                }
                Err(TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(TryRecvError::Disconnected) => {
                    ui.module_debug_worker_active
                        .store(false, Ordering::Release);

                    if ui.module_debug_generation.load(Ordering::Relaxed) != generation {
                        ui.refresh_module_debug_metadata(force);
                    }

                    glib::ControlFlow::Break
                }
            }
        });
    }
}

fn parse_pretty_printer_scopes(lines: &[String]) -> Vec<PrettyPrinterScope> {
    let entries = lines
        .iter()
        .filter_map(|line| {
            let name = line.trim();

            (!name.is_empty()).then(|| (line.len() - line.trim_start().len(), name.to_owned()))
        })
        .collect::<Vec<_>>();

    let Some(base_indent) = entries.iter().map(|(indent, _)| *indent).min() else {
        return Vec::new();
    };

    let mut scopes = Vec::<PrettyPrinterScope>::new();
    let mut index = 0_usize;

    while index < entries.len() {
        let (indent, name) = &entries[index];

        let has_children = entries
            .get(index + 1)
            .is_some_and(|(next_indent, _)| next_indent > indent);

        if *indent == base_indent && name.ends_with(':') && has_children {
            scopes.push(PrettyPrinterScope {
                name: name.clone(),
                direct_printers: Vec::new(),
                providers: Vec::new(),
            });

            index += 1;
            continue;
        }

        if scopes.is_empty() {
            scopes.push(PrettyPrinterScope {
                name: String::from("GDB pretty-printers"),
                direct_printers: Vec::new(),
                providers: Vec::new(),
            });
        }

        if *indent == base_indent {
            scopes
                .last_mut()
                .expect("a fallback printer scope was created")
                .direct_printers
                .push(name.clone());

            index += 1;
            continue;
        }

        let scope = scopes.last_mut().expect("a printer scope was created");

        if !has_children {
            scope.direct_printers.push(name.clone());
            index += 1;
            continue;
        }

        let provider_indent = *indent;
        let provider_name = name.clone();
        index += 1;
        let children_begin = index;

        while index < entries.len() && entries[index].0 > provider_indent {
            index += 1;
        }

        let children = &entries[children_begin..index];

        let printers = children
            .iter()
            .enumerate()
            .filter(|(child_index, (child_indent, _))| {
                children
                    .get(child_index + 1)
                    .is_none_or(|(next_indent, _)| next_indent <= child_indent)
            })
            .map(|(_, (_, child_name))| child_name.clone())
            .collect::<Vec<_>>();

        if printers.is_empty() {
            scope.direct_printers.push(provider_name);
        } else {
            scope.providers.push(PrettyPrinterProvider {
                name: provider_name,
                printers,
            });
        }
    }

    scopes.retain(|scope| pretty_printer_count(scope) > 0);

    scopes
}

fn pretty_printer_count(scope: &PrettyPrinterScope) -> usize {
    scope.direct_printers.len()
        + scope
            .providers
            .iter()
            .map(|provider| provider.printers.len())
            .sum::<usize>()
}

fn text_matches(value: &str, lowercase_query: &str) -> bool {
    lowercase_query.is_empty()
        || value
            .as_bytes()
            .windows(lowercase_query.len())
            .any(|window| window.eq_ignore_ascii_case(lowercase_query.as_bytes()))
}

fn filter_pretty_printer_scope(
    scope: &PrettyPrinterScope,
    lowercase_query: &str,
    remaining: &mut usize,
) -> (Option<PrettyPrinterScope>, usize) {
    let scope_matches = lowercase_query.is_empty() || text_matches(&scope.name, lowercase_query);
    let mut matching = 0_usize;
    let mut direct_printers = Vec::new();

    for printer in &scope.direct_printers {
        if scope_matches || text_matches(printer, lowercase_query) {
            matching += 1;

            if *remaining > 0 {
                direct_printers.push(printer.clone());
                *remaining -= 1;
            }
        }
    }

    let mut providers = Vec::new();

    for provider in &scope.providers {
        let provider_matches = scope_matches || text_matches(&provider.name, lowercase_query);
        let mut printers = Vec::new();

        for printer in &provider.printers {
            if provider_matches || text_matches(printer, lowercase_query) {
                matching += 1;

                if *remaining > 0 {
                    printers.push(printer.clone());
                    *remaining -= 1;
                }
            }
        }

        if !printers.is_empty() {
            providers.push(PrettyPrinterProvider {
                name: provider.name.clone(),
                printers,
            });
        }
    }

    let visible = !direct_printers.is_empty() || !providers.is_empty();

    (
        visible.then(|| PrettyPrinterScope {
            name: scope.name.clone(),
            direct_printers,
            providers,
        }),
        matching,
    )
}

#[allow(clippy::too_many_arguments)]
fn pretty_printer_loader_panel(
    view: &DebugDataView,
    scopes: &[PrettyPrinterScope],
    gcc_directory: Option<&Path>,
    configured_paths: &[PathBuf],
    runtime_paths: &[PathBuf],
    loading: bool,
    safe_mode: bool,
    printer_supported: bool,
) -> gtk::Box {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 6);
    panel.add_css_class("debug-data-printer-loaders");
    let heading = gtk::Label::new(Some("PRINTER LOADERS"));
    heading.add_css_class("debug-data-printer-loader-heading");
    heading.set_halign(gtk::Align::Start);
    panel.append(&heading);

    let gcc_registered = pretty_printer_registry_contains(scopes, "libstdc++");
    let (gcc_path, gcc_status, gcc_status_class) = if safe_mode {
        (
            String::from("Automatic discovery is disabled in safe mode"),
            "DISABLED",
            "loader-disabled",
        )
    } else if let Some(directory) = gcc_directory {
        (
            directory.display().to_string(),
            if gcc_registered {
                "LOADED"
            } else {
                "DISCOVERED"
            },
            if gcc_registered {
                "loader-loaded"
            } else {
                "loader-discovered"
            },
        )
    } else {
        (
            String::from("No compiler-matched installation was found"),
            "NOT FOUND",
            "loader-missing",
        )
    };

    panel.append(&pretty_printer_loader_row(
        "GCC C++ stdcxx",
        &gcc_path,
        gcc_status,
        gcc_status_class,
    ));

    for path in configured_paths {
        panel.append(&pretty_printer_loader_row(
            "Configured script",
            &path.display().to_string(),
            if safe_mode { "DISABLED" } else { "STARTUP" },
            if safe_mode {
                "loader-disabled"
            } else {
                "loader-discovered"
            },
        ));
    }

    for path in runtime_paths {
        panel.append(&pretty_printer_loader_row(
            "Session script",
            &path.display().to_string(),
            "LOADED",
            "loader-loaded",
        ));
    }

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    controls.add_css_class("debug-data-printer-loader-controls");
    controls.append(&view.printer_path);
    controls.append(&view.printer_browse);
    controls.append(&view.printer_load);
    panel.append(&controls);
    let note = wrapping_value(if !printer_supported {
        "This GDB does not expose dynamic pretty printing"
    } else if loading {
        "Loading the selected script inside GDB…"
    } else {
        "Scripts execute inside GDB for this session. Add pretty_printer_path to the fgdb configuration to load a script at startup"
    });

    note.add_css_class("debug-data-printer-loader-note");
    panel.append(&note);

    panel
}

fn pretty_printer_loader_row(name: &str, path: &str, status: &str, status_class: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("debug-data-printer-loader-row");
    let identity = gtk::Box::new(gtk::Orientation::Vertical, 2);
    identity.set_hexpand(true);
    let name = gtk::Label::new(Some(name));
    name.add_css_class("debug-data-printer-loader-name");
    name.set_halign(gtk::Align::Start);
    identity.append(&name);
    let path = selectable_value(path);
    path.add_css_class("debug-data-printer-loader-path");
    path.set_ellipsize(pango::EllipsizeMode::Middle);
    path.set_tooltip_text(Some(path.text().as_str()));
    identity.append(&path);
    row.append(&identity);
    let status = gtk::Label::new(Some(status));
    status.add_css_class("debug-data-printer-loader-status");
    status.add_css_class(status_class);
    status.set_valign(gtk::Align::Center);
    row.append(&status);

    row
}

fn pretty_printer_registry_contains(scopes: &[PrettyPrinterScope], needle: &str) -> bool {
    scopes.iter().any(|scope| {
        text_matches(&scope.name, needle)
            || scope
                .direct_printers
                .iter()
                .any(|printer| text_matches(printer, needle))
            || scope.providers.iter().any(|provider| {
                text_matches(&provider.name, needle)
                    || provider
                        .printers
                        .iter()
                        .any(|printer| text_matches(printer, needle))
            })
    })
}

fn pretty_printer_scope_card(
    scope: &str,
    direct_printers: &[String],
    providers: &[PrettyPrinterProvider],
    visible_printers: usize,
    matching_printers: usize,
) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    card.add_css_class("debug-data-printer-scope");
    let (kind, title, path) = pretty_printer_scope_identity(scope);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 7);
    header.add_css_class("debug-data-printer-scope-header");
    let kind = gtk::Label::new(Some(kind));
    kind.add_css_class("debug-data-printer-kind");
    header.append(&kind);
    let title = gtk::Label::new(Some(&title));
    title.add_css_class("debug-data-printer-scope-name");
    title.set_halign(gtk::Align::Start);
    title.set_hexpand(true);
    title.set_ellipsize(pango::EllipsizeMode::Middle);
    header.append(&title);

    let count_text = if visible_printers < matching_printers {
        format!("{visible_printers} of {matching_printers} printers")
    } else {
        format!(
            "{visible_printers} printer{}",
            if visible_printers == 1 { "" } else { "s" }
        )
    };

    let count = gtk::Label::new(Some(&count_text));
    count.add_css_class("debug-data-printer-count");
    header.append(&count);
    card.append(&header);

    if let Some(path) = path {
        let path_label = selectable_value(&path);
        path_label.add_css_class("debug-data-printer-path");
        path_label.set_halign(gtk::Align::Fill);
        path_label.set_hexpand(true);
        card.append(&path_label);
    }

    if !direct_printers.is_empty() {
        card.append(&pretty_printer_grid(direct_printers));
    }

    for provider in providers {
        let provider_header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        provider_header.add_css_class("debug-data-printer-provider");
        let name = gtk::Label::new(Some(&provider.name));
        name.add_css_class("debug-data-printer-provider-name");
        name.set_halign(gtk::Align::Start);
        name.set_hexpand(true);
        provider_header.append(&name);
        let count = gtk::Label::new(Some(&provider.printers.len().to_string()));
        count.add_css_class("debug-data-printer-count");
        provider_header.append(&count);
        card.append(&provider_header);
        card.append(&pretty_printer_grid(&provider.printers));
    }

    card
}

fn pretty_printer_scope_identity(scope: &str) -> (&'static str, String, Option<String>) {
    let scope = scope.trim_end_matches(':');

    if scope.eq_ignore_ascii_case("global pretty-printers") {
        return ("GLOBAL", String::from("Global"), None);
    }

    if let Some(path) = scope
        .strip_prefix("objfile ")
        .and_then(|scope| scope.strip_suffix(" pretty-printers"))
    {
        let title = Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(path)
            .to_owned();

        return ("OBJFILE", title, Some(path.to_owned()));
    }

    ("SCOPE", scope.to_owned(), None)
}

fn pretty_printer_grid(printers: &[String]) -> gtk::Grid {
    let grid = gtk::Grid::builder()
        .column_homogeneous(true)
        .column_spacing(5)
        .row_spacing(4)
        .build();

    grid.add_css_class("debug-data-printer-grid");
    let columns = printers.len().clamp(1, 3);

    for (index, printer) in printers.iter().enumerate() {
        let label = selectable_value(printer);
        label.add_css_class("debug-data-printer");
        label.set_halign(gtk::Align::Fill);
        label.set_hexpand(true);
        label.set_max_width_chars(32);

        grid.attach(
            &label,
            (index % columns) as i32,
            (index / columns) as i32,
            1,
            1,
        );
    }

    grid
}

fn debug_data_activity_time() -> String {
    glib::DateTime::now_local()
        .and_then(|time| time.format("%H:%M:%S"))
        .map(|time| time.to_string())
        .unwrap_or_else(|_| String::from("--:--:--"))
}

fn append_debug_data_activity(
    activity: &mut Vec<DebugDataActivity>,
    kind: DebugDataActivityKind,
    message: String,
    time: String,
) {
    if let Some(last) = activity
        .last_mut()
        .filter(|last| last.kind == kind && last.message.as_ref() == message.as_str())
    {
        last.time = time;
        last.occurrences = last.occurrences.saturating_add(1);
        return;
    }

    activity.push(DebugDataActivity {
        kind,
        message: Rc::from(message),
        time,
        occurrences: 1,
    });

    if activity.len() > MAX_DEBUG_DATA_ACTIVITY_EVENTS {
        let excess = activity.len() - MAX_DEBUG_DATA_ACTIVITY_EVENTS;
        activity.drain(..excess);
    }
}

fn debug_data_activity_summary(activity: &[DebugDataActivity]) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("debug-data-activity-summary");
    let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
    labels.set_hexpand(true);
    labels.set_valign(gtk::Align::Center);
    let title = gtk::Label::new(Some("RECENT ACTIVITY"));
    title.set_halign(gtk::Align::Start);
    title.set_xalign(0.0);
    title.add_css_class("debug-data-activity-summary-title");

    let issue_count = activity
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                DebugDataActivityKind::Warning | DebugDataActivityKind::Error
            )
        })
        .count();

    let detail = match (activity.len(), issue_count) {
        (0, _) => String::from("No activity has been recorded for this session"),
        (events, 0) => format!("{events} events · no warnings or errors"),
        (events, 1) => format!("{events} events · 1 warning or error"),
        (events, issues) => format!("{events} events · {issues} warnings or errors"),
    };

    let detail = gtk::Label::new(Some(&detail));
    detail.set_halign(gtk::Align::Start);
    detail.set_xalign(0.0);
    detail.add_css_class("debug-data-activity-summary-detail");
    labels.append(&title);
    labels.append(&detail);
    row.append(&labels);
    let order = gtk::Label::new(Some("NEWEST FIRST"));
    order.set_halign(gtk::Align::End);
    order.set_valign(gtk::Align::Center);
    order.add_css_class("debug-data-activity-order");
    row.append(&order);

    row
}

fn debug_data_activity_row(event: &DebugDataActivity) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    row.add_css_class("debug-data-activity-row");
    row.add_css_class(event.kind.css_class());
    let badge = gtk::Label::new(Some(event.kind.label()));
    badge.set_halign(gtk::Align::Start);
    badge.set_valign(gtk::Align::Center);
    badge.add_css_class("debug-data-activity-kind");
    row.append(&badge);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 3);
    content.set_hexpand(true);
    content.set_valign(gtk::Align::Center);

    let (headline, detail) = event.message.split_once('\n').map_or_else(
        || (event.message.as_ref(), None),
        |(headline, detail)| (headline, (!detail.is_empty()).then_some(detail)),
    );

    let headline = wrapping_value(headline);
    headline.set_hexpand(true);
    headline.set_halign(gtk::Align::Fill);
    headline.add_css_class("debug-data-activity-message");
    content.append(&headline);

    if let Some(detail) = detail {
        let detail = wrapping_value(detail);
        detail.set_hexpand(true);
        detail.set_halign(gtk::Align::Fill);
        detail.add_css_class("debug-data-activity-detail");
        content.append(&detail);
    }

    row.append(&content);
    let metadata = gtk::Box::new(gtk::Orientation::Vertical, 2);
    metadata.set_valign(gtk::Align::Center);
    let time = gtk::Label::new(Some(&event.time));
    time.set_halign(gtk::Align::End);
    time.add_css_class("debug-data-activity-time");
    metadata.append(&time);

    if event.occurrences > 1 {
        let occurrences = gtk::Label::new(Some(&format!("×{}", event.occurrences)));
        occurrences.set_halign(gtk::Align::End);
        occurrences.set_tooltip_text(Some("Consecutive identical events"));
        occurrences.add_css_class("debug-data-activity-occurrences");
        metadata.append(&occurrences);
    }

    row.append(&metadata);

    row
}

fn debug_data_page() -> gtk::Box {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 6);
    page.set_margin_top(8);
    page.set_margin_bottom(8);
    page.set_margin_start(8);
    page.set_margin_end(8);

    page
}

fn debug_data_page_with_search(search: &gtk::Entry) -> gtk::Box {
    let page = debug_data_page();
    page.append(search);

    page
}

fn debug_data_search(placeholder: &str) -> gtk::Entry {
    let search = gtk::Entry::builder()
        .placeholder_text(placeholder)
        .primary_icon_name("system-search-symbolic")
        .build();

    search.add_css_class("debug-data-search");

    search.connect_changed(|search| {
        search
            .set_secondary_icon_name((!search.text().is_empty()).then_some("edit-clear-symbolic"));
    });

    search.connect_icon_release(|search, position| {
        if position == gtk::EntryIconPosition::Secondary {
            search.set_text("");
        }
    });

    search
}

fn scrolled_page(page: &gtk::Box) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .child(page)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .overlay_scrolling(false)
        .build()
}

fn append_debug_data_page(notebook: &gtk::Notebook, content: &gtk::Box, title: &str) {
    let page = scrolled_page(content);
    let label = gtk::Label::new(Some(title));
    label.set_hexpand(true);
    label.set_halign(gtk::Align::Fill);
    label.set_xalign(0.5);
    label.set_width_chars(1);
    label.set_max_width_chars(1);
    label.set_ellipsize(pango::EllipsizeMode::End);
    notebook.append_page(&page, Some(&label));
    let notebook_page = notebook.page(&page);
    notebook_page.set_tab_expand(true);
    notebook_page.set_tab_fill(true);
}

fn clear_page_after_search(page: &gtk::Box) {
    clear_label_selections(page);

    while page
        .last_child()
        .is_some_and(|child| !child.has_css_class("debug-data-search"))
    {
        if let Some(child) = page.last_child() {
            page.remove(&child);
        }
    }
}

fn clear_debug_data_box(container: &gtk::Box) {
    clear_label_selections(container);
    clear_box(container);
}

fn debug_data_section(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("debug-data-section");
    label.set_halign(gtk::Align::Fill);
    label.set_xalign(0.0);
    label.set_margin_top(5);

    label
}

fn debug_data_fact(name: &str, value: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    row.add_css_class("debug-data-fact");
    let name = gtk::Label::new(Some(name));
    name.add_css_class("debug-data-fact-name");
    name.set_halign(gtk::Align::Start);
    name.set_xalign(0.0);
    let value = selectable_value(value);
    value.set_hexpand(true);
    row.append(&name);
    row.append(&value);

    row
}

fn selectable_value(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_halign(gtk::Align::Start);
    label.set_xalign(0.0);
    enable_stable_text_selection(&label);
    label.set_focusable(false);
    label.set_ellipsize(pango::EllipsizeMode::Middle);
    label.set_tooltip_text(Some(text));

    label
}

fn wrapping_value(text: &str) -> gtk::Label {
    let label = selectable_value(text);
    label.set_ellipsize(pango::EllipsizeMode::None);
    label.set_wrap(true);
    label.set_wrap_mode(pango::WrapMode::WordChar);

    label
}

fn muted_label(text: &str) -> gtk::Label {
    let label = wrapping_value(text);
    label.add_css_class("muted");

    label
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_pretty_printers_by_scope_and_provider() {
        let lines = [
            "global pretty-printers:",
            "  builtin",
            "objfile /tmp/rust-target pretty-printers:",
            "  rust",
            "    StdArc",
            "    StdBTreeMap",
            "    StdVec",
        ]
        .map(String::from);

        assert_eq!(
            parse_pretty_printer_scopes(&lines),
            [
                PrettyPrinterScope {
                    name: String::from("global pretty-printers:"),
                    direct_printers: vec![String::from("builtin")],
                    providers: Vec::new(),
                },
                PrettyPrinterScope {
                    name: String::from("objfile /tmp/rust-target pretty-printers:"),
                    direct_printers: Vec::new(),
                    providers: vec![PrettyPrinterProvider {
                        name: String::from("rust"),
                        printers: vec![
                            String::from("StdArc"),
                            String::from("StdBTreeMap"),
                            String::from("StdVec"),
                        ],
                    }],
                },
            ]
        );
    }

    #[test]
    fn presents_objfile_scope_as_a_short_title_and_full_path() {
        assert_eq!(
            pretty_printer_scope_identity("objfile /tmp/debug/rust-target pretty-printers:"),
            (
                "OBJFILE",
                String::from("rust-target"),
                Some(String::from("/tmp/debug/rust-target")),
            )
        );
    }

    #[test]
    fn preserves_flat_pretty_printer_reports() {
        let lines = ["StdVec", "StdString", "CustomPrinter"].map(String::from);

        assert_eq!(
            parse_pretty_printer_scopes(&lines),
            [PrettyPrinterScope {
                name: String::from("GDB pretty-printers"),
                direct_printers: lines.into_iter().collect(),
                providers: Vec::new(),
            }]
        );
    }

    #[test]
    fn limits_rendered_printers_without_losing_match_counts() {
        let scope = PrettyPrinterScope {
            name: String::from("global pretty-printers:"),
            direct_printers: (0..500).map(|index| format!("Printer{index}")).collect(),
            providers: Vec::new(),
        };

        let mut remaining = PRETTY_PRINTER_PAGE_SIZE;
        let (filtered, matching) = filter_pretty_printer_scope(&scope, "", &mut remaining);
        let filtered = filtered.expect("the first page should be visible");
        assert_eq!(matching, 500);
        assert_eq!(pretty_printer_count(&filtered), PRETTY_PRINTER_PAGE_SIZE);
        assert_eq!(remaining, 0);
        let mut remaining = PRETTY_PRINTER_PAGE_SIZE;

        let (filtered, matching) =
            filter_pretty_printer_scope(&scope, "printer499", &mut remaining);

        let filtered = filtered.expect("the matching printer should be visible");
        assert_eq!(matching, 1);
        assert_eq!(filtered.direct_printers, ["Printer499"]);
    }

    #[test]
    fn coalesces_only_consecutive_activity_with_the_same_kind() {
        let mut activity = Vec::new();

        append_debug_data_activity(
            &mut activity,
            DebugDataActivityKind::Progress,
            String::from("Loading symbols"),
            String::from("10:00:00"),
        );

        append_debug_data_activity(
            &mut activity,
            DebugDataActivityKind::Progress,
            String::from("Loading symbols"),
            String::from("10:00:01"),
        );

        assert_eq!(activity.len(), 1);
        assert_eq!(activity[0].occurrences, 2);
        assert_eq!(activity[0].time, "10:00:01");

        append_debug_data_activity(
            &mut activity,
            DebugDataActivityKind::Error,
            String::from("Loading symbols"),
            String::from("10:00:02"),
        );

        assert_eq!(activity.len(), 2);
        assert_eq!(activity[1].occurrences, 1);
    }

    #[test]
    fn activity_history_retains_only_the_newest_bounded_entries() {
        let mut activity = Vec::new();

        for index in 0..MAX_DEBUG_DATA_ACTIVITY_EVENTS + 5 {
            append_debug_data_activity(
                &mut activity,
                DebugDataActivityKind::Success,
                format!("event {index}"),
                String::from("10:00:00"),
            );
        }

        assert_eq!(activity.len(), MAX_DEBUG_DATA_ACTIVITY_EVENTS);
        assert_eq!(activity[0].message.as_ref(), "event 5");
    }
}
