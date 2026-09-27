//! Dispatch configurable shortcuts through the same guarded controls as pointer actions.

use super::*;
use crate::config::keybindings::{Action, Bindings, Scope, ShortcutMatch};

enum Target {
    Button(gtk::Button),
    Toggle(gtk::ToggleButton),
    Panel(Rc<workspace::Panels>, PanelId, Option<gtk::Stack>),
}

impl Target {
    fn button(&self) -> Option<&gtk::Button> {
        match self {
            Self::Button(button) => Some(button),
            Self::Toggle(button) => Some(button.upcast_ref()),
            Self::Panel(..) => None,
        }
    }

    fn activate(&self) -> bool {
        if self.button().is_some_and(|button| !button.is_sensitive()) {
            return false;
        }

        match self {
            Self::Button(button) => button.emit_clicked(),
            Self::Toggle(button) => button.set_active(!button.is_active()),
            Self::Panel(panels, panel, locks) => {
                if let Some(pages) = locks {
                    pages.set_visible_child_name("locks");
                }

                panels.reveal(*panel);
            }
        }

        true
    }
}

impl Ui {
    fn shortcut_targets(&self) -> Vec<(Action, Target)> {
        Action::ALL
            .iter()
            .map(|&action| {
                let panel = match action {
                    Action::Context => Some(PanelId::Context),
                    Action::Watches => Some(PanelId::Watches),
                    Action::Registers => Some(PanelId::Registers),
                    Action::Memory => Some(PanelId::Memory),
                    Action::Breakpoints => Some(PanelId::Breakpoints),
                    Action::CallStack => Some(PanelId::CallStack),
                    Action::Threads => Some(PanelId::Threads),
                    Action::Kernel => Some(PanelId::Kernel),
                    Action::Locks => Some(PanelId::Misc),
                    _ => None,
                };

                if let Some(panel) = panel {
                    return (
                        action,
                        Target::Panel(
                            Rc::clone(&self.panels),
                            panel,
                            (action == Action::Locks).then(|| self.misc_view.pages.clone()),
                        ),
                    );
                }

                let button = match action {
                    Action::Run => &self.run_button,
                    Action::Pause => &self.pause_button,
                    Action::Next => &self.next_button,
                    Action::Step => &self.step_button,
                    Action::NextInstruction => &self.next_instruction_button,
                    Action::StepInstruction => &self.step_instruction_button,
                    Action::Finish => &self.finish_button,
                    Action::Resynchronize => &self.resynchronize_button,
                    Action::Settings => &self.configuration_button,
                    Action::DebugData => &self.debug_data_button,
                    Action::SourceBack => &self.source.navigation.back,
                    Action::SourceForward => &self.source.navigation.forward,
                    Action::QuickOpen => &self.source.navigation.quick_open,
                    Action::OpenFile => &self.source.navigation.open_file,
                    Action::Find => &self.source.navigation.find,
                    Action::GoToLine => &self.source.navigation.go_to_line,
                    Action::Symbols => &self.source.navigation.symbols,
                    Action::LoadedSources => &self.source.navigation.loaded_search,
                    Action::SourceTree => &self.source.navigation.tree_search,
                    Action::ReopenSource => &self.source.navigation.reopen_closed,
                    Action::Terminal => {
                        return (action, Target::Toggle(self.terminal_toggle_button.clone()));
                    }
                    Action::Log => return (action, Target::Toggle(self.log_toggle_button.clone())),
                    Action::Context
                    | Action::Watches
                    | Action::Registers
                    | Action::Memory
                    | Action::Breakpoints
                    | Action::CallStack
                    | Action::Threads
                    | Action::Kernel
                    | Action::Locks => unreachable!(),
                };

                (action, Target::Button(button.clone()))
            })
            .collect()
    }

    pub(super) fn update_keybinding_hints(&self, bindings: &Bindings) {
        for (action, target) in self.shortcut_targets() {
            let binding = bindings.value(action);
            let detail = if binding == "Disabled" { "" } else { &binding };
            let tooltip = if detail.is_empty() {
                action.help().to_owned()
            } else {
                format!("{}\n{detail}", action.help())
            };

            if let Some(button) = target.button() {
                button.set_tooltip_text(Some(&tooltip));
                components::set_menu_action_detail(button, detail);
            }
        }
    }

    // Install once per workspace host, not on its transient value editors.
    pub(super) fn connect_keyboard_shortcuts(&self, host: &impl IsA<gtk::Window>) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let targets = self.shortcut_targets();
        let settings = Rc::clone(&self.settings);
        let terminal = self.terminal.clone();
        let find_bar = self.source.navigation.find_bar.clone();
        let find_close = self.source.navigation.find_close.clone();
        let window = host.as_ref().downgrade();
        let log = self.application_log.clone();
        let warned = Cell::new(None);

        keys.connect_key_pressed(move |controller, key, _, modifiers| {
            let host = window.upgrade();
            let focus = host.as_ref().and_then(gtk::prelude::GtkWindowExt::focus);

            let in_terminal = focus.as_ref().is_some_and(|focus| {
                focus == terminal.upcast_ref::<gtk::Widget>() || focus.is_ancestor(&terminal)
            });

            // Escape is a local dismissal key, never a configurable execution action.
            if key == gtk::gdk::Key::Escape
                && (modifiers - gtk::gdk::ModifierType::LOCK_MASK).is_empty()
                && find_bar.is_visible()
                && !in_terminal
                && workspace::host_window(&find_bar).as_ref() == host.as_ref()
            {
                find_close.emit_clicked();
                return glib::Propagation::Stop;
            }

            let Some(event) = controller
                .current_event()
                .and_then(|event| event.downcast::<gtk::gdk::KeyEvent>().ok())
            else {
                return glib::Propagation::Proceed;
            };

            // This returns a copied identity. No settings borrow crosses an action callback.
            let (action, shortcut) = match settings.shortcut_action(&event) {
                ShortcutMatch::Unbound => return glib::Propagation::Proceed,
                ShortcutMatch::Bound(action, shortcut) => (action, shortcut),
                ShortcutMatch::Ambiguous(first, second) => {
                    if warned.replace(Some((first, second))) != Some((first, second)) {
                        log.record(LogLevel::Warning, "Keybindings", &format!(
                            "'{}' and '{}' match the same key on this keyboard layout. Change one in Settings > Keybindings. Neither action was executed",
                            first.title(), second.title(),
                        ));
                    }

                    return glib::Propagation::Proceed;
                }
            };

            if in_terminal
                && (action.scope() == Scope::Source
                    || (!shortcut.is_function_key()
                        && !matches!(action, Action::Terminal | Action::Resynchronize)))
            {
                return glib::Propagation::Proceed;
            }

            if action.scope() == Scope::Execution
                && !shortcut.is_function_key()
                && focus.is_some_and(|focus| {
                    focus.is::<gtk::Editable>()
                        || focus
                            .downcast_ref::<gtk::TextView>()
                            .is_some_and(|view| view.is_editable())
                })
            {
                return glib::Propagation::Proceed;
            }

            let target = &targets[action as usize].1;

            if action.scope() == Scope::Source
                && let Some(button) = target.button()
                && button.is_sensitive()
                && let Some(window) = workspace::host_window(button)
                && Some(&window) != host.as_ref()
            {
                window.present();
            }

            if target.activate() {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });

        host.as_ref().add_controller(keys);
    }
}
