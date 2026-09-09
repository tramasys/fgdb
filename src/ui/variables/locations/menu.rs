//! Location details and actions in the shared variable context menu.

use super::*;

pub(super) struct LocationMenu {
    pub(super) variable: Variable,
    pub(super) label: glib::WeakRef<gtk::Label>,
    pub(super) copy: glib::WeakRef<gtk::Button>,
    pub(super) inspect: glib::WeakRef<gtk::Button>,
    pub(super) retry: glib::WeakRef<gtk::Button>,
}

impl Locations {
    pub(in crate::ui) fn append_menu(
        self: &Rc<Self>,
        menu: &gtk::Box,
        popover: &gtk::Popover,
        variable: &Variable,
    ) {
        use crate::ui::build::context_menu_action;
        use crate::ui::views::{enable_recycled_text_selection, variable_menu_summary};

        menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let summary = variable_menu_summary("LOCATION");
        let label = gtk::Label::new(Some("…"));
        label.add_css_class("local-variable-menu-value");
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        label.set_max_width_chars(40);
        enable_recycled_text_selection(&label);
        let copy = context_menu_action("Copy address");
        let inspect = context_menu_action("Show in memory");
        let retry = context_menu_action("Retry location");
        // Keep the menu's height stable as asynchronous results arrive. Adding
        // an action to a mapped autohiding popover can dismiss it on resize.
        retry.set_sensitive(false);
        copy.set_sensitive(false);
        inspect.set_sensitive(false);
        let lease = Rc::new(LocationMenu {
            variable: variable.clone(),
            label: label.downgrade(),
            copy: copy.downgrade(),
            inspect: inspect.downgrade(),
            retry: retry.downgrade(),
        });

        self.menus.borrow_mut().push(Rc::downgrade(&lease));
        let lease = RefCell::new(Some(lease));
        let weak = Rc::downgrade(self);
        popover.connect_closed(move |_| {
            lease.borrow_mut().take();
            if let Some(locations) = weak.upgrade() {
                locations.schedule();
            }
        });

        for (button, open) in [(&copy, false), (&inspect, true)] {
            let variable = variable.clone();
            let weak = Rc::downgrade(self);
            let popover = popover.downgrade();

            button.connect_clicked(move |button| {
                let Some(locations) = weak.upgrade() else {
                    return;
                };

                let Some(address) = locations
                    .cached(&variable)
                    .and_then(|location| location.address())
                else {
                    return;
                };

                if open {
                    let handler = locations.open_memory.borrow().clone();
                    if let Some(handler) = handler {
                        handler(address);
                    }
                } else {
                    button
                        .display()
                        .clipboard()
                        .set_text(&format!("0x{address:x}"));
                }

                if let Some(popover) = popover.upgrade() {
                    popover.popdown();
                }
            });
        }

        summary.append(&label);
        menu.append(&summary);
        menu.append(&copy);
        menu.append(&inspect);
        let weak = Rc::downgrade(self);
        let variable = variable.clone();

        retry.connect_clicked(move |_| {
            if let Some(locations) = weak.upgrade() {
                locations.cache.borrow_mut().remove(&variable);
                locations.schedule();
            }
        });

        menu.append(&retry);
        self.schedule();
    }
}
