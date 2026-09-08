//! Stable module cards. Progress updates do not replace controls or unrelated rows.

use super::*;
use crate::symbols::{DebugInfo, ModuleKey, ModuleSymbols, Outcome, ResolveMode, SymbolStatus};

#[derive(Clone)]
pub(super) struct ModuleList {
    rows: gtk::Box,
    empty: gtk::Label,
    more: gtk::Button,
    all: gtk::Button,
    cards: Rc<RefCell<HashMap<ModuleKey, ModuleCard>>>,
    dirty: Rc<Cell<bool>>,
}

impl ModuleList {
    pub(super) fn new(page: &gtk::Box, ui: Weak<Ui>) -> Self {
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let empty = muted_label("Modules appear after an executable or core is loaded");
        let more = gtk::Button::with_label("Show more modules");
        more.add_css_class("inline-action");
        more.set_halign(gtk::Align::Center);
        more.set_visible(false);
        let weak = ui.clone();

        more.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.dispatch_debug_data_action(DebugDataAction::ShowMoreModules);
            }
        });

        let all = gtk::Button::with_label("Resolve all module symbols");
        all.add_css_class("inline-action");
        all.set_halign(gtk::Align::Start);
        all.set_visible(false);

        all.connect_clicked(move |_| {
            if let Some(ui) = ui.upgrade() {
                ui.dispatch_symbol_action(DebugDataAction::RetryAllSymbols);
            }
        });

        page.append(&rows);
        page.append(&empty);
        page.append(&more);
        page.append(&all);

        Self {
            rows,
            empty,
            more,
            all,
            cards: Rc::default(),
            dirty: Rc::new(Cell::new(true)),
        }
    }

    pub(super) fn render(&self, ui: &Ui, view: &DebugDataView) {
        if !view.modules.is_mapped() {
            self.dirty.set(true);
            return;
        }

        self.dirty.set(false);
        let started = Instant::now();
        let query = view.module_search.text().trim().to_ascii_lowercase();
        let terms: Vec<_> = query.split_whitespace().collect();
        let metadata = ui.module_debug_metadata.borrow();
        let modules = ui.latest_modules.borrow();
        let inferior = ui.model.selected_inferior_id();
        let limit = ui
            .debug_data_state
            .borrow()
            .module_limit
            .max(DEBUG_DATA_RESULT_PAGE_SIZE);
        let mut cards = self.cards.take();
        let mut retained = HashSet::new();
        let mut matching = 0;
        let mut previous = None::<gtk::Widget>;

        for module in modules.iter() {
            let Some(inferior) = inferior.as_deref() else {
                break;
            };

            let key = ModuleKey::new(inferior, module);
            let symbols = ui.model.symbols.snapshot(&key);
            let path = Path::new(module.host_name.as_deref().unwrap_or(&module.target_name));
            let details = metadata.get(path);
            let status = symbols.as_ref().map_or_else(
                || SymbolStatus::new(module.symbols_loaded, &DebugInfo::Unverified),
                |symbols| symbols.status(),
            );

            let build_id = symbols
                .as_ref()
                .and_then(|symbols| symbols.build_id.as_deref())
                .or_else(|| details.and_then(|details| details.build_id.as_deref()))
                .unwrap_or("");

            if !terms.iter().all(|term| {
                text_matches(&module.target_name, term)
                    || text_matches(&path.to_string_lossy(), term)
                    || text_matches(build_id, term)
                    || text_matches(status.label(), term)
            }) {
                continue;
            }

            matching += 1;

            if retained.len() >= limit {
                continue;
            }

            let card = cards.entry(key.clone()).or_insert_with(|| {
                let card = ModuleCard::new(key.clone(), ui.self_weak.borrow().clone());
                self.rows.append(&card.root);
                card
            });

            card.update(
                symbols,
                details,
                path,
                status,
                ui.model.debugger_synchronization_available(),
            );

            if card.root.prev_sibling() != previous {
                self.rows.reorder_child_after(&card.root, previous.as_ref());
            }

            previous = Some(card.root.clone().upcast());
            retained.insert(key);
        }

        cards.retain(|key, card| {
            if retained.contains(key) {
                true
            } else {
                self.rows.remove(&card.root);
                false
            }
        });

        self.cards.replace(cards);
        self.empty.set_visible(matching == 0);
        set_label(
            &self.empty,
            if modules.is_empty() {
                "Modules appear after an executable or core is loaded"
            } else {
                "No modules match the filter"
            },
        );
        self.more.set_visible(matching > retained.len());
        set_button(
            &self.more,
            &format!(
                "Show {} more modules",
                matching
                    .saturating_sub(retained.len())
                    .min(DEBUG_DATA_RESULT_PAGE_SIZE)
            ),
        );
        self.all.set_visible(!modules.is_empty());
        self.all.set_sensitive(
            ui.model.debugger_synchronization_available() && !ui.model.symbols.batch_in_progress(),
        );
        ui.record_ui_render_duration("Debug Data modules", started);
    }

    pub(super) fn update(&self, ui: &Ui, view: &DebugDataView, keys: &HashSet<ModuleKey>) {
        if !view.modules.is_mapped() {
            self.dirty.set(true);
            return;
        }

        if self.dirty.get() || !view.module_search.text().trim().is_empty() {
            self.render(ui, view);
            return;
        }

        let metadata = ui.module_debug_metadata.borrow();

        for key in keys {
            let card = self.cards.borrow().get(key).cloned();

            if let (Some(card), Some(symbols)) = (card, ui.model.symbols.snapshot(key)) {
                let path = symbols.host.clone();
                let status = symbols.status();
                card.update(
                    Some(symbols),
                    metadata.get(&path),
                    &path,
                    status,
                    ui.model.debugger_synchronization_available(),
                );
            }
        }

        self.all.set_sensitive(
            ui.model.debugger_synchronization_available() && !ui.model.symbols.batch_in_progress(),
        );
    }
}

