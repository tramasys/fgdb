//! A distinct result table reusing the shared variable tree and inspection path.

use super::*;
use crate::model::return_value::CapturedReturnValue;

type RefreshHandler = Rc<dyn Fn(Variable)>;
type CaptureHandler = Rc<dyn Fn()>;

#[derive(Clone)]
pub(super) struct ReturnValueView {
    pub(super) root: gtk::Box,
    pub(super) store: gio::ListStore,
    view: gtk::ColumnView,
    selection: gtk::SingleSelection,
    summary: gtk::Label,
    clear: gtk::Button,
    enabled: Rc<Cell<bool>>,
    state: Rc<RefCell<ReturnViewState>>,
    refresh: Rc<RefCell<Option<RefreshHandler>>>,
    capture: Rc<RefCell<Option<CaptureHandler>>>,
    capture_setting_dirty: Rc<Cell<bool>>,
}

#[derive(Default)]
struct ReturnViewState {
    generation: u64,
    inspectable: bool,
    entries: Vec<ReturnEntry>,
    pending: HashSet<(u64, u64)>,
}

struct ReturnEntry {
    captured: Rc<CapturedReturnValue>,
    variable: Variable,
    attempted: bool,
}

impl ReturnValueView {
    pub(super) fn new(bindings: &InspectorBindings<'_>) -> Self {
        let filter = source_search_entry("Filter name, type, or value");
        filter.add_css_class("locals-filter-entry");
        filter.set_hexpand(true);

        let (view, store, selection) = build_locals_view(
            &bindings.columns.table(TableId::ReturnValues),
            bindings.variable_children_handler,
            bindings.variable_viewer_handler,
            bindings.variable_viewers,
            bindings.variable_presentation,
            bindings.variable_locations,
            Some((&filter, None)),
        );

        let summary = gtk::Label::new(None);
        summary.add_css_class("locals-summary");
        let clear = gtk::Button::with_label("Clear");
        clear.add_css_class("inline-action");
        clear.set_tooltip_text(Some("Clear return history for all threads"));
        let tools = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        tools.add_css_class("locals-toolbar");
        tools.append(&filter);
        tools.append(&summary);
        tools.append(&clear);

        let scrolled = gtk::ScrolledWindow::builder()
            .child(&view)
            .min_content_height(64)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .build();

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&tools);
        body.append(&scrolled);
        let root = build_disclosure("RETURN VALUES", &body, true, "return-values-panel");
        root.set_visible(false);

