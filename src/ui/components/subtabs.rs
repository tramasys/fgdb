//! Responsive subtab navigation shared by the Kernel and Misc panels.

use gtk::{glib, prelude::*};
use std::{cell::Cell, rc::Rc};

pub(in crate::ui) struct SubtabNavigation {
    pub root: gtk::Box,
    pub compact_root: gtk::Box,
    pub scroll: gtk::ScrolledWindow,
    pub previous: gtk::Button,
    pub next: gtk::Button,
}

pub(in crate::ui) fn build_subtab_navigation(
    switcher: &gtk::StackSwitcher,
    pages: &gtk::Stack,
    page_specs: &'static [(&'static str, &'static str)],
    previous_tooltip: &str,
    next_tooltip: &str,
) -> SubtabNavigation {
    // The switcher must keep its natural width inside the viewport. Expanding
    // it to the viewport makes GTK clip its buttons without exposing an
    // adjustment, which in turn leaves narrow panes with no way to reach the
    // hidden tabs.
    switcher.set_hexpand(false);
    switcher.set_halign(gtk::Align::Start);
    switcher.set_size_request(0, -1);
    let scroll = gtk::ScrolledWindow::new();
    scroll.add_css_class("kernel-tabs-scroll");
    scroll.set_policy(gtk::PolicyType::External, gtk::PolicyType::Never);
    scroll.set_overlay_scrolling(true);
    scroll.set_propagate_natural_width(false);
    scroll.set_propagate_natural_height(true);
    scroll.set_min_content_width(0);
    scroll.set_size_request(0, -1);
    scroll.set_child(Some(switcher));
    scroll.set_hexpand(true);
    scroll.set_halign(gtk::Align::Fill);
    let previous = gtk::Button::with_label("‹");
    previous.add_css_class("kernel-tab-nav-button");
    previous.set_tooltip_text(Some(previous_tooltip));
    let next = gtk::Button::with_label("›");
    next.add_css_class("kernel-tab-nav-button");
    next.set_tooltip_text(Some(next_tooltip));
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.add_css_class("kernel-tab-navigation");
    root.set_size_request(0, -1);
    root.set_hexpand(true);
    root.set_halign(gtk::Align::Fill);
    root.append(&previous);
    root.append(&scroll);
    root.append(&next);
    let scroll_for_previous = scroll.clone();
    previous.connect_clicked(move |_| scroll_subtabs(&scroll_for_previous, -1.0));
    let scroll_for_next = scroll.clone();
    next.connect_clicked(move |_| scroll_subtabs(&scroll_for_next, 1.0));
    let adjustment = scroll.hadjustment();
    let pending = Rc::new(Cell::new(false));
    let previous_for_adjustment = previous.downgrade();
    let next_for_adjustment = next.downgrade();

    // Adjustment changes can originate inside allocation. Revealing arrows
    // there gives GTK newly visible children with no allocated rectangle.
    let schedule = move |adjustment: &gtk::Adjustment| {
        if pending.replace(true) {
            return;
        }

        let pending = Rc::clone(&pending);
        let adjustment = adjustment.downgrade();
        let previous = previous_for_adjustment.clone();
        let next = next_for_adjustment.clone();

        glib::idle_add_local_once(move || {
            pending.set(false);

            if let (Some(adjustment), Some(previous), Some(next)) =
                (adjustment.upgrade(), previous.upgrade(), next.upgrade())
            {
                update_subtab_arrows(&adjustment, &previous, &next);
            }
        });
    };

    adjustment.connect_value_changed(schedule.clone());
    adjustment.connect_changed(schedule);

    update_subtab_arrows(&adjustment, &previous, &next);

    let compact_root =
        build_compact_subtab_navigation(pages, page_specs, previous_tooltip, next_tooltip);

    SubtabNavigation {
        root,
        compact_root,
        scroll,
        previous,
        next,
    }
}

