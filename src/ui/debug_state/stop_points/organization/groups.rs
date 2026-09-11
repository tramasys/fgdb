use super::*;
use std::rc::Weak;

struct Section {
    root: gtk::Widget,
    body: gtk::Box,
    count: gtk::Label,
    parents: usize,
}

pub(in super::super) struct GroupRows {
    organization: Rc<StopPointOrganization>,
    ui: Weak<Ui>,
    sections: BTreeMap<Option<String>, Section>,
}

impl GroupRows {
    pub(in super::super) fn new(organization: &Rc<StopPointOrganization>, ui: Weak<Ui>) -> Self {
        for (_, controls) in organization.group_controls.borrow_mut().drain(..) {
            controls.retire();
        }

        Self {
            organization: Rc::clone(organization),
            ui,
            sections: BTreeMap::new(),
        }
    }

    pub(in super::super) fn parent_container(&mut self, group: Option<&str>) -> gtk::Box {
        let key = group.map(str::to_owned);

        let section = self.sections.entry(key.clone()).or_insert_with(|| {
            let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let count = gtk::Label::new(None);
            count.add_css_class("muted");
            count.add_css_class("breakpoint-group-count");
            count.set_tooltip_text(Some("Displayed stop points in this group"));

            let root = if self.organization.groups.values.borrow().is_empty() {
                body.clone().upcast()
            } else {
                let expanded = !self.organization.collapsed.borrow().contains(&key);

                let indicator = gtk::Label::new(Some(if expanded {
                    DISCLOSURE_EXPANDED_ICON
                } else {
                    DISCLOSURE_COLLAPSED_ICON
                }));

                indicator.add_css_class("disclosure-arrow");
                indicator.set_width_chars(1);
                indicator.set_xalign(0.5);
                let title = gtk::Label::new(Some(group.unwrap_or("Ungrouped")));
                title.set_ellipsize(pango::EllipsizeMode::End);
                title.set_xalign(0.0);
                title.set_hexpand(true);
                title.set_tooltip_text(group);
                let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                heading.set_hexpand(true);
                heading.append(&indicator);
                heading.append(&title);
                heading.append(&count);

                if let Some(group) = group {
                    let weak = self.ui.clone();
                    let name = group.to_owned();

                    let controls = actions::GroupControls::new(move |action| {
                        if let Some(ui) = weak.upgrade() {
                            ui.apply_stop_point_group_action(&name, action);
                        }
                    });

                    heading.append(&controls.button);

                    self.organization
                        .group_controls
                        .borrow_mut()
                        .push((group.to_owned(), controls));
                }

                let expander = gtk::Expander::builder()
                    .label_widget(&heading)
                    .child(&body)
                    .expanded(expanded)
                    .hexpand(true)
                    .css_classes(["breakpoint-group"])
                    .build();

                let weak = Rc::downgrade(&self.organization);

                expander.connect_expanded_notify(move |expander| {
                    indicator.set_text(if expander.is_expanded() {
                        DISCLOSURE_EXPANDED_ICON
                    } else {
                        DISCLOSURE_COLLAPSED_ICON
                    });

                    let Some(organization) = weak.upgrade() else {
                        return;
                    };

                    if expander.is_expanded() {
                        organization.collapsed.borrow_mut().remove(&key);
                    } else {
                        organization.collapsed.borrow_mut().insert(key.clone());
                    }
                });

                expander.upcast()
            };

            Section {
                root,
                body,
                count,
                parents: 0,
            }
        });

        section.parents += 1;
        section.count.set_text(&section.parents.to_string());
        section.body.clone()
    }

    pub(in super::super) fn append_to(mut self, list: &gtk::Box) {
        let ungrouped = self.sections.remove(&None);

        for section in self.sections.into_values().chain(ungrouped) {
            list.append(&section.root);
        }
    }
}
