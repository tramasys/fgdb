use super::*;
use crate::debugger::array::{ArrayPage, ArrayShape, ArraySlice, AxisRange, PAGE_LIMIT};

#[cfg(test)]
mod tests;

type QueryHandler = Rc<dyn Fn(ArrayPage)>;

pub(super) struct ArrayControls {
    pub(super) root: gtk::Box,
    dimensions: gtk::Grid,
    description: gtk::Label,
    position: gtk::Label,
    size: gtk::SpinButton,
    apply: gtk::Button,
    reset: gtk::Button,
    previous: gtk::Button,
    next: gtk::Button,
    cancel: gtk::Button,
    shape: RefCell<Option<ArrayShape>>,
    entries: RefCell<Vec<[gtk::Entry; 3]>>,
    applied: RefCell<Option<ArrayPage>>,
    next_offset: Cell<Option<u64>>,
    busy: Cell<bool>,
    dirty: Cell<bool>,
    handler: RefCell<Option<QueryHandler>>,
}

impl ArrayControls {
    pub(super) fn new(limit: usize) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
        root.set_visible(false);
        let description = gtk::Label::new(None);
        description.set_xalign(0.0);
        description.set_wrap(true);
        description.add_css_class("muted");
        root.append(&description);

        let dimensions = gtk::Grid::builder()
            .column_spacing(components::CONTROL_GAP)
            .row_spacing(components::CONTROL_GAP)
            .build();

        let scroll = gtk::ScrolledWindow::builder()
            .child(&dimensions)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .overlay_scrolling(false)
            .propagate_natural_height(true)
            .max_content_height(190)
            .build();

        root.append(&scroll);
        let toolbar = components::control_row();
        let previous = components::navigation_button("go-previous-symbolic", "Previous slice page");
        let next = components::navigation_button("go-next-symbolic", "Next slice page");
        let size = gtk::SpinButton::with_range(1.0, limit.clamp(1, PAGE_LIMIT) as f64, 1.0);
        size.set_value(limit.clamp(1, PAGE_LIMIT) as f64);
        size.set_tooltip_text(Some(
            "Maximum elements per page, read in cancellable batches",
        ));

        toolbar.append(&components::control_group(
            "PAGE",
            &[previous.clone().upcast(), next.clone().upcast()],
        ));

        toolbar.append(&components::control_group("SIZE", &[size.clone().upcast()]));
        let position = gtk::Label::new(None);
        position.set_hexpand(true);
        position.set_xalign(0.0);
        position.add_css_class("muted");
        toolbar.append(&position);
        let reset = gtk::Button::with_label("Full range");
        let apply = gtk::Button::with_label("Apply");
        let cancel = gtk::Button::with_label("Cancel");

        for button in [&reset, &apply, &cancel] {
            toolbar.append(button);
        }

        root.append(&toolbar);

        let controls = Rc::new(Self {
            root,
            dimensions,
            description,
            position,
            size,
            apply,
            reset,
            previous,
            next,
            cancel,
            shape: RefCell::new(None),
            entries: RefCell::new(Vec::new()),
            applied: RefCell::new(None),
            next_offset: Cell::new(None),
            busy: Cell::new(false),
            dirty: Cell::new(false),
            handler: RefCell::new(None),
        });

        let weak = Rc::downgrade(&controls);

        controls.apply.connect_clicked(move |_| {
            if let Some(controls) = weak.upgrade() {
                controls.submit(0);
            }
        });

        let weak = Rc::downgrade(&controls);

        controls.reset.connect_clicked(move |_| {
            if let Some(controls) = weak.upgrade() {
                controls.reset_entries();
                controls.submit(0);
            }
        });

        for (button, forward) in [(&controls.previous, false), (&controls.next, true)] {
            let weak = Rc::downgrade(&controls);

            button.connect_clicked(move |_| {
                let Some(controls) = weak.upgrade() else {
                    return;
                };
                let offset = if forward {
                    controls.next_offset.get()
                } else {
                    controls
                        .applied
                        .borrow()
                        .as_ref()
                        .map(|page| page.offset.saturating_sub(page.count as u64))
                };

                if let Some(offset) = offset {
                    controls.submit(offset);
                }
            });
        }

        let weak = Rc::downgrade(&controls);

        controls.size.connect_value_changed(move |_| {
            if let Some(controls) = weak.upgrade() {
                controls.changed();
            }
        });

