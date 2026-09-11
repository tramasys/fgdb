use super::*;

mod input;

use input::{Choice, Field, Input, Query};

const MAX_SUGGESTIONS: usize = 8;

struct Completion {
    entry: glib::WeakRef<gtk::Entry>,
    focus: gtk::EventControllerFocus,
    popup: gtk::Popover,
    list: gtk::ListBox,
    field: Field,
    choices: Vec<Choice>,
    shown: RefCell<Vec<usize>>,
    input: RefCell<Option<Input>>,
    dismissed: RefCell<Option<String>>,
    scheduled: Cell<bool>,
    inserting: Cell<bool>,
}

impl Completion {
    fn attach(entry: &gtk::Entry, field: Field, choices: Vec<String>) -> Rc<Self> {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .activate_on_single_click(true)
            .focusable(false)
            .css_classes(["metadata-suggestions"])
            .build();

        let popup = gtk::Popover::builder()
            .child(&list)
            .autohide(false)
            .has_arrow(false)
            .position(gtk::PositionType::Bottom)
            .build();

        popup.set_parent(entry);
        let focus = gtk::EventControllerFocus::new();
        entry.add_controller(focus.clone());

        let completion = Rc::new(Self {
            entry: entry.downgrade(),
            focus,
            popup,
            list,
            field,
            choices: choices.into_iter().map(Choice::new).collect(),
            shown: RefCell::new(Vec::new()),
            input: RefCell::new(None),
            dismissed: RefCell::new(None),
            scheduled: Cell::new(false),
            inserting: Cell::new(false),
        });

        let weak = Rc::downgrade(&completion);

        entry.connect_changed(move |_| {
            if let Some(completion) = weak.upgrade()
                && !completion.inserting.get()
            {
                completion.dismissed.replace(None);
                completion.schedule();
            }
        });

        let weak = Rc::downgrade(&completion);

        entry.connect_cursor_position_notify(move |_| {
            if let Some(completion) = weak.upgrade()
                && completion.popup.is_visible()
                && !completion.inserting.get()
            {
                completion.schedule();
            }
        });

        let weak = Rc::downgrade(&completion);

        completion.focus.connect_leave(move |_| {
            let weak = weak.clone();

            glib::idle_add_local_once(move || {
                if let Some(completion) = weak.upgrade()
                    && !completion.focus.contains_focus()
                {
                    completion.dismiss();
                }
            });
        });

        let weak = Rc::downgrade(&completion);

        completion.list.connect_row_activated(move |_, row| {
            if let Some(completion) = weak.upgrade() {
                completion.accept(row.index());
            }
        });

        completion
    }

    fn read(&self) -> Option<Input> {
        let entry = self.entry.upgrade()?;

        Some(Input {
            text: entry.text().into(),
            cursor: entry.position(),
            selection: entry.selection_bounds(),
        })
    }

    fn schedule(self: &Rc<Self>) {
        if self.scheduled.replace(true) {
            return;
        }

        let weak = Rc::downgrade(self);

        glib::idle_add_local_once(move || {
            if let Some(completion) = weak.upgrade() {
                completion.scheduled.set(false);
                completion.refresh();
            }
        });
    }

