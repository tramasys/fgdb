use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct GroupState {
    parents: usize,
    enabled: bool,
    disabled: bool,
}

impl GroupState {
    fn include(&mut self, breakpoint: &Breakpoint) {
        self.parents += usize::from(!breakpoint.is_location());
        self.enabled |= breakpoint.enabled;
        self.disabled |= !breakpoint.enabled;
    }
}

fn parent_groups<'a>(
    breakpoints: &'a [Breakpoint],
    metadata: &'a HashMap<String, StopPointMetadata>,
) -> HashMap<&'a str, &'a str> {
    breakpoints
        .iter()
        .filter(|breakpoint| !breakpoint.is_location())
        .filter_map(|breakpoint| {
            let number = breakpoint.command_number();
            let group = metadata.get(number)?.group.as_deref()?;
            Some((number, group))
        })
        .collect()
}

fn group_states<'a>(
    breakpoints: &'a [Breakpoint],
    metadata: &'a HashMap<String, StopPointMetadata>,
) -> HashMap<&'a str, GroupState> {
    let parents = parent_groups(breakpoints, metadata);
    let mut states = HashMap::<&str, GroupState>::new();

    for breakpoint in breakpoints {
        if let Some(group) = parents.get(breakpoint.command_number()) {
            states.entry(group).or_default().include(breakpoint);
        }
    }

    states
}

fn group_targets(
    breakpoints: &[Breakpoint],
    metadata: &HashMap<String, StopPointMetadata>,
    group: &str,
    action: StopPointBulkAction,
) -> Vec<String> {
    let parents = parent_groups(breakpoints, metadata);

    // GDB preserves individually disabled locations when enabling a parent.
    // Set each differing location explicitly, but delete only parent numbers.
    let mut numbers = breakpoints
        .iter()
        .filter(|breakpoint| parents.get(breakpoint.command_number()).copied() == Some(group))
        .filter(|breakpoint| match action {
            StopPointBulkAction::Enable => !breakpoint.enabled,
            StopPointBulkAction::Disable => breakpoint.enabled,
            StopPointBulkAction::Delete => !breakpoint.is_location(),
        })
        .map(|breakpoint| breakpoint.number.clone())
        .collect::<Vec<_>>();

    numbers.sort_unstable();
    numbers.dedup();
    numbers
}

pub(super) struct GroupControls {
    pub(super) button: gtk::MenuButton,
    enable: gtk::Button,
    disable: gtk::Button,
    delete: gtk::Button,
}

impl GroupControls {
    pub(super) fn new(activate: impl Fn(StopPointBulkAction) + 'static) -> Self {
        let (popover, menu) = build_context_menu();
        let enable = context_menu_action("Enable all");
        let disable = context_menu_action("Disable all");
        let delete = context_menu_action("Delete group and stop points");
        delete.add_css_class("danger-action");
        menu.append(&enable);
        menu.append(&disable);
        menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        menu.append(&delete);
        let activate = Rc::new(activate);

        for (button, action) in [
            (&enable, StopPointBulkAction::Enable),
            (&disable, StopPointBulkAction::Disable),
            (&delete, StopPointBulkAction::Delete),
        ] {
            let activate = Rc::clone(&activate);
            let popup = popover.downgrade();
            button.set_sensitive(false);

            button.connect_clicked(move |button| {
                if !button.is_sensitive() {
                    return;
                }

                if let Some(popup) = popup.upgrade() {
                    popup.popdown();
                }

                activate(action);
            });
        }

        let button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .popover(&popover)
            .valign(gtk::Align::Center)
            .css_classes(["breakpoint-group-menu"])
            .tooltip_text("Group actions apply to all members, including filtered stop points")
            .sensitive(false)
            .build();

        Self {
            button,
            enable,
            disable,
            delete,
        }
    }

    fn update(&self, state: GroupState, available: bool) {
        self.button.set_sensitive(available && state.parents > 0);
        self.enable.set_sensitive(available && state.disabled);
        self.disable.set_sensitive(available && state.enabled);
        self.delete.set_sensitive(available && state.parents > 0);
    }

    pub(super) fn retire(&self) {
        self.button.popdown();
        self.update(GroupState::default(), false);
    }
}

impl Ui {
    pub(in crate::ui) fn update_stop_point_group_controls(&self) {
        let controls = self.stop_point_filter.organization.group_controls.borrow();

        if controls.is_empty() {
            return;
        }

        let available = stop_point_actions_available(&self.model);

        self.with_latest_source_breakpoints(|breakpoints| {
            let metadata = self.stop_point_metadata.borrow();
            let states = group_states(breakpoints, &metadata);

            for (group, controls) in controls.iter() {
                controls.update(
                    states.get(group.as_str()).copied().unwrap_or_default(),
                    available,
                );
            }
        });
    }

    pub(in crate::ui) fn apply_stop_point_group_action(
        &self,
        group: &str,
        action: StopPointBulkAction,
    ) {
        if !stop_point_actions_available(&self.model) {
            return;
        }

        let numbers = self.with_latest_source_breakpoints(|breakpoints| {
            group_targets(
                breakpoints,
                &self.stop_point_metadata.borrow(),
                group,
                action,
            )
        });

        let handler = self.stop_point_bulk_handler.borrow().clone();

        if !numbers.is_empty()
            && let Some(handler) = handler
        {
            handler(action, numbers);
        }
    }
}

#[cfg(test)]
mod tests;