        controls.update_buttons();
        controls
    }

    pub(super) fn configure(self: &Rc<Self>, shape: ArrayShape) -> Result<(), &'static str> {
        shape.full_slice()?;
        self.description.set_text(&shape.description());
        self.shape.replace(Some(shape));

        while let Some(child) = self.dimensions.first_child() {
            self.dimensions.remove(&child);
        }

        self.entries.borrow_mut().clear();

        for (column, title) in ["DIMENSION / BOUNDS", "START", "COUNT", "STRIDE"]
            .iter()
            .enumerate()
        {
            let label = components::section_title(title);
            self.dimensions.attach(&label, column as i32, 0, 1, 1);
        }

        let bounds = self.shape.borrow().as_ref().unwrap().bounds.clone();

        for (index, (lower, upper)) in bounds.iter().enumerate() {
            let label = gtk::Label::new(Some(&format!("{} · {lower}:{upper}", index + 1)));
            label.set_xalign(0.0);
            self.dimensions.attach(&label, 0, index as i32 + 1, 1, 1);

            let entries = std::array::from_fn(|column| {
                let entry = gtk::Entry::builder()
                    .hexpand(true)
                    .width_chars(10)
                    .max_length(21)
                    .build();
                entry.set_tooltip_text(Some(match column {
                    0 => "First native index in this dimension",
                    1 => "Number of selected indices. Use 1 to fix this dimension",
                    _ => "Non-zero index step. Negative strides reverse this dimension",
                }));

                let weak = Rc::downgrade(self);

                entry.connect_changed(move |_| {
                    if let Some(controls) = weak.upgrade() {
                        controls.changed();
                    }
                });

                let weak = Rc::downgrade(self);

                entry.connect_activate(move |_| {
                    if let Some(controls) = weak.upgrade() {
                        controls.submit(0);
                    }
                });

                self.dimensions
                    .attach(&entry, column as i32 + 1, index as i32 + 1, 1, 1);
                entry
            });

            self.entries.borrow_mut().push(entries);
        }

        self.reset_entries();
        self.root.set_visible(true);
        Ok(())
    }

    fn reset_entries(&self) {
        let slice = self
            .shape
            .borrow()
            .as_ref()
            .and_then(|shape| shape.full_slice().ok());

        if let Some(slice) = slice {
            for (entries, axis) in self.entries.borrow().iter().zip(slice.axes) {
                entries[0].set_text(&axis.start.to_string());
                entries[1].set_text(&axis.count.to_string());
                entries[2].set_text(&axis.stride.to_string());
            }
        }
    }

    fn changed(&self) {
        self.dirty.set(true);
        self.update_buttons();
    }

    fn query(&self, offset: u64) -> Result<ArrayPage, &'static str> {
        let axes = self
            .entries
            .borrow()
            .iter()
            .map(|entries| {
                Ok(AxisRange {
                    start: entries[0]
                        .text()
                        .trim()
                        .parse()
                        .map_err(|_| "Start must be a signed integer")?,
                    count: entries[1]
                        .text()
                        .trim()
                        .parse()
                        .map_err(|_| "Count must be a non-negative integer")?,
                    stride: entries[2]
                        .text()
                        .trim()
                        .parse()
                        .map_err(|_| "Stride must be a non-zero signed integer")?,
                })
            })
            .collect::<Result<Vec<_>, &'static str>>()?;

        let page = ArrayPage {
            slice: ArraySlice { axes },
            offset,
            count: self.size.value_as_int() as usize,
        };
        page.validate(
            self.shape
                .borrow()
                .as_ref()
                .ok_or("Array bounds are not available")?,
        )?;
        Ok(page)
    }

    pub(super) fn submit(&self, offset: u64) {
        if self.busy.get() {
            return;
        }

        match self.query(offset) {
            Ok(page) => {
                self.description.remove_css_class("status-error");

                if let Some(shape) = self.shape.borrow().as_ref() {
                    self.description.set_text(&shape.description());
                }

                let handler = self.handler.borrow().clone();

                if let Some(handler) = handler {
                    handler(page);
                }
            }
            Err(error) => {
                self.description.add_css_class("status-error");
                self.description.set_text(error);
            }
        }
    }

    pub(super) fn connect_query(&self, handler: impl Fn(ArrayPage) + 'static) {
        self.handler.replace(Some(Rc::new(handler)));
    }

    pub(super) fn connect_cancel(&self, handler: impl Fn() + 'static) {
        self.cancel.connect_clicked(move |_| handler());
    }

    pub(super) fn begin(&self, page: &ArrayPage) {
        self.applied.replace(Some(page.clone()));
        self.next_offset.set(None);
        self.dirty.set(false);
        self.busy.set(true);
        self.update_buttons();
    }

    pub(super) fn complete(&self, shown: usize, ended: bool) {
        self.busy.set(false);

        if let Some(page) = self.applied.borrow().as_ref() {
            let next = page.offset.saturating_add(shown as u64);
            let total = page.slice.total().unwrap_or(0);
            self.position.set_text(&format!(
                "{}–{next} / {total}",
                if shown == 0 { next } else { page.offset + 1 }
            ));
            self.next_offset
                .set((!ended && shown > 0 && next < total).then_some(next));
        }

        self.update_buttons();
    }

    fn update_buttons(&self) {
        let ready = self.shape.borrow().is_some();
        let idle = !self.busy.get();
        set_execution_sensitive(&self.dimensions, idle, !idle);
        set_execution_sensitive(&self.size, idle, !idle);
        set_transient_execution_sensitive(&self.apply, ready && idle, ready && !idle);
        set_execution_sensitive(&self.reset, ready && idle, ready && !idle);
        self.cancel.set_sensitive(!idle);
        self.previous.set_sensitive(
            idle && !self.dirty.get()
                && self
                    .applied
                    .borrow()
                    .as_ref()
                    .is_some_and(|page| page.offset > 0),
        );
        self.next
            .set_sensitive(idle && !self.dirty.get() && self.next_offset.get().is_some());
    }
}