fn build_compact_subtab_navigation(
    pages: &gtk::Stack,
    page_specs: &'static [(&'static str, &'static str)],
    previous_tooltip: &str,
    next_tooltip: &str,
) -> gtk::Box {
    let labels = page_specs
        .iter()
        .map(|(_, title)| *title)
        .collect::<Vec<_>>();

    let selector = gtk::DropDown::from_strings(&labels);
    selector.add_css_class("kernel-compact-tab-selector");
    selector.set_hexpand(true);
    selector.set_tooltip_text(Some("Select a view"));
    let previous = gtk::Button::with_label("‹");
    previous.add_css_class("kernel-tab-nav-button");
    previous.set_tooltip_text(Some(previous_tooltip));
    let next = gtk::Button::with_label("›");
    next.add_css_class("kernel-tab-nav-button");
    next.set_tooltip_text(Some(next_tooltip));
    let has_multiple_pages = page_specs.len() > 1;
    previous.set_visible(has_multiple_pages);
    next.set_visible(has_multiple_pages);
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.add_css_class("kernel-tab-navigation");
    root.add_css_class("kernel-compact-tab-navigation");
    root.set_hexpand(true);
    root.append(&previous);
    root.append(&selector);
    root.append(&next);
    root.set_visible(false);
    let pages_for_selector = pages.downgrade();

    selector.connect_selected_notify(move |selector| {
        let index = selector.selected() as usize;

        if let Some((name, _)) = page_specs.get(index)
            && let Some(pages) = pages_for_selector.upgrade()
        {
            pages.set_visible_child_name(name);
        }
    });

    let pages_for_previous = pages.downgrade();

    previous.connect_clicked(move |_| {
        if let Some(pages) = pages_for_previous.upgrade() {
            select_relative_subtab(&pages, page_specs, -1);
        }
    });

    let pages_for_next = pages.downgrade();

    next.connect_clicked(move |_| {
        if let Some(pages) = pages_for_next.upgrade() {
            select_relative_subtab(&pages, page_specs, 1);
        }
    });

    let selector_for_page = selector.downgrade();
    let previous_for_page = previous.downgrade();
    let next_for_page = next.downgrade();

    let update = move |pages: &gtk::Stack| {
        let (Some(selector), Some(previous), Some(next)) = (
            selector_for_page.upgrade(),
            previous_for_page.upgrade(),
            next_for_page.upgrade(),
        ) else {
            return;
        };

        let index = selected_subtab_index(pages, page_specs);
        selector.set_selected(index as u32);
        previous.set_sensitive(index > 0);
        next.set_sensitive(index + 1 < page_specs.len());
    };

    update(pages);
    pages.connect_visible_child_notify(update);

    root
}

fn selected_subtab_index(pages: &gtk::Stack, page_specs: &[(&str, &str)]) -> usize {
    pages
        .visible_child_name()
        .as_deref()
        .and_then(|name| {
            page_specs
                .iter()
                .position(|(candidate, _)| *candidate == name)
        })
        .unwrap_or(0)
}

fn select_relative_subtab(pages: &gtk::Stack, page_specs: &[(&str, &str)], direction: isize) {
    let current = selected_subtab_index(pages, page_specs);
    let last = page_specs.len().saturating_sub(1);
    let target = current.saturating_add_signed(direction).min(last);

    if let Some((name, _)) = page_specs.get(target) {
        pages.set_visible_child_name(name);
    }
}

fn scroll_subtabs(scroll: &gtk::ScrolledWindow, direction: f64) {
    let adjustment = scroll.hadjustment();
    let lower = adjustment.lower();
    let upper = (adjustment.upper() - adjustment.page_size()).max(lower);
    let step = (adjustment.page_size() * 0.7).max(80.0);
    adjustment.set_value((adjustment.value() + direction * step).clamp(lower, upper));
}

pub(in crate::ui) fn update_subtab_arrows(
    adjustment: &gtk::Adjustment,
    previous: &gtk::Button,
    next: &gtk::Button,
) {
    let overflow = adjustment.upper() - adjustment.lower() > adjustment.page_size() + 1.0;
    previous.set_visible(overflow);
    next.set_visible(overflow);
    previous.set_sensitive(overflow && adjustment.value() > adjustment.lower() + 1.0);

    next.set_sensitive(
        overflow && adjustment.value() + adjustment.page_size() < adjustment.upper() - 1.0,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display, run separately from other GTK tests"]
    fn navigation_tracks_selection_without_retaining_its_stack() {
        gtk::init().unwrap();
        let pages = gtk::Stack::new();
        pages.add_named(&gtk::Box::new(gtk::Orientation::Vertical, 0), Some("first"));
        pages.add_named(
            &gtk::Box::new(gtk::Orientation::Vertical, 0),
            Some("second"),
        );

        let switcher = gtk::StackSwitcher::builder().stack(&pages).build();

        let navigation = build_subtab_navigation(
            &switcher,
            &pages,
            &[("first", "First"), ("second", "Second")],
            "Previous",
            "Next",
        );

        let previous = navigation
            .compact_root
            .first_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();

        let selector = previous
            .next_sibling()
            .unwrap()
            .downcast::<gtk::DropDown>()
            .unwrap();

        let next = selector
            .next_sibling()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();

        selector.set_selected(1);
        assert_eq!(pages.visible_child_name().as_deref(), Some("second"));
        assert!(!next.is_sensitive());
        previous.emit_clicked();
        assert_eq!(pages.visible_child_name().as_deref(), Some("first"));
        assert_eq!(selector.selected(), 0);
        pages.set_visible_child_name("second");
        assert_eq!(selector.selected(), 1);
        let weak_pages = pages.downgrade();
        let weak_selector = selector.downgrade();
        drop((previous, selector, next, navigation, switcher, pages));
        assert!(weak_selector.upgrade().is_none());
        assert!(weak_pages.upgrade().is_none());
    }
}
