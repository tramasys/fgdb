use super::*;
use crate::debugger::linked::{
    LinkedListAction, LinkedListProgress, LinkedListQuery, NODE_LIMIT, PAGE_LIMIT,
};

type Handler = Rc<dyn Fn(LinkedListAction)>;

#[cfg(test)]
mod tests;

pub(super) struct LinkedControls {
    pub root: gtk::Box,
    member: gtk::Entry,
    size: gtk::SpinButton,
    first: gtk::Button,
    previous: gtk::Button,
    next: gtk::Button,
    apply: gtk::Button,
    cancel: gtk::Button,
    position: gtk::Label,
    description: gtk::Label,
    limit: usize,
    dirty: Cell<bool>,
    progress: Cell<LinkedListProgress>,
    handler: RefCell<Option<Handler>>,
}

impl LinkedControls {
    pub(super) fn new(limit: usize) -> Rc<Self> {
        let limit = limit.clamp(1, PAGE_LIMIT);
        let root = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
        let links = components::control_row();
        let member = gtk::Entry::builder()
            .hexpand(true)
            .max_length(128)
            .placeholder_text("Automatic (next, link, …)")
            .tooltip_text("Direct link field. Use prev to follow backward links. Leave empty for automatic detection")
            .build();

        links.append(&components::section_title("LINK MEMBER"));
        links.append(&member);
        let apply = gtk::Button::with_label("Restart");
        apply.set_tooltip_text(Some(
            "Start at the original root using these settings, discarding the cached traversal",
        ));

        links.append(&apply);
        root.append(&links);
        let toolbar = components::control_row();
        let first = components::navigation_button("go-first-symbolic", "First cached page");
        let previous = components::navigation_button(
            "go-previous-symbolic",
            "Previous cached page, without target reads",
        );

        let next = components::navigation_button(
            "go-next-symbolic",
            "Next page, following more links only when needed",
        );

        toolbar.append(&components::control_group(
            "PAGE",
            &[
                first.clone().upcast(),
                previous.clone().upcast(),
                next.clone().upcast(),
            ],
        ));

        let size = gtk::SpinButton::with_range(1.0, limit as f64, 1.0);
        size.set_value(limit as f64);
        size.set_tooltip_text(Some("Nodes per page. Restart to apply changes"));
        toolbar.append(&components::control_group("SIZE", &[size.clone().upcast()]));
        let position = gtk::Label::builder()
            .hexpand(true)
            .xalign(0.0)
            .css_classes(["muted"])
            .build();

        toolbar.append(&position);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.set_tooltip_text(Some(
            "Stop following links and retain the nodes already read",
        ));

        toolbar.append(&cancel);
        root.append(&toolbar);
        let description = gtk::Label::new(Some(&format!(
            "Pages are cached at the opening stop · {NODE_LIMIT} node safety limit · Filter applies to the displayed page"
        )));

        description.set_wrap(true);
        description.set_xalign(0.0);
        description.add_css_class("muted");
        root.append(&description);

        let controls = Rc::new(Self {
            root,
            member,
            size,
            first,
            previous,
            next,
            apply,
            cancel,
            position,
            description,
            limit,
            dirty: Cell::new(false),
            progress: Cell::new(LinkedListProgress::default()),
            handler: RefCell::new(None),
        });

        for (button, action) in [
            (&controls.first, LinkedListAction::First),
            (&controls.previous, LinkedListAction::Previous),
            (&controls.next, LinkedListAction::Next),
            (&controls.cancel, LinkedListAction::Cancel),
        ] {
            let weak = Rc::downgrade(&controls);

            button.connect_clicked(move |_| {
                if let Some(controls) = weak.upgrade() {
                    controls.emit(action.clone());
                }
            });
        }

        let weak = Rc::downgrade(&controls);
        controls.apply.connect_clicked(move |_| {
            if let Some(controls) = weak.upgrade() {
                controls.restart();
            }
        });

        let weak = Rc::downgrade(&controls);
        controls.member.connect_activate(move |_| {
            if let Some(controls) = weak.upgrade() {
                controls.restart();
            }
        });

        let weak = Rc::downgrade(&controls);
        controls.member.connect_changed(move |_| {
            if let Some(controls) = weak.upgrade() {
                controls.mark_dirty();
            }
        });

        let weak = Rc::downgrade(&controls);
        controls.size.connect_value_changed(move |_| {
            if let Some(controls) = weak.upgrade() {
                controls.mark_dirty();
            }
        });

        controls.update_buttons();
        controls
    }

    fn emit(&self, action: LinkedListAction) {
        let progress = self.progress.get();

        if progress.busy && !matches!(action, LinkedListAction::Cancel) {
            return;
        }

        let handler = self.handler.borrow().clone();

        if let Some(handler) = handler {
            handler(action);
        }
    }

    fn mark_dirty(&self) {
        self.dirty.set(true);
        self.description.remove_css_class("status-error");
        self.description
            .set_text("Settings changed · Restart to apply the link member and page size");
        self.update_buttons();
    }

    fn restart(&self) {
        let query = LinkedListQuery {
            member: self.member.text().trim().to_owned(),
            page_size: self.size.value_as_int() as usize,
        };

        if let Err(error) = query.validate(self.limit) {
            self.description.set_text(error);
            self.description.add_css_class("status-error");
            return;
        }

        self.emit(LinkedListAction::Restart(query));
    }

    pub(super) fn connect(&self, handler: impl Fn(LinkedListAction) + 'static) {
        self.handler.replace(Some(Rc::new(handler)));
        self.restart();
    }

    pub(super) fn begin(&self) {
        self.dirty.set(false);
        self.description.remove_css_class("status-error");
        self.description.set_text(&format!("Pages are cached at the opening stop · {NODE_LIMIT} node safety limit · Filter applies to the displayed page"));
        self.update(LinkedListProgress {
            busy: true,
            ..LinkedListProgress::default()
        });
    }

    pub(super) fn update(&self, progress: LinkedListProgress) {
        self.progress.set(progress);
        let end = progress.offset + progress.shown;
        let first = if progress.shown == 0 {
            end
        } else {
            progress.offset + 1
        };

        self.position
            .set_text(&format!("{first}–{end} · {} cached", progress.cached));

        self.update_buttons();
    }

    fn update_buttons(&self) {
        let progress = self.progress.get();
        let ready = self.handler.borrow().is_some();
        let idle = !progress.busy;
        set_execution_sensitive(&self.member, ready && idle, ready && !idle);
        set_execution_sensitive(&self.size, ready && idle, ready && !idle);
        set_transient_execution_sensitive(&self.apply, ready && idle, ready && !idle);
        self.cancel.set_sensitive(ready && !idle);
        self.first.set_sensitive(idle && progress.offset > 0);
        self.previous.set_sensitive(idle && progress.offset > 0);
        self.next.set_sensitive(
            idle && !self.dirty.get()
                && (progress.offset + progress.shown < progress.cached || progress.can_continue),
        );
    }
}
