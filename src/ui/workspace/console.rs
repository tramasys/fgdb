//! Shared Terminal/Log selection, visibility, and focus, independent of storage.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui) enum Selection {
    Hidden,
    Terminal,
    Log,
}

impl Selection {
    pub(in crate::ui) fn name(self) -> &'static str {
        match self {
            Self::Hidden => "hidden",
            Self::Terminal => "terminal",
            Self::Log => "log",
        }
    }

    pub(in crate::ui) fn parse(text: &str) -> Option<Self> {
        match text {
            "hidden" => Some(Self::Hidden),
            "terminal" => Some(Self::Terminal),
            "log" => Some(Self::Log),
            _ => None,
        }
    }
}

pub(in crate::ui) fn install(
    stack: &gtk::Stack,
    terminal_button: &gtk::ToggleButton,
    log_button: &gtk::ToggleButton,
    terminal: &gtk::Widget,
    log: &gtk::Widget,
    selected: Selection,
    changed: impl Fn(Selection) + 'static,
) {
    terminal_button.set_active(selected == Selection::Terminal);
    log_button.set_active(selected == Selection::Log);

    if selected != Selection::Hidden {
        stack.set_visible_child_name(selected.name());
    }

    stack.set_visible(selected != Selection::Hidden);
    let updating = Rc::new(Cell::new(false));
    let changed = Rc::new(changed);

    // Signals own this controller's state but only weakly reference widgets.
    // Discarding the workspace also discards its subscriptions and state.
    for (button, other, focus, selected) in [
        (terminal_button, log_button, terminal, Selection::Terminal),
        (log_button, terminal_button, log, Selection::Log),
    ] {
        let stack = stack.downgrade();
        let other = other.downgrade();
        let focus = focus.downgrade();
        let updating = Rc::clone(&updating);
        let changed = Rc::clone(&changed);

        button.connect_toggled(move |button| {
            let (Some(stack), Some(other)) = (stack.upgrade(), other.upgrade()) else {
                return;
            };

            if updating.replace(true) {
                return;
            }

            // Switch once without temporarily collapsing the shared pane.
            let selected = if button.is_active() {
                other.set_active(false);
                stack.set_visible_child_name(selected.name());
                selected
            } else {
                Selection::Hidden
            };

            stack.set_visible(selected != Selection::Hidden);
            updating.set(false);
            changed(selected);

            if selected != Selection::Hidden
                && let Some(focus) = focus.upgrade()
            {
                if let Some(window) = host_window(&focus) {
                    window.present();
                }

                focus.grab_focus();
            }
        });
    }
}
