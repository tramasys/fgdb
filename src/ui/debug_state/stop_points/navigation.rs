use super::*;

fn source_target(breakpoint: &Breakpoint) -> Option<(&Path, u32)> {
    let path = Path::new(breakpoint.source_path()?);
    let line = breakpoint.line?;

    (breakpoint.pending.is_none() && !path.as_os_str().is_empty() && line > 0)
        .then_some((path, line))
}

fn row_navigation_target(row: &gtk::Widget, mut target: gtk::Widget) -> bool {
    while target != *row {
        if target.is::<gtk::Button>()
            || target
                .downcast_ref::<gtk::Label>()
                .is_some_and(|label| label.is_selectable())
        {
            return false;
        }

        let Some(parent) = target.parent() else {
            return false;
        };

        target = parent;
    }

    true
}

fn connect_navigation(row: &gtk::Box, source: &gtk::Label, activate: impl Fn() + 'static) {
    let activate: Rc<dyn Fn()> = Rc::new(activate);
    let open = Rc::clone(&activate);
    let text = glib::markup_escape_text(&source.text());
    source.add_css_class("breakpoint-source");
    source.set_markup(&format!("<a href=\"breakpoint-source\">{text}</a>"));

    // GtkLabel handles link clicks without taking away drag-to-select or copy.
    source.connect_activate_link(move |_, _| {
        open();
        glib::Propagation::Stop
    });

    row.set_focusable(true);
    row.set_cursor_from_name(Some("pointer"));
    row.set_tooltip_text(Some("Open breakpoint source"));
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_PRIMARY);
    let pressed = Rc::new(Cell::new(false));
    let pressed_start = Rc::clone(&pressed);

    click.connect_pressed(move |gesture, count, x, y| {
        let eligible = count == 1
            && (gesture.current_event_state() & gtk::accelerator_get_default_mod_mask()).is_empty()
            && gesture.widget().is_some_and(|row| {
                row.pick(x, y, gtk::PickFlags::INSENSITIVE)
                    .is_some_and(|target| row_navigation_target(&row, target))
            });

        pressed_start.set(eligible);
    });

    let cancelled = Rc::clone(&pressed);
    click.connect_cancel(move |_, _| cancelled.set(false));
    let stopped = Rc::clone(&pressed);
    click.connect_stopped(move |_| stopped.set(false));
    let open = Rc::clone(&activate);

    click.connect_released(move |gesture, count, x, y| {
        if pressed.replace(false)
            && count == 1
            && let Some(row) = gesture.widget()
            && row
                .pick(x, y, gtk::PickFlags::INSENSITIVE)
                .is_some_and(|target| row_navigation_target(&row, target))
        {
            open();
        }
    });

    row.add_controller(click);
    let keys = gtk::EventControllerKey::new();

    keys.connect_key_pressed(move |keys, key, _, modifiers| {
        if modifiers.is_empty()
            && matches!(
                key,
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter | gtk::gdk::Key::space
            )
            && keys.widget().is_some_and(|row| row.has_focus())
        {
            activate();
            return glib::Propagation::Stop;
        }

        glib::Propagation::Proceed
    });

    row.add_controller(keys);
}

impl Ui {
    pub(super) fn connect_breakpoint_source(
        &self,
        row: &gtk::Box,
        source: &gtk::Label,
        breakpoint: &Breakpoint,
    ) {
        let Some((path, line)) = source_target(breakpoint) else {
            return;
        };

        let path = path.to_path_buf();
        let number = breakpoint.number.clone();
        let weak = self.self_weak.borrow().clone();

        connect_navigation(row, source, move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };

            // Source-index refreshes can leave the previous rows visible briefly.
            if !ui
                .latest_source_breakpoint(&number)
                .is_some_and(|breakpoint| source_target(&breakpoint) == Some((&path, line)))
            {
                return;
            }

            ui.navigate_to_source_then(&path, line, true, |ui, opened| {
                if opened {
                    ui.set_status("Source", "Breakpoint source opened", None);
                }
            });
        });
    }
}

#[cfg(test)]
mod tests;
