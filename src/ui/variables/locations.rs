//! Lazy location presentation shared by both variable tables and their menus.

use super::VariableNode;
use crate::debugger::{
    Variable,
    location::{BATCH_LIMIT, LocationReply, ValueLocation},
};
use gtk::{glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::{Rc, Weak},
};

type Handler = Rc<dyn Fn(Vec<Variable>, Rc<dyn Fn() -> bool>, LocationReply)>;
type Validate = Rc<dyn Fn(&Variable) -> bool>;
type OpenMemory = Rc<dyn Fn(u64)>;
const CACHE_LIMIT: usize = 1024;

pub(in crate::ui) struct Locations {
    enabled: Cell<bool>,
    pointer_bits: Rc<Cell<u32>>,
    context: Cell<Option<(u64, u64)>>,
    epoch: Cell<u64>,
    active: Cell<bool>,
    scheduled: Cell<bool>,
    cache: RefCell<HashMap<Variable, ValueLocation>>,
    wanted: RefCell<HashSet<Variable>>,
    items: RefCell<Vec<glib::WeakRef<gtk::ListItem>>>,
    columns: RefCell<Vec<glib::WeakRef<gtk::ColumnViewColumn>>>,
    menus: RefCell<Vec<Weak<LocationMenu>>>,
    handler: RefCell<Option<Handler>>,
    validate: RefCell<Option<Validate>>,
    open_memory: RefCell<Option<OpenMemory>>,
}

mod menu;
use menu::LocationMenu;

impl Locations {
    pub(in crate::ui) fn new(enabled: bool, pointer_bits: Rc<Cell<u32>>) -> Rc<Self> {
        Rc::new(Self {
            enabled: Cell::new(enabled),
            pointer_bits,
            context: Cell::new(None),
            epoch: Cell::new(0),
            active: Cell::new(false),
            scheduled: Cell::new(false),
            cache: RefCell::default(),
            wanted: RefCell::default(),
            items: RefCell::default(),
            columns: RefCell::default(),
            menus: RefCell::default(),
            handler: RefCell::new(None),
            validate: RefCell::new(None),
            open_memory: RefCell::new(None),
        })
    }

    pub(in crate::ui) fn connect(
        self: &Rc<Self>,
        handler: Handler,
        validate: Validate,
        open_memory: OpenMemory,
    ) {
        self.handler.replace(Some(handler));
        self.validate.replace(Some(validate));
        self.open_memory.replace(Some(open_memory));
        self.schedule();
    }

    pub(in crate::ui) fn register_column(&self, column: &gtk::ColumnViewColumn) {
        column.set_visible(self.enabled.get());
        self.columns.borrow_mut().push(column.downgrade());
    }

    pub(in crate::ui) fn watch_scrolling(self: &Rc<Self>, view: &gtk::ColumnView) {
        use crate::ui::lifecycle::SignalSubscription;
        let subscriptions = Rc::new(RefCell::new(Vec::<SignalSubscription>::new()));
        let mapped = Rc::clone(&subscriptions);
        let weak = Rc::downgrade(self);

        view.connect_map(move |view| {
            mapped.borrow_mut().clear();

            if let Some(scroller) = scroller(view.upcast_ref()) {
                for adjustment in [scroller.vadjustment(), scroller.hadjustment()] {
                    let changed = weak.clone();
                    let handler = adjustment.connect_value_changed(move |_| {
                        if let Some(locations) = changed.upgrade() {
                            locations.schedule();
                        }
                    });

                    mapped
                        .borrow_mut()
                        .push(SignalSubscription::new(&adjustment, handler));

                    // Resizing can reveal recycled rows without mapping them
                    // again or changing the scroll offset.
                    let resized = weak.clone();
                    let handler = adjustment.connect_changed(move |_| {
                        if let Some(locations) = resized.upgrade() {
                            locations.schedule();
                        }
                    });

                    mapped
                        .borrow_mut()
                        .push(SignalSubscription::new(&adjustment, handler));
                }
            }

            if let Some(locations) = weak.upgrade() {
                locations.schedule();
            }
        });

        view.connect_unmap(move |_| subscriptions.borrow_mut().clear());
    }

    pub(in crate::ui) fn register(self: &Rc<Self>, item: &gtk::ListItem, label: &gtk::Label) {
        let mut items = self.items.borrow_mut();

        if items.len() == items.capacity() {
            items.retain(|item| item.upgrade().is_some());
        }

        items.push(item.downgrade());
        drop(items);
        let weak = Rc::downgrade(self);
        label.connect_map(move |_| {
            if let Some(locations) = weak.upgrade() {
                locations.schedule();
            }
        });

        let weak = Rc::downgrade(self);
        label.connect_unmap(move |_| {
            if let Some(locations) = weak.upgrade() {
                locations.schedule();
            }
        });
    }