#[derive(Clone)]
struct ModuleCard {
    root: gtk::Box,
    name: gtk::Label,
    path: gtk::Label,
    status: gtk::Label,
    retry: gtk::Button,
    local: gtk::Button,
    progress: gtk::Box,
    spinner: gtk::Spinner,
    phase: gtk::Label,
    message: gtk::Label,
    details: gtk::Box,
    rendered: Rc<RefCell<Option<CardDetails>>>,
}

#[derive(PartialEq, Eq)]
struct CardDetails {
    metadata: Option<ModuleDebugMetadata>,
    debug_info: DebugInfo,
    build_id: Option<String>,
}

impl ModuleCard {
    fn new(key: ModuleKey, ui: Weak<Ui>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 3);
        root.add_css_class("debug-data-row");
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 7);
        let name = gtk::Label::new(None);
        name.add_css_class("module-name");
        name.set_halign(gtk::Align::Start);
        name.set_hexpand(true);
        name.set_ellipsize(pango::EllipsizeMode::End);
        name.set_tooltip_text(Some(&key.target));
        let status = gtk::Label::new(None);
        let retry = gtk::Button::with_label("Load symbols");
        let local = gtk::Button::with_label("Local only");

        for (button, local_only) in [(&retry, false), (&local, true)] {
            button.add_css_class("inline-action");
            let ui = ui.clone();
            let key = key.clone();

            button.connect_clicked(move |_| {
                let Some(ui) = ui.upgrade() else { return };
                let Some(state) = ui.model.symbols.snapshot(&key) else {
                    return;
                };

                let action = if !local_only && state.phase.is_some() {
                    DebugDataAction::CancelSymbols(key.clone())
                } else {
                    DebugDataAction::ResolveSymbols {
                        module: key.clone(),
                        mode: if local_only {
                            ResolveMode::LocalOnly
                        } else if state.outcome == Some(Outcome::NeedsConsent) {
                            ResolveMode::DownloadOnce
                        } else {
                            ResolveMode::Configured
                        },
                    }
                };

                ui.dispatch_symbol_action(action);
            });
        }

        local.set_tooltip_text(Some(
            "Load matching local or cached DWARF files without downloading",
        ));
        heading.append(&name);
        heading.append(&status);
        heading.append(&retry);
        heading.append(&local);
        let path = selectable_value("");
        let progress = gtk::Box::new(gtk::Orientation::Horizontal, components::CONTENT_INSET);
        let spinner = gtk::Spinner::new();
        let weak_spinner = spinner.downgrade();

        progress.connect_map(move |_| {
            if let Some(spinner) = weak_spinner.upgrade() {
                spinner.start();
            }
        });

        let weak_spinner = spinner.downgrade();

        progress.connect_unmap(move |_| {
            if let Some(spinner) = weak_spinner.upgrade() {
                spinner.stop();
            }
        });

        let phase = muted_label("");
        progress.append(&spinner);
        progress.append(&phase);
        let message = wrapping_value("");
        let details = gtk::Box::new(gtk::Orientation::Vertical, 3);
        root.append(&heading);
        root.append(&path);
        root.append(&progress);
        root.append(&message);
        root.append(&details);

        Self {
            root,
            name,
            path,
            status,
            retry,
            local,
            progress,
            spinner,
            phase,
            message,
            details,
            rendered: Rc::default(),
        }
    }

    fn update(
        &self,
        symbols: Option<Rc<ModuleSymbols>>,
        metadata: Option<&ModuleDebugMetadata>,
        path: &Path,
        status: SymbolStatus,
        available: bool,
    ) {
        set_label(
            &self.name,
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Module"),
        );
        set_label(&self.path, &path.to_string_lossy());
        super::super::modules::set_module_symbol_status(&self.status, status);
        let pending = symbols.as_ref().and_then(|state| state.phase);
        let consent = symbols
            .as_ref()
            .is_some_and(|state| state.outcome == Some(Outcome::NeedsConsent));
        set_button(
            &self.retry,
            if pending.is_some() {
                "Cancel"
            } else if consent {
                "Fetch debug info"
            } else if status == SymbolStatus::NotLoaded {
                "Load symbols"
            } else {
                "Retry"
            },
        );
        self.retry
            .set_sensitive(symbols.is_some() && (available || pending.is_some()));
        self.retry.set_tooltip_text(Some(if pending.is_some() {
            "Cancel queued work or a download. A GDB command already loading symbols must finish safely"
        } else if consent { "Allow a debuginfod download for this module only" }
        else { "Resolve this module using the configured symbol policy" }));
        self.local.set_visible(pending.is_none());
        self.local.set_sensitive(symbols.is_some() && available);
        self.progress.set_visible(pending.is_some());
        self.spinner
            .set_spinning(pending.is_some() && self.root.is_mapped());
        set_label(&self.phase, pending.map_or("", |phase| phase.label()));
        let message = symbols.as_ref().map_or("", |state| state.message.as_str());
        set_label(&self.message, message);
        self.message
            .set_visible(pending.is_none() && !message.is_empty());

        let debug_info = symbols
            .as_ref()
            .map_or(&DebugInfo::Unverified, |state| &state.debug_info);
        let build_id = symbols
            .as_ref()
            .and_then(|state| state.build_id.as_deref())
            .or_else(|| metadata.and_then(|metadata| metadata.build_id.as_deref()));

        if self.rendered.borrow().as_ref().is_some_and(|previous| {
            previous.metadata.as_ref() == metadata
                && &previous.debug_info == debug_info
                && previous.build_id.as_deref() == build_id
        }) {
            return;
        }

        let next = CardDetails {
            metadata: metadata.cloned(),
            debug_info: debug_info.clone(),
            build_id: build_id.map(str::to_owned),
        };

        clear_box(&self.details);

        match &next.debug_info {
            DebugInfo::Separate(path) => self.details.append(&debug_data_fact(
                "Attached debug file",
                &path.to_string_lossy(),
            )),
            DebugInfo::Embedded => self.details.append(&debug_data_fact(
                "Debug information",
                "DWARF data present in module",
            )),
            DebugInfo::Unverified => self.details.append(&debug_data_fact(
                "Debug information",
                "Not checked by fgdb. Use Local only to check without downloading",
            )),
            DebugInfo::Missing => {}
        }

        if next.debug_info.is_present() {
            self.details.append(&muted_label(
                "Types and split-DWARF dependencies are checked by GDB when inspected",
            ));
        }

        if let Some(build_id) = &next.build_id {
            self.details.append(&debug_data_fact("Build ID", build_id));
        }

        if let Some(metadata) = metadata {
            if let Some(debuglink) = &metadata.debuglink {
                let value = metadata.debuglink_crc.map_or_else(
                    || debuglink.clone(),
                    |crc| format!("{debuglink}  CRC {crc:08x}"),
                );
                self.details.append(&debug_data_fact("Debuglink", &value));
            }

            if !next.debug_info.is_present() {
                let local = metadata.separate_debug_file.as_ref().map_or_else(
                    || {
                        if metadata.embedded_debug_info {
                            String::from("DWARF data present in module")
                        } else {
                            String::from("Not found in searched directories")
                        }
                    },
                    |path| path.to_string_lossy().into_owned(),
                );
                let fact = debug_data_fact("Local debug information", &local);
                fact.set_tooltip_text(Some(
                    "Information found on this system. This does not mean GDB has loaded it",
                ));
                self.details.append(&fact);
            }

            for rejected in &metadata.rejected_debug_files {
                self.details.append(&muted_label(rejected));
            }

            if let Some(message) = metadata.error.as_deref().or_else(|| {
                (!next.debug_info.is_present())
                    .then_some(metadata.suggestion.as_deref())
                    .flatten()
            }) {
                self.details.append(&muted_label(message));
            }
        } else {
            self.details
                .append(&muted_label("Inspecting ELF metadata…"));
        }

        self.rendered.replace(Some(next));
    }
}