    fn refresh(&self) {
        let Some(entry) = self.entry.upgrade().filter(|entry| entry.is_mapped()) else {
            self.popup.popdown();
            return;
        };

        let Some(input) = self.read() else {
            return;
        };

        if !self.focus.contains_focus() || self.dismissed.borrow().as_ref() == Some(&input.text) {
            self.popup.popdown();
            return;
        }

        let shown = Query::new(&input, self.field)
            .map(|query| {
                self.choices
                    .iter()
                    .enumerate()
                    .filter(|(_, choice)| query.matches(&input, choice))
                    .take(MAX_SUGGESTIONS)
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        self.input.replace(Some(input));

        if shown.is_empty() {
            self.shown.borrow_mut().clear();
            self.popup.popdown();
            return;
        }

        if *self.shown.borrow() != shown {
            while let Some(child) = self.list.first_child() {
                self.list.remove(&child);
            }

            for &index in &shown {
                let label = gtk::Label::builder()
                    .label(&self.choices[index].value)
                    .xalign(0.0)
                    .ellipsize(pango::EllipsizeMode::End)
                    .max_width_chars(45)
                    .build();

                label.set_tooltip_text(Some(&self.choices[index].value));
                let row = gtk::ListBoxRow::new();
                row.set_focusable(false);
                row.set_child(Some(&label));
                self.list.append(&row);
            }

            self.shown.replace(shown);
            self.list.select_row(self.list.row_at_index(0).as_ref());
        }

        if self.popup.parent().is_none() {
            self.popup.set_parent(&entry);
        }

        self.popup.set_size_request(entry.width().max(180), -1);
        self.popup.popup();
    }

    fn accept(&self, row: i32) {
        if !self.popup.is_visible() || !self.focus.contains_focus() {
            return;
        }

        let Some(input) = self.read() else {
            return;
        };

        if self.input.borrow().as_ref() != Some(&input) {
            self.refresh();
            return;
        }

        let index = usize::try_from(row)
            .ok()
            .and_then(|row| self.shown.borrow().get(row).copied());

        let (Some(index), Some(query), Some(entry)) =
            (index, Query::new(&input, self.field), self.entry.upgrade())
        else {
            return;
        };

        let (text, cursor) = query.complete(&input, &self.choices[index].value);
        self.inserting.set(true);
        entry.set_text(&text);
        entry.set_position(cursor);
        self.inserting.set(false);
        self.dismiss();
        entry.grab_focus_without_selecting();
    }

    fn dismiss(&self) {
        self.dismissed.replace(self.read().map(|input| input.text));
        self.popup.popdown();
    }

    fn detach(&self) {
        self.dismiss();

        if self.popup.parent().is_some() {
            self.popup.unparent();
        }
    }

    fn key(&self, key: gtk::gdk::Key) -> bool {
        if !self.focus.contains_focus() {
            return false;
        }

        if self.scheduled.get() {
            self.refresh();
        }

        if key == gtk::gdk::Key::Down && !self.popup.is_visible() {
            self.dismissed.replace(None);
            self.refresh();
            return self.popup.is_visible();
        }

        if !self.popup.is_visible() || self.shown.borrow().is_empty() {
            return false;
        }

        match key {
            gtk::gdk::Key::Up | gtk::gdk::Key::Down => {
                let current = self.list.selected_row().map_or(0, |row| row.index());
                let last = self.shown.borrow().len() as i32 - 1;

                let next = if key == gtk::gdk::Key::Up {
                    current - 1
                } else {
                    current + 1
                };

                self.list
                    .select_row(self.list.row_at_index(next.clamp(0, last)).as_ref());
            }
            gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter | gtk::gdk::Key::Tab => {
                if let Some(row) = self.list.selected_row() {
                    self.accept(row.index());
                }
            }
            gtk::gdk::Key::Escape => self.dismiss(),
            _ => return false,
        }

        true
    }
}

pub(super) fn connect(
    window: &gtk::Window,
    group: &gtk::Entry,
    tags: &gtk::Entry,
    group_suggestions: Vec<String>,
    tag_suggestions: Vec<String>,
) {
    let completions = [
        Completion::attach(group, Field::Group, group_suggestions),
        Completion::attach(tags, Field::Tags, tag_suggestions),
    ];

    let cleanup = completions.clone();

    // Manually parented popups must not outlive the dialog's visible host.
    window.connect_unmap(move |_| {
        for completion in &cleanup {
            completion.detach();
        }
    });

    let cleanup = completions.clone();

    window.connect_destroy(move |_| {
        for completion in &cleanup {
            completion.detach();
        }
    });

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = window.downgrade();

    keys.connect_key_pressed(move |_, key, _, modifiers| {
        if (modifiers & gtk::accelerator_get_default_mod_mask()).is_empty() {
            if completions.iter().any(|completion| completion.key(key)) {
                return glib::Propagation::Stop;
            }

            if key == gtk::gdk::Key::Escape {
                if let Some(window) = weak.upgrade() {
                    window.close();
                }

                return glib::Propagation::Stop;
            }
        }

        glib::Propagation::Proceed
    });

    window.add_controller(keys);
}

#[cfg(test)]
mod tests;
