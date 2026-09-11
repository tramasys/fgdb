use super::*;
use std::collections::{BTreeMap, BTreeSet};

mod actions;
mod groups;

pub(super) use groups::GroupRows;

#[derive(Debug, Default, PartialEq, Eq)]
struct Catalog {
    groups: Vec<String>,
    tags: Vec<String>,
    ungrouped: bool,
}

impl Catalog {
    fn build(breakpoints: &[Breakpoint], metadata: &HashMap<String, StopPointMetadata>) -> Self {
        let mut groups = BTreeSet::new();
        let mut tags = BTreeMap::<String, String>::new();
        let mut ungrouped = false;

        for breakpoint in breakpoints
            .iter()
            .filter(|breakpoint| !breakpoint.is_location())
        {
            let metadata = metadata.get(breakpoint.command_number());

            if let Some(group) = metadata.and_then(|metadata| metadata.group.as_ref()) {
                groups.insert(group.clone());
            } else {
                ungrouped = true;
            }

            for tag in metadata.into_iter().flat_map(|metadata| &metadata.tags) {
                tags.entry(tag.to_ascii_lowercase())
                    .and_modify(|display| {
                        if tag < display {
                            display.clone_from(tag);
                        }
                    })
                    .or_insert_with(|| tag.clone());
            }
        }

        Self {
            groups: groups.into_iter().collect(),
            tags: tags.into_values().collect(),
            ungrouped,
        }
    }
}

pub(super) struct Selection {
    group: Option<String>,
    tag: Option<String>,
}

impl Selection {
    pub(super) fn matches(&self, metadata: Option<&StopPointMetadata>) -> bool {
        self.group.as_ref().is_none_or(|group| {
            metadata.and_then(|metadata| metadata.group.as_ref()) == Some(group)
        }) && self.tag.as_ref().is_none_or(|tag| {
            metadata.is_some_and(|metadata| {
                metadata
                    .tags
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(tag))
            })
        })
    }
}

struct Facet {
    widget: gtk::DropDown,
    values: RefCell<Vec<String>>,
    all: &'static str,
    ignore_case: bool,
}

impl Facet {
    fn new(all: &'static str, ignore_case: bool) -> Self {
        let widget = gtk::DropDown::from_strings(&[all]);
        widget.set_enable_search(true);
        widget.set_tooltip_text(Some(all));
        let factory = gtk::SignalListItemFactory::new();

        factory.connect_setup(|_, object| {
            let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
                return;
            };

            let label = gtk::Label::builder()
                .xalign(0.0)
                .max_width_chars(22)
                .ellipsize(pango::EllipsizeMode::End)
                .build();

            item.set_child(Some(&label));
        });

        factory.connect_bind(|_, object| {
            let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
                return;
            };

            if let (Some(label), Some(value)) = (
                item.child().and_downcast::<gtk::Label>(),
                item.item().and_downcast::<gtk::StringObject>(),
            ) {
                label.set_text(&value.string());
                label.set_tooltip_text(Some(&value.string()));
            }
        });

        widget.set_factory(Some(&factory));

        Self {
            widget,
            values: RefCell::new(Vec::new()),
            all,
            ignore_case,
        }
    }

    fn selected(&self) -> Option<String> {
        let index = self.widget.selected().checked_sub(1)?;
        self.values.borrow().get(index as usize).cloned()
    }

    fn sync(&self, values: Vec<String>) {
        self.widget.set_visible(!values.is_empty());

        if *self.values.borrow() == values {
            return;
        }

        let selected = self.selected();

        let position = selected
            .as_ref()
            .and_then(|selected| {
                values.iter().position(|value| {
                    value == selected || (self.ignore_case && value.eq_ignore_ascii_case(selected))
                })
            })
            .map_or(0, |index| index as u32 + 1);

        let labels = std::iter::once(self.all)
            .chain(values.iter().map(String::as_str))
            .collect::<Vec<_>>();

        let model = gtk::StringList::new(&labels);
        self.values.replace(values);
        self.widget.set_model(Some(&model));
        self.widget.set_selected(position);
    }
}

