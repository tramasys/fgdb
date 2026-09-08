//! Shared presentation controls. Keep debugger policy and request lifetimes in
//! their existing owners, so a visual change cannot change command semantics.

use super::*;
use gtk::gdk;

mod models;
pub(super) use models::{replace_boxed_store, replace_boxed_store_if_changed};

pub(super) const CONTROL_GAP: i32 = 6;
pub(super) const CONTENT_INSET: i32 = 8;
pub(super) const DIALOG_INSET: i32 = 12;

/// Pane chrome and tables meet at their edges. Content owns its own inset.
pub(super) fn panel() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .css_classes(["panel"])
        .build()
}

pub(super) fn dynamic_list(empty_text: &str) -> gtk::Box {
    let list = gtk::Box::new(gtk::Orientation::Vertical, 1);
    list.append(&empty_label(empty_text));

    list
}

pub(super) fn empty_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("muted");
    label.add_css_class("pane-note");
    label.set_halign(gtk::Align::Fill);
    label.set_xalign(0.0);
    label.set_wrap(true);

    label
}

pub(super) fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

pub(super) fn inset(widget: &impl IsA<gtk::Widget>, margin: i32) {
    widget.set_margin_top(margin);
    widget.set_margin_bottom(margin);
    widget.set_margin_start(margin);
    widget.set_margin_end(margin);
}

pub(super) fn section_title(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes(["section-title"])
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .build()
}

pub(super) fn control_row() -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, CONTROL_GAP)
}

pub(super) fn icon_label(icon: &str, label: &str) -> gtk::Box {
    let row = control_row();
    row.set_halign(gtk::Align::Center);
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(14);
    row.append(&image);
    row.append(&gtk::Label::new(Some(label)));

    row
}

/// Shared title-bar controls that do not depend on the desktop's icon theme.
pub(super) fn window_controls(window: &impl IsA<gtk::Window>) -> gtk::Box {
    let window = window.as_ref();
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    controls.add_css_class("window-controls");
    let minimize = window_control_button("−", "Minimize", "minimize");
    let maximize = window_control_button("□", "Maximize or restore", "maximize");
    let close = window_control_button("×", "Close", "close");
    let weak = window.downgrade();

    minimize.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.minimize();
        }
    });

    let weak = window.downgrade();

    maximize.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            if window.is_maximized() {
                window.unmaximize();
            } else {
                window.maximize();
            }
        }
    });

    let weak = window.downgrade();

    close.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });

    controls.append(&minimize);
    controls.append(&maximize);
    controls.append(&close);

    controls
}

fn window_control_button(label: &str, tooltip: &str, class: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("window-control");
    button.add_css_class(class);
    button.set_focus_on_click(false);
    button.set_tooltip_text(Some(tooltip));
    button.update_property(&[gtk::accessible::Property::Label(tooltip)]);

    button
}

/// Keep an outer popover dismissible after a nested dropdown releases its grab.
/// GTK can leave the outer surface visible while routing input to the window.
pub(super) fn keep_popover_dismissible(window: &gtk::ApplicationWindow, popover: &gtk::Popover) {
    let weak = popover.downgrade();
    let events = gtk::EventControllerLegacy::new();
    events.set_propagation_phase(gtk::PropagationPhase::Capture);

    // A retained popover can still be the event target for an outside click.
    // Receive that event across its native-surface boundary.
    events.set_propagation_limit(gtk::PropagationLimit::None);

    events.connect_event(move |_, event| {
        let dismiss = matches!(
            event.event_type(),
            gdk::EventType::ButtonPress | gdk::EventType::TouchBegin
        ) || event.downcast_ref::<gdk::KeyEvent>().is_some_and(|key| {
            event.event_type() == gdk::EventType::KeyPress && key.keyval() == gdk::Key::Escape
        });

        if !dismiss {
            return glib::Propagation::Proceed;
        }

        let Some(popover) = weak.upgrade().filter(|popover| popover.is_visible()) else {
            return glib::Propagation::Proceed;
        };

        let Some(popover_surface) = popover.surface() else {
            return glib::Propagation::Proceed;
        };

        let Some(mut surface) = event.surface() else {
            return glib::Propagation::Proceed;
        };

        loop {
            // Dropdowns have their own native surfaces. Their events are still
            // inside this popover and must reach the dropdown unchanged.
            if surface == popover_surface {
                return glib::Propagation::Proceed;
            }

            let Some(parent) = surface
                .downcast_ref::<gdk::Popup>()
                .and_then(|popup| popup.parent())
            else {
                break;
            };

            surface = parent;
        }

        popover.popdown();
        glib::Propagation::Stop
    });

    window.add_controller(events);
}