    pub(in crate::ui) fn set_enabled(self: &Rc<Self>, enabled: bool) {
        if self.enabled.replace(enabled) == enabled {
            return;
        }

        self.cancel();
        let columns: Vec<_> = self
            .columns
            .borrow()
            .iter()
            .filter_map(glib::WeakRef::upgrade)
            .collect();

        for column in columns {
            column.set_visible(enabled);
        }

        self.schedule();
    }

    pub(in crate::ui) fn set_context(self: &Rc<Self>, context: Option<(u64, u64)>) {
        if self.context.replace(context) != context {
            self.cancel();
            self.cache.borrow_mut().clear();
        }

        self.schedule();
    }

    fn cancel(&self) {
        self.epoch.set(self.epoch.get().wrapping_add(1));
        self.active.set(false);
        self.wanted.borrow_mut().clear();
    }

    pub(in crate::ui) fn schedule(self: &Rc<Self>) {
        if !self.enabled.get()
            && self
                .menus
                .borrow()
                .iter()
                .all(|menu| menu.strong_count() == 0)
        {
            return;
        }

        if self.scheduled.replace(true) {
            return;
        }

        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(locations) = weak.upgrade() {
                locations.scheduled.set(false);
                locations.drive();
            }
        });
    }

    fn can_inspect(&self, variable: &Variable) -> bool {
        let validate = self.validate.borrow().clone();
        self.context.get().is_some() && validate.is_some_and(|validate| validate(variable))
    }

    fn cached(&self, variable: &Variable) -> Option<ValueLocation> {
        self.can_inspect(variable)
            .then(|| self.cache.borrow().get(variable).cloned())
            .flatten()
    }

    fn render(&self, label: &gtk::Label, variable: &Variable, current: bool) {
        let result = current
            .then(|| self.cache.borrow().get(variable).cloned())
            .flatten();

        let (text, tooltip) = presentation(result.as_ref(), current, self.pointer_bits.get());

        if label.text().as_str() != text {
            crate::ui::views::clear_label_selection(label);
            label.set_text(&text);
        }

        if label.tooltip_text().as_deref() != Some(tooltip.as_str()) {
            label.set_tooltip_text(Some(&tooltip));
        }
    }

    fn drive(self: &Rc<Self>) {
        let mut wanted = HashSet::new();

        if self.enabled.get() {
            let items: Vec<_> = self
                .items
                .borrow()
                .iter()
                .filter_map(glib::WeakRef::upgrade)
                .collect();

            for item in items {
                let Some(label) = item.child().and_downcast::<gtk::Label>().filter(visible) else {
                    continue;
                };

                let Some(data) = item
                    .item()
                    .and_downcast::<gtk::TreeListRow>()
                    .and_then(|row| row.item())
                    .and_downcast::<glib::BoxedAnyObject>()
                else {
                    continue;
                };

                let node = data.borrow::<VariableNode>();

                if node.placeholder {
                    label.set_text("");
                    continue;
                }

                let variable = node.variable.clone();
                drop(node);
                let current = self.can_inspect(&variable);
                self.render(&label, &variable, current);

                if current {
                    wanted.insert(variable);
                }
            }
        }

        self.menus
            .borrow_mut()
            .retain(|menu| menu.strong_count() > 0);

        let menus: Vec<_> = self
            .menus
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();

        for menu in menus {
            if let Some(label) = menu.label.upgrade().filter(|label| label.is_mapped()) {
                self.render(&label, &menu.variable, self.can_inspect(&menu.variable));
                let address = self
                    .cached(&menu.variable)
                    .and_then(|location| location.address());

                if let Some(retry) = menu.retry.upgrade() {
                    retry.set_sensitive(matches!(
                        self.cached(&menu.variable),
                        Some(ValueLocation::Unknown(_))
                    ));
                }

                for button in [&menu.copy, &menu.inspect]
                    .into_iter()
                    .filter_map(glib::WeakRef::upgrade)
                {
                    button.set_sensitive(address.is_some());
                }

                if self.can_inspect(&menu.variable) {
                    wanted.insert(menu.variable.clone());
                }
            }
        }

        if self.cache.borrow().len() + BATCH_LIMIT > CACHE_LIMIT {
            self.cache
                .borrow_mut()
                .retain(|variable, _| wanted.contains(variable));
        }

        self.wanted.replace(wanted);

        if self.active.get() {
            return;
        }

        let Some(handler) = self.handler.borrow().clone() else {
            return;
        };

        let variables: Vec<_> = self
            .wanted
            .borrow()
            .iter()
            .filter(|variable| !self.cache.borrow().contains_key(*variable))
            .take(BATCH_LIMIT.min(CACHE_LIMIT.saturating_sub(self.cache.borrow().len())))
            .cloned()
            .collect();

        if variables.is_empty() {
            return;
        }

        self.active.set(true);
        let epoch = self.epoch.get();
        let expected = variables.clone();
        let weak = Rc::downgrade(self);
        let current: Rc<dyn Fn() -> bool> = Rc::new(move || {
            weak.upgrade().is_some_and(|locations| {
                locations.epoch.get() == epoch
                    && expected.iter().all(|variable| {
                        locations.wanted.borrow().contains(variable)
                            && locations.can_inspect(variable)
                    })
            })
        });

        let guard = Rc::clone(&current);
        let weak = Rc::downgrade(self);
        let requested = variables.clone();

        handler(
            variables,
            current,
            Box::new(move |result| {
                let Some(locations) = weak
                    .upgrade()
                    .filter(|locations| locations.epoch.get() == epoch)
                else {
                    return;
                };

                locations.active.set(false);
                let retry_changed = !guard();

                if let Some(result) = result {
                    if result.len() == requested.len() && !retry_changed {
                        locations
                            .cache
                            .borrow_mut()
                            .extend(requested.into_iter().zip(result));
                    } else if !retry_changed {
                        locations
                            .cache
                            .borrow_mut()
                            .extend(requested.into_iter().map(|variable| {
                                (
                                    variable,
                                    ValueLocation::Unknown("Incomplete location response".into()),
                                )
                            }));
                    }

                    locations.schedule();
                } else if retry_changed {
                    locations.schedule();
                }
            }),
        );
    }
}