pub(in crate::ui) struct StopPointOrganization {
    pub(in crate::ui) root: gtk::Box,
    groups: Facet,
    tags: Facet,
    updating: Cell<bool>,
    collapsed: RefCell<HashSet<Option<String>>>,
    group_controls: RefCell<Vec<(String, actions::GroupControls)>>,
}

impl StopPointOrganization {
    pub(in crate::ui) fn new() -> Rc<Self> {
        let root = components::control_row();
        root.add_css_class("breakpoint-quick-filters");
        root.set_visible(false);
        let groups = Facet::new("All groups", false);
        let tags = Facet::new("All tags", true);

        groups
            .widget
            .set_tooltip_text(Some("Filter by existing group"));

        tags.widget.set_tooltip_text(Some("Filter by existing tag"));
        root.append(&groups.widget);
        root.append(&tags.widget);

        Rc::new(Self {
            root,
            groups,
            tags,
            updating: Cell::new(false),
            collapsed: RefCell::new(HashSet::new()),
            group_controls: RefCell::new(Vec::new()),
        })
    }

    pub(super) fn connect_changed(self: &Rc<Self>, changed: impl Fn() + 'static) {
        let changed: Rc<dyn Fn()> = Rc::new(changed);

        for widget in [&self.groups.widget, &self.tags.widget] {
            let weak = Rc::downgrade(self);
            let changed = Rc::clone(&changed);

            widget.connect_selected_notify(move |_| {
                if let Some(organization) = weak.upgrade()
                    && !organization.updating.get()
                {
                    changed();
                }
            });
        }
    }

    pub(super) fn sync(
        &self,
        breakpoints: &[Breakpoint],
        metadata: &HashMap<String, StopPointMetadata>,
    ) {
        let catalog = Catalog::build(breakpoints, metadata);

        self.collapsed.borrow_mut().retain(|group| match group {
            Some(group) => catalog.groups.binary_search(group).is_ok(),
            None => catalog.ungrouped,
        });

        self.updating.set(true);

        self.root
            .set_visible(!catalog.groups.is_empty() || !catalog.tags.is_empty());

        self.groups.sync(catalog.groups);
        self.tags.sync(catalog.tags);
        self.updating.set(false);
    }

    pub(super) fn selection(&self) -> Selection {
        Selection {
            group: self.groups.selected(),
            tag: self.tags.selected(),
        }
    }

    pub(super) fn suggestions(&self) -> (Vec<String>, Vec<String>) {
        (
            self.groups.values.borrow().clone(),
            self.tags.values.borrow().clone(),
        )
    }
}

pub(super) fn tag_chips(tags: &[String]) -> gtk::FlowBox {
    const MAX_CHIPS: usize = 8;

    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .halign(gtk::Align::Start)
        .column_spacing(4)
        .row_spacing(2)
        .min_children_per_line(1)
        .max_children_per_line(MAX_CHIPS as u32)
        .build();

    flow.add_css_class("breakpoint-tags");

    let visible = if tags.len() > MAX_CHIPS {
        MAX_CHIPS - 1
    } else {
        tags.len()
    };

    for tag in &tags[..visible] {
        let label = gtk::Label::builder()
            .label(tag)
            .max_width_chars(22)
            .ellipsize(pango::EllipsizeMode::End)
            .css_classes(["breakpoint-tag"])
            .build();

        label.set_tooltip_text(Some(tag));
        flow.insert(&label, -1);
    }

    if visible < tags.len() {
        let more = gtk::Label::new(Some(&format!("+{}", tags.len() - visible)));
        more.add_css_class("breakpoint-tag");
        more.set_tooltip_text(Some(&tags[visible..].join(", ")));
        flow.insert(&more, -1);
    }

    flow
}

#[cfg(test)]
mod tests;
