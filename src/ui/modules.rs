//! Shared-library navigation and symbol actions. Menus are built on demand.

use super::*;
use crate::symbols::{ModuleKey, Outcome, ResolveMode};

#[derive(Clone)]
pub(super) struct ModuleControls {
    pub root: gtk::Box,
    pub list: gtk::Box,
    pub symbol_labels: Rc<RefCell<Vec<gtk::Label>>>,
    load_all: gtk::Button,
    menu: Rc<RefCell<Option<gtk::Popover>>>,
}

impl ModuleControls {
    pub(super) fn dismiss_menu(&self) {
        let previous = self.menu.borrow_mut().take();

        if let Some(popover) = previous {
            popover.popdown();
        }
    }

    fn show_menu(&self, popover: &gtk::Popover) {
        let active = Rc::downgrade(&self.menu);

        popover.connect_closed(move |popover| {
            if let Some(active) = active.upgrade() {
                let is_current = active.borrow().as_ref() == Some(popover);

                if is_current {
                    active.borrow_mut().take();
                }
            }

            if let Some(parent) = popover.parent() {
                let popover = popover.clone();

                // GTK must finish closing before the popup is detached. Keep its
                // owner alive until that structural cleanup has completed.
                glib::idle_add_local_once(move || {
                    if popover.parent().is_some() {
                        popover.unparent();
                    }

                    drop(parent);
                });
            }
        });

        self.menu.replace(Some(popover.clone()));
        popover.popup();
    }
}

pub(super) fn build_module_controls() -> ModuleControls {
    let root = components::panel();
    let load_all = gtk::Button::with_label("Load symbols for all");
    load_all.add_css_class("inline-action");
    load_all.set_hexpand(true);
    load_all.set_focus_on_click(false);
    load_all.set_sensitive(false);
    load_all.set_tooltip_text(Some(
        "Resolve all modules in the selected inferior, one at a time, using the configured download policy",
    ));

    let toolbar = components::control_row();
    toolbar.add_css_class("module-toolbar");
    toolbar.append(&load_all);
    root.append(&toolbar);
    let list = dynamic_list("Modules appear after the inferior starts");

    let scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();

    root.append(&scroll);

    ModuleControls {
        root,
        list,
        symbol_labels: Rc::new(RefCell::new(Vec::new())),
        load_all,
        menu: Rc::new(RefCell::new(None)),
    }
}

pub(super) fn set_module_symbol_status(label: &gtk::Label, status: crate::symbols::SymbolStatus) {
    if label.text() != status.label() {
        label.set_text(status.label());
    }

    if label.tooltip_text().as_deref() != Some(status.description()) {
        label.set_tooltip_text(Some(status.description()));
    }

    let (current, previous) = match status {
        crate::symbols::SymbolStatus::SymbolsLoaded | crate::symbols::SymbolStatus::DebugInfo => {
            ("module-symbols-loaded", "module-symbols-missing")
        }
        crate::symbols::SymbolStatus::NotLoaded | crate::symbols::SymbolStatus::SymbolsOnly => {
            ("module-symbols-missing", "module-symbols-loaded")
        }
    };

    if !label.has_css_class(current) {
        label.remove_css_class(previous);
        label.add_css_class(current);
    }
}

impl Ui {
    pub(super) fn connect_module_controls(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);