        Self {
            root,
            store,
            view,
            selection,
            summary,
            clear,
            enabled: Rc::new(Cell::new(true)),
            state: Rc::default(),
            refresh: Rc::default(),
            capture: Rc::default(),
            capture_setting_dirty: Rc::default(),
        }
    }

    fn render(&self) {
        let variables = self
            .state
            .borrow()
            .entries
            .iter()
            .map(|entry| entry.variable.clone())
            .collect::<Vec<_>>();

        replace_variable_roots(&self.store, &variables, false);

        self.summary.set_text(&format!(
            "{} result{}",
            variables.len(),
            if variables.len() == 1 { "" } else { "s" }
        ));

        self.root
            .set_visible(self.enabled.get() && !variables.is_empty());
    }

    fn sync(
        &self,
        values: Vec<Rc<CapturedReturnValue>>,
        generation: u64,
        inspectable: bool,
    ) -> Option<Vec<String>> {
        let mut state = self.state.borrow_mut();

        if state.generation == generation
            && state.inspectable == inspectable
            && state.entries.len() == values.len()
            && state
                .entries
                .iter()
                .zip(&values)
                .all(|(entry, value)| entry.captured.id == value.id)
        {
            return None;
        }

        let context_changed = state.generation != generation;
        state.generation = generation;
        state.inspectable = inspectable;
        state.pending.retain(|(pending, _)| *pending == generation);
        let mut previous = std::mem::take(&mut state.entries);
        let mut retired = Vec::new();

        state.entries = values
            .into_iter()
            .map(|captured| {
                if let Some(index) = previous
                    .iter()
                    .position(|entry| entry.captured.id == captured.id)
                {
                    let mut entry = previous.remove(index);

                    if context_changed {
                        retired.extend(entry.variable.varobj.take());
                        entry.attempted = entry.variable.type_name.is_some();
                    }

                    entry
                } else {
                    let variable = captured.variable();

                    ReturnEntry {
                        captured,
                        variable,
                        attempted: false,
                    }
                }
            })
            .collect();

        retired.extend(
            previous
                .into_iter()
                .filter_map(|entry| entry.variable.varobj),
        );

        let requests = if inspectable {
            state
                .entries
                .iter_mut()
                .filter_map(|entry| {
                    if entry.attempted
                        || !valid_history_reference(
                            entry.captured.value.history_variable.as_deref(),
                        )
                    {
                        return None;
                    }

                    entry.attempted = true;
                    Some(entry.variable.clone())
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        drop(state);

        self.render();

        if let Some(handler) = self.refresh.borrow().clone() {
            for variable in requests {
                let handler = Rc::clone(&handler);
                glib::idle_add_local_once(move || handler(variable));
            }
        }

        Some(retired)
    }
}

fn valid_history_reference(reference: Option<&str>) -> bool {
    reference
        .and_then(|value| value.strip_prefix('$'))
        .is_some_and(|digits| {
            !digits.is_empty()
                && !digits.starts_with('0')
                && digits.bytes().all(|byte| byte.is_ascii_digit())
        })
}

impl Ui {
    pub(crate) fn connect_return_values(
        self: &Rc<Self>,
        refresh: impl Fn(Variable) + 'static,
        capture: impl Fn() + 'static,
    ) {
        self.return_value.refresh.replace(Some(Rc::new(refresh)));
        self.return_value.capture.replace(Some(Rc::new(capture)));
        let weak = Rc::downgrade(self);

        self.return_value.clear.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.model.clear_return_value();
                ui.return_value.capture_setting_dirty.set(true);
                ui.update_control_sensitivity();

                if let Some(capture) = ui.return_value.capture.borrow().clone() {
                    glib::idle_add_local_once(move || capture());
                }
            }
        });

        let weak = Rc::downgrade(self);

        self.return_value.view.connect_activate(move |_, position| {
            let Some(ui) = weak.upgrade() else {
                return;
            };

            let Some((row, node)) = variable_node_at(&ui.return_value.selection, position) else {
                return;
            };

            if !ui.variable_action_is_current(&node.variable) && node.load_more.is_none() {
                return;
            }

            if node.load_more.is_some() {
                defer_next_variable_page(
                    &row,
                    &ui.return_value.selection,
                    &ui.variable_children_handler,
                );
            } else if row.is_expandable() {
                defer_variable_toggle(
                    &row,
                    &ui.return_value.selection,
                    &ui.variable_children_handler,
                );
            }
        });
    }

    pub(super) fn sync_return_values(&self) {
        let values = if self.return_values_enabled() {
            self.model.return_values()
        } else {
            Vec::new()
        };

        let generation = self.model.current_stop_refresh_generation();

        if let Some(retired) =
            self.return_value
                .sync(values, generation, self.model.can_edit_variable(generation))
        {
            self.defer_variable_object_deletions(retired);
            self.rebuild_variable_node_index();
        }
    }

    pub(crate) fn refresh_return_values(&self) {
        self.sync_return_values();
    }

    pub(super) fn set_return_values_enabled(&self, enabled: bool) {
        let changed = self.return_value.enabled.replace(enabled) != enabled;

        if changed {
            self.return_value.capture_setting_dirty.set(true);
        }

        self.sync_return_values();

        if changed && let Some(capture) = self.return_value.capture.borrow().clone() {
            glib::idle_add_local_once(move || capture());
        }
    }

    pub(crate) fn return_values_enabled(&self) -> bool {
        self.return_value.enabled.get()
    }

    pub(crate) fn return_value_capture_setting_dirty(&self) -> bool {
        self.return_value.capture_setting_dirty.get()
    }

    pub(crate) fn acknowledge_return_value_capture_setting(&self, enabled: bool) {
        if self.return_values_enabled() == enabled {
            self.return_value.capture_setting_dirty.set(false);
        }
    }

    pub(crate) fn return_value_action_is_current(&self, variable: &Variable) -> bool {
        let state = self.return_value.state.borrow();

        state.generation == self.model.current_stop_refresh_generation()
            && self.return_value.enabled.get()
            && self
                .model
                .return_values()
                .iter()
                .any(|value| Some(value.id) == variable.return_value)
            && state
                .entries
                .iter()
                .any(|entry| entry.variable.return_value == variable.return_value)
            && if let Some(varobj) = variable.varobj.as_deref() {
                self.find_variable_node(varobj)
                    .is_some_and(|node| node.variable.has_same_children(variable))
            } else {
                state
                    .entries
                    .iter()
                    .any(|entry| entry.variable == *variable)
            }
    }

    pub(crate) fn claim_return_value_object(&self, variable: &Variable) -> bool {
        let Some(id) = variable.return_value else {
            return false;
        };

        if !self.variable_action_is_current(variable)
            || !valid_history_reference(Some(&variable.name))
            || variable.varobj.is_some()
        {
            return false;
        }

        let generation = self.model.current_stop_refresh_generation();

        self.return_value
            .state
            .borrow_mut()
            .pending
            .insert((generation, id))
    }

    pub(super) fn return_variable_node(&self, variable: &Variable) -> Option<VariableNode> {
        (0..self.return_value.store.n_items()).find_map(|position| {
            let node = variable_root_node(&self.return_value.store, position as usize)?;

            (variable.return_value.is_some() && node.variable.has_same_children(variable))
                .then_some(node)
        })
    }

    pub(crate) fn attach_return_value_object(
        &self,
        generation: u64,
        original: &Variable,
        created: Option<Variable>,
    ) -> bool {
        let Some(id) = original.return_value else {
            return false;
        };

        self.return_value
            .state
            .borrow_mut()
            .pending
            .remove(&(generation, id));

        if !self.model.is_stop_refresh_current(generation)
            || !self.variable_action_is_current(original)
        {
            return false;
        }

        let Some(mut created) = created else {
            return false;
        };

        created.return_value = Some(id);
        let mut state = self.return_value.state.borrow_mut();

        let Some(entry) = state
            .entries
            .iter_mut()
            .find(|entry| entry.captured.id == id)
        else {
            return false;
        };

        entry.variable = created;
        drop(state);
        self.return_value.render();
        self.rebuild_variable_node_index();
        true
    }
}

#[cfg(test)]
mod tests;