pub(super) fn action_flow() -> gtk::FlowBox {
    gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .min_children_per_line(1)
        .max_children_per_line(4)
        .column_spacing(CONTROL_GAP as u32)
        .row_spacing(CONTROL_GAP as u32)
        .css_classes(["ui-action-flow"])
        .build()
}

pub(super) fn card() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(CONTROL_GAP)
        .css_classes(["ui-card"])
        .build()
}

pub(super) fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .css_classes(["icon-action"])
        .build()
}

pub(super) fn workspace_toggle(text: &str, tooltip: &str) -> gtk::ToggleButton {
    gtk::ToggleButton::builder()
        .label(text)
        .tooltip_text(tooltip)
        .css_classes(["toolbar-toggle", "workspace-pane-toggle"])
        .build()
}

/// An entry for immediate filtering. Callers retain their existing debounce
/// policy. The primary icon is decorative and only the clear icon is active.
pub(super) fn search_entry(placeholder: &str) -> gtk::Entry {
    let entry = gtk::Entry::builder()
        .placeholder_text(placeholder)
        .width_chars(1)
        .primary_icon_name("system-search-symbolic")
        .primary_icon_activatable(false)
        .primary_icon_sensitive(false)
        .secondary_icon_tooltip_text("Clear search")
        .css_classes(["ui-search"])
        .build();

    entry.connect_changed(|entry| {
        entry.set_secondary_icon_name((!entry.text().is_empty()).then_some("edit-clear-symbolic"));
    });

    entry.connect_icon_release(|entry, position| {
        if position == gtk::EntryIconPosition::Secondary {
            entry.set_text("");
        }
    });

    entry
}

/// Preserve GtkSearchEntry's delayed search-changed signal for existing filters.
pub(super) fn delayed_search_entry(placeholder: &str) -> gtk::SearchEntry {
    gtk::SearchEntry::builder()
        .placeholder_text(placeholder)
        .width_chars(1)
        .css_classes(["ui-search"])
        .build()
}

/// Shared menu geometry, with an optional right-aligned shortcut or short hint.
/// Descriptions remain tooltips when a second column would crowd the menu.
pub(super) fn menu_action(text: &str, detail: Option<&str>) -> gtk::Button {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.add_css_class("menu-action-label");

    let child: gtk::Widget = if let Some(detail) = detail {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.append(&label);
        let hint = gtk::Label::new(Some(detail));
        hint.set_xalign(1.0);
        hint.add_css_class("menu-action-detail");
        hint.set_visible(!detail.is_empty());
        row.append(&hint);
        row.upcast()
    } else {
        label.upcast()
    };

    gtk::Button::builder()
        .child(&child)
        .hexpand(true)
        .css_classes(["menu-action"])
        .build()
}

pub(super) fn set_menu_action_detail(button: &gtk::Button, detail: &str) {
    if let Some(hint) = button
        .child()
        .and_then(|child| child.last_child())
        .and_downcast::<gtk::Label>()
        && hint.has_css_class("menu-action-detail")
    {
        hint.set_text(detail);
        hint.set_visible(!detail.is_empty());
    }
}