fn set_label(label: &gtk::Label, text: &str) {
    if label.text() != text {
        label.set_text(text);
    }
}

fn set_button(button: &gtk::Button, text: &str) {
    if button.label().as_deref() != Some(text) {
        button.set_label(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::Phase;

    #[test]
    #[ignore = "requires a GTK display, run separately from other GTK tests"]
    fn module_progress_preserves_controls_and_details() {
        gtk::init().unwrap();
        Theme::graphite().install();
        let key = ModuleKey {
            inferior: String::from("i1"),
            target: String::from("/lib/example.so"),
            from: None,
            to: None,
        };

        let card = ModuleCard::new(key, Weak::new());
        let window = gtk::Window::builder()
            .default_width(700)
            .child(&card.root)
            .build();
        let mut symbols = ModuleSymbols {
            host: PathBuf::from("/lib/example.so"),
            symbols_loaded: true,
            debug_info: DebugInfo::Missing,
            build_id: Some(String::from("deadbeef")),
            phase: None,
            outcome: None,
            message: String::new(),
        };

        card.update(
            Some(Rc::new(symbols.clone())),
            None,
            &symbols.host,
            symbols.status(),
            true,
        );
        window.present();
        let context = glib::MainContext::default();
        context.block_on(glib::timeout_future(Duration::from_millis(50)));
        let detail = card.details.first_child();
        let heading = card.root.first_child();
        let retry = card.retry.downgrade();

        for phase in [
            Some(Phase::Queued),
            Some(Phase::Checking),
            Some(Phase::Downloading),
            Some(Phase::Cancelling),
            None,
        ] {
            symbols.phase = phase;
            card.update(
                Some(Rc::new(symbols.clone())),
                None,
                &symbols.host,
                symbols.status(),
                true,
            );
            assert_eq!(card.details.first_child(), detail);
            assert_eq!(card.root.first_child(), heading);
            assert_eq!(retry.upgrade().as_ref(), Some(&card.retry));
            assert_eq!(card.progress.is_visible(), phase.is_some());
            assert!(card.retry.is_sensitive());
        }

        window.close();
        assert!(!card.spinner.is_spinning());
    }
}