        self.module_controls.load_all.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.dispatch_symbol_action(DebugDataAction::RetryAllSymbols);
            }
        });

        let weak = Rc::downgrade(self);

        self.module_controls.root.connect_unmap(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.module_controls.dismiss_menu();
            }
        });
    }

    pub(super) fn update_module_control_sensitivity(&self) {
        let available = self.model.debugger_synchronization_available()
            && self.model.selected_inferior_id().is_some()
            && !self.latest_modules.borrow().is_empty()
            && !self.model.symbols.batch_in_progress();

        set_transient_execution_sensitive(
            &self.module_controls.load_all,
            available,
            self.execution_visual_transition_pending() || self.model.inferior_is_running(),
        );
    }

    fn connect_module_context_menu(&self, row: &gtk::Box, module: &SharedLibrary) {
        let Some(inferior) = self.model.selected_inferior_id() else {
            return;
        };

        let key = ModuleKey::new(&inferior, module);
        let host = module.host_name.clone();
        let weak = self.self_weak.borrow().clone();
        let gesture = gtk::GestureClick::new();
        gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
        gesture.set_propagation_phase(gtk::PropagationPhase::Capture);

        gesture.connect_pressed(move |gesture, _, x, y| {
            let (Some(ui), Some(row)) = (weak.upgrade(), gesture.widget()) else {
                return;
            };

            if ui.model.selected_inferior_id().as_deref() != Some(&key.inferior)
                || ui.model.symbols.snapshot(&key).is_none()
            {
                return;
            }

            gesture.set_state(gtk::EventSequenceState::Claimed);
            ui.present_module_menu(&row, &key, host.as_deref(), x, y);
        });

        row.add_controller(gesture);
    }

    fn present_module_menu(
        self: &Rc<Self>,
        row: &gtk::Widget,
        key: &ModuleKey,
        host: Option<&str>,
        x: f64,
        y: f64,
    ) {
        self.module_controls.dismiss_menu();
        let Some(symbols) = self.model.symbols.snapshot(key) else {
            return;
        };

        let Some(point) = row.compute_point(
            &self.module_controls.root,
            &gtk::graphene::Point::new(x as f32, y as f32),
        ) else {
            return;
        };

        let (popover, menu) = build_context_menu();
        popover.set_parent(&self.module_controls.root);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
            point.x().round() as i32,
            point.y().round() as i32,
            1,
            1,
        )));

        let name = Path::new(&key.target)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&key.target);

        let heading = gtk::Label::new(Some(name));
        heading.add_css_class("context-menu-heading");
        heading.set_xalign(0.0);
        heading.set_ellipsize(pango::EllipsizeMode::Middle);
        heading.set_max_width_chars(40);
        heading.set_tooltip_text(Some(&key.target));
        menu.append(&heading);
        let available = self.model.debugger_synchronization_available();

        if symbols.phase.is_some() {
            self.append_module_action(
                &menu,
                &popover,
                "Cancel symbol loading",
                "Cancel queued work or a download. A GDB command already loading symbols must finish safely",
                true,
                DebugDataAction::CancelSymbols(key.clone()),
            );
        } else {
            let consent = symbols.outcome == Some(Outcome::NeedsConsent);
            self.append_module_action(
                &menu,
                &popover,
                if consent {
                    "Fetch debug information"
                } else if symbols.symbols_loaded {
                    "Resolve debug information"
                } else {
                    "Load symbols"
                },
                if consent {
                    "Allow a debuginfod download for this module only"
                } else {
                    "Load and verify symbols using the configured download policy"
                },
                available,
                DebugDataAction::ResolveSymbols {
                    module: key.clone(),
                    mode: if consent {
                        ResolveMode::DownloadOnce
                    } else {
                        ResolveMode::Configured
                    },
                },
            );

            self.append_module_action(
                &menu,
                &popover,
                "Load local symbols only",
                "Load and verify local or cached debug information without downloading",
                available,
                DebugDataAction::ResolveSymbols {
                    module: key.clone(),
                    mode: ResolveMode::LocalOnly,
                },
            );
        }

        menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let details = context_menu_action("Show in Debug Data");
        let weak = Rc::downgrade(self);
        let weak_popover = popover.downgrade();
        let target = key.target.clone();
        let lifetime = self.model.symbols.lifetime();
        let inferior = key.inferior.clone();

        details.connect_clicked(move |_| {
            if let Some(popover) = weak_popover.upgrade() {
                popover.popdown();
            }

            let weak = weak.clone();
            let target = target.clone();
            let inferior = inferior.clone();

            glib::idle_add_local_once(move || {
                if let Some(ui) = weak.upgrade()
                    && ui.model.symbols.lifetime() == lifetime
                    && ui.model.selected_inferior_id().as_deref() == Some(&inferior)
                {
                    ui.present_debug_data_module(&target);
                }
            });
        });

        menu.append(&details);
        menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        append_module_copy_action(&menu, &popover, "Copy module name", name);
        append_module_copy_action(&menu, &popover, "Copy target path", &key.target);

        if let Some(host) = host.filter(|host| *host != key.target) {
            append_module_copy_action(&menu, &popover, "Copy host path", host);
        }

        self.module_controls.show_menu(&popover);
    }

    fn append_module_action(
        self: &Rc<Self>,
        menu: &gtk::Box,
        popover: &gtk::Popover,
        label: &str,
        tooltip: &str,
        available: bool,
        action: DebugDataAction,
    ) {
        let button = context_menu_action(label);
        button.set_tooltip_text(Some(tooltip));
        button.set_sensitive(available);
        let weak = Rc::downgrade(self);
        let weak_popover = popover.downgrade();
        let lifetime = self.model.symbols.lifetime();
        let inferior = self.model.selected_inferior_id();

        button.connect_clicked(move |_| {
            if let Some(popover) = weak_popover.upgrade() {
                popover.popdown();
            }

            if let Some(ui) = weak.upgrade()
                && ui.model.symbols.lifetime() == lifetime
                && ui.model.selected_inferior_id() == inferior
            {
                ui.dispatch_symbol_action(action.clone());
            }
        });

        menu.append(&button);
    }

    pub fn show_modules(&self, modules: &[SharedLibrary]) -> bool {
        if self.latest_modules.borrow().as_slice() == modules {
            return false;
        }

        let render_started = Instant::now();
        let same_rows = {
            let previous = self.latest_modules.borrow();
            previous.len() == modules.len()
                && previous.iter().zip(modules).all(|(before, after)| {
                    before.target_name == after.target_name
                        && before.host_name == after.host_name
                        && before.from == after.from
                        && before.to == after.to
                })
        };

        self.latest_modules.replace(modules.to_vec());

        if same_rows {
            self.refresh_module_symbol_labels();
            self.render_debug_data_modules();
            self.record_ui_render_duration("module pane", render_started);
            return true;
        }

        self.reset_debug_data_module_paging();

        if modules.is_empty() {
            self.module_debug_metadata.borrow_mut().clear();
        }

        self.module_controls.dismiss_menu();
        self.update_module_control_sensitivity();
        self.render_debug_data_overview();
        self.render_debug_data_modules();
        self.module_controls.symbol_labels.borrow_mut().clear();
        clear_box(&self.module_controls.list);

        if modules.is_empty() {
            self.module_controls
                .list
                .append(&empty_label("No shared libraries loaded"));

            self.record_ui_render_duration("module pane", render_started);
            return true;
        }

        let module_limit =
            self.adaptive_render_limit("module pane", crate::performance::MODULE_WIDGET_BUDGET, 32);

        for module in modules.iter().take(module_limit) {
            let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
            row.add_css_class("module-row");
            let heading = gtk::Box::new(gtk::Orientation::Horizontal, 4);

            let name = Path::new(&module.target_name)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&module.target_name);

            let name = gtk::Label::new(Some(name));
            name.add_css_class("module-name");
            name.set_halign(gtk::Align::Start);
            name.set_hexpand(true);
            name.set_ellipsize(pango::EllipsizeMode::End);

            let symbol_state = gtk::Label::new(None);
            symbol_state.add_css_class("module-symbol-state");
            self.module_controls
                .symbol_labels
                .borrow_mut()
                .push(symbol_state.clone());

            heading.append(&name);
            heading.append(&symbol_state);

            let range = match (&module.from, &module.to) {
                (Some(from), Some(to)) => format!("{from}-{to}"),
                _ => String::from("address range unavailable"),
            };

            let range = gtk::Label::new(Some(&range));
            range.add_css_class("module-range");
            range.set_halign(gtk::Align::Start);
            enable_stable_text_selection(&range);
            let path = module.host_name.as_deref().unwrap_or(&module.target_name);
            let path_label = gtk::Label::new(Some(path));
            path_label.add_css_class("module-path");
            path_label.set_halign(gtk::Align::Start);
            path_label.set_ellipsize(pango::EllipsizeMode::Middle);
            enable_stable_text_selection(&path_label);

            path_label.set_tooltip_text(Some(&format!(
                "Target: {}\nHost: {}",
                module.target_name, path
            )));

            row.append(&heading);
            row.append(&range);
            row.append(&path_label);
            self.connect_module_context_menu(&row, module);
            self.module_controls.list.append(&row);
        }

        if modules.len() > module_limit {
            let shown = module_limit;
            let omitted = modules.len() - shown;

            let notice = super::debug_state::performance_partial_label(&format!(
                "{omitted} additional module{} available in Debug Data",
                if omitted == 1 { " is" } else { "s are" }
            ));

            self.module_controls.list.append(&notice);

            self.record_performance_notice(crate::performance::PerformanceNotice::count(
                crate::performance::BudgetOutcome::Partial,
                "module pane",
                shown,
                modules.len(),
            ));
        }

        self.refresh_module_symbol_labels();
        self.record_ui_render_duration("module pane", render_started);

        true
    }

    pub(super) fn refresh_module_symbol_labels(&self) {
        let inferior = self.model.selected_inferior_id();
        let modules = self.latest_modules.borrow();
        let labels = self.module_controls.symbol_labels.borrow();

        for (module, label) in modules.iter().zip(labels.iter()) {
            let status = inferior
                .as_deref()
                .and_then(|inferior| {
                    self.model
                        .symbols
                        .snapshot(&crate::symbols::ModuleKey::new(inferior, module))
                })
                .map_or_else(
                    || {
                        crate::symbols::SymbolStatus::new(
                            module.symbols_loaded,
                            &crate::symbols::DebugInfo::Unverified,
                        )
                    },
                    |symbols| symbols.status(),
                );

            set_module_symbol_status(label, status);
        }
    }
}