fn scroller(widget: &gtk::Widget) -> Option<gtk::ScrolledWindow> {
    widget
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_downcast::<gtk::ScrolledWindow>()
}

fn visible(label: &gtk::Label) -> bool {
    if !label.is_mapped() {
        return false;
    }

    let Some(scroller) = scroller(label.upcast_ref()) else {
        return true;
    };

    let Some(bounds) = label.compute_bounds(&scroller) else {
        return false;
    };

    bounds.x() < scroller.width() as f32
        && bounds.y() < scroller.height() as f32
        && bounds.x() + bounds.width() > 0.0
        && bounds.y() + bounds.height() > 0.0
}

fn presentation(location: Option<&ValueLocation>, current: bool, bits: u32) -> (String, String) {
    let (text, detail) = match location {
        Some(ValueLocation::Memory {
            address,
            referenced,
        }) => {
            let width = (bits / 4).clamp(1, 16) as usize;
            let text = format!(
                "0x{address:0width$x}{}",
                if *referenced { " (referent)" } else { "" }
            );

            let detail = if *referenced {
                "Address of the referenced object, not storage for the reference itself"
            } else {
                "Storage address reported by GDB. For a pointer this is the pointer's own storage, not its target. Bitfields may share a storage unit"
            };

            return (text, detail.into());
        }
        Some(ValueLocation::NonAddressable) => (
            "No address",
            "GDB reports no addressable storage. This does not identify a register. Computed and synthetic values may also have no address",
        ),
        Some(ValueLocation::OptimizedOut) => (
            "Optimized out",
            "The compiler optimized out this value or part of it",
        ),
        Some(ValueLocation::Unavailable) => (
            "Unavailable",
            "GDB cannot access the target state needed to locate this value",
        ),
        Some(ValueLocation::Unknown(detail)) => return ("Not resolved".into(), detail.clone()),
        None if current => ("…", "Resolving storage at the current stop"),
        None => ("—", "A current paused value is required"),
    };

    (text.into(), detail.into())
}

impl crate::ui::Ui {
    pub(crate) fn connect_variable_locations(
        self: &Rc<Self>,
        handler: impl Fn(Vec<Variable>, Rc<dyn Fn() -> bool>, LocationReply) + 'static,
    ) {
        let validate = Rc::downgrade(self);
        let open_memory = Rc::downgrade(self);

        self.variable_locations.connect(
            Rc::new(handler),
            Rc::new(move |variable| {
                validate
                    .upgrade()
                    .is_some_and(|ui| ui.variable_action_is_current(variable))
            }),
            Rc::new(move |address| {
                let Some(ui) = open_memory.upgrade() else {
                    return;
                };

                if crate::ui::memory_view::add_memory_watch(
                    &ui.memory_watch_container,
                    &ui.memory_watches,
                    &ui.memory_watch_handler,
                    format!("0x{address:x}"),
                    128,
                    crate::ui::MemoryWatchFormat::Bytes,
                ) {
                    ui.memory_search.show_inspector();
                    ui.panels.reveal(crate::ui::PanelId::Memory);
                } else {
                    ui.set_status(
                        "Memory watch limit",
                        "Remove a memory watch before adding another (limit 256)",
                        Some("status-error"),
                    );
                }
            }),
        );
    }
}

#[cfg(test)]
mod tests;
