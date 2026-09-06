//! Shared presentation controls. Keep debugger policy and request lifetimes in
//! their existing owners, so a visual change cannot change command semantics.

use super::*;

pub(super) const CONTROL_GAP: i32 = 6;
pub(super) const CONTENT_INSET: i32 = 8;
pub(super) const DIALOG_INSET: i32 = 12;

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

    let child: gtk::Widget = if let Some(detail) = detail.filter(|detail| !detail.is_empty()) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.append(&label);
        let hint = gtk::Label::new(Some(detail));
        hint.set_xalign(1.0);
        hint.add_css_class("menu-action-detail");
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