fn append_module_copy_action(menu: &gtk::Box, popover: &gtk::Popover, label: &str, text: &str) {
    let button = context_menu_action(label);
    let text = text.to_owned();
    let weak = popover.downgrade();

    button.connect_clicked(move |button| {
        button.display().clipboard().set_text(&text);

        if let Some(popover) = weak.upgrade() {
            popover.popdown();
        }
    });

    menu.append(&button);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display, run separately from other GTK tests"]
    fn module_menus_release_after_close_and_row_replacement() {
        gtk::init().unwrap();
        Theme::graphite().install();
        let controls = build_module_controls();
        let window = gtk::Window::builder()
            .default_width(320)
            .default_height(400)
            .child(&controls.root)
            .build();

        window.present();
        let context = glib::MainContext::default();

        for index in 0..8 {
            clear_box(&controls.list);
            let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
            row.append(&gtk::Label::new(Some("libc.so.6")));
            controls.list.append(&row);
            context.block_on(glib::timeout_future(Duration::from_millis(50)));
            let (popover, menu) = build_context_menu();
            menu.append(&context_menu_action("Load symbols"));
            popover.set_parent(&controls.root);
            controls.show_menu(&popover);
            context.block_on(glib::timeout_future(Duration::from_millis(50)));

            while context.pending() {
                context.iteration(false);
            }

            assert!(popover.is_visible());
            assert!(popover.is_mapped());

            if index % 2 == 0 {
                controls.dismiss_menu();
            } else {
                popover.popdown();
            }

            assert!(controls.menu.borrow().is_none());
            assert!(!popover.is_visible());
            let weak = popover.downgrade();
            let weak_row = row.downgrade();
            drop(popover);
            drop(menu);
            clear_box(&controls.list);
            drop(row);
            let deadline = Instant::now() + Duration::from_secs(2);

            while weak.upgrade().is_some() && Instant::now() < deadline {
                context.block_on(glib::timeout_future(Duration::from_millis(20)));
            }

            assert!(weak.upgrade().is_none());
            assert!(weak_row.upgrade().is_none());
        }

        window.close();
    }
}
