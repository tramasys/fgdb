//! Bounded child expansion, tied to the original stop and parent snapshot.

use super::*;

pub(in crate::app) fn request_variable_children(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    parent: Variable,
    from: usize,
) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    if !current_ui.variable_action_is_current(&parent) || !parent.can_expand() {
        current_ui.cancel_variable_children_request(&parent);
        return;
    }

    if parent.varobj.is_none() {
        drop(current_ui);
        request_lazy_local_variable_children(ui, client, parent, from);
        return;
    }

    let generation = current_ui.model.current_stop_refresh_generation();

    if !current_ui.begin_variable_children_loading(&parent, from) {
        return;
    }

    let Some(requests) = stop_requests(&ui, &client, generation) else {
        current_ui.cancel_variable_children_request(&parent);
        return;
    };

    drop(current_ui);

    let request = Rc::new(ChildrenRequest {
        ui,
        requests,
        parent,
        from,
    });

    if request.parent.num_children > 0 || request.parent.has_more || request.parent.dynamic {
        request.list();
    } else if request.parent.is_pointer() && from == 0 {
        request.resolve_pointer();
    } else {
        request.fail("No children are available at this offset");
    }
}

struct ChildrenRequest {
    ui: Weak<Ui>,
    requests: StopRequests,
    parent: Variable,
    from: usize,
}

impl ChildrenRequest {
    fn is_current(&self) -> bool {
        self.requests.is_current()
            && self
                .ui
                .upgrade()
                .is_some_and(|ui| ui.variable_children_target_is_current(&self.parent, self.from))
    }

    fn cancel(&self) {
        if let Some(ui) = self.ui.upgrade() {
            ui.cancel_variable_children_request(&self.parent);
        }
    }

    fn fail(&self, message: &str) {
        if !self.is_current() {
            self.cancel();
            return;
        }

        if let Some(ui) = self.ui.upgrade() {
            ui.show_variable_children_page_error(&self.parent, self.from, message);
        }
    }

    fn list(self: Rc<Self>) {
        let Some(to) = variable_child_page_end(self.from) else {
            self.fail("The variable child inspection limit has been reached");
            return;
        };

        let varobj = self.parent.varobj.as_deref().expect("expanded object");

        let command = format!(
            "-var-list-children --all-values {} {} {to}",
            crate::debugger::quote(varobj),
            self.from,
        );

        let guard = Rc::clone(&self);
        let response = Rc::clone(&self);

        if let Err(error) = self
            .requests
            .unscoped(&command)
            .when(move || guard.is_current())
            .with_print_limit(to, move |client, record| {
                if record.class == "superseded" || !response.is_current() {
                    response.cancel();
                    return;
                }

                if !record.is_done() {
                    response.fail(
                        record
                            .error_message()
                            .unwrap_or("GDB could not expand this value"),
                    );

                    return;
                }

                let Ok((children, more)) = parse_page(&record, to - response.from) else {
                    response
                        .fail("GDB returned an incomplete or invalid child page. Retry expansion");

                    return;
                };

                let next = response.from + children.len();

                let has_more = more.unwrap_or(next < response.parent.num_children);

                if has_more && children.is_empty() {
                    response.fail("GDB reported more children but returned none. Retry expansion");
                    return;
                }

                if let Some(ui) = response.ui.upgrade()
                    && ui.show_variable_children_page(
                        &response.parent,
                        response.from,
                        &children,
                        has_more && next < MAX_VARIABLE_CHILDREN,
                    )
                {
                    set_variable_update_range(
                        client,
                        &response.ui,
                        response.requests.generation(),
                        response.parent.varobj.as_deref().expect("expanded object"),
                        next,
                    );
                }
            })
        {
            self.fail(&error.to_string());
        }
    }

    fn resolve_pointer(self: Rc<Self>) {
        let command = format!(
            "-var-info-path-expression {}",
            crate::debugger::quote(self.parent.varobj.as_deref().expect("expanded object")),
        );

        let guard = Rc::clone(&self);
        let response = Rc::clone(&self);

        if let Err(error) = self
            .requests
            .unscoped(&command)
            .when(move || guard.is_current())
            .request(move |_, record| {
                if record.class == "superseded" || !response.is_current() {
                    response.cancel();
                    return;
                }

                if let Some(path) = crate::debugger::variable_path_expression(&record) {
                    response.dereference(path);
                } else {
                    response.fail(
                        record
                            .error_message()
                            .unwrap_or("GDB cannot dereference this pointer type"),
                    );
                }
            })
        {
            self.fail(&error.to_string());
        }
    }

    fn dereference(self: Rc<Self>, path: String) {
        let varobj = next_variable_object_name();

        let command = format!(
            "-var-create {varobj} * {}",
            crate::debugger::quote(&format!("*({path})")),
        );

        let guard = Rc::clone(&self);
        let response = Rc::clone(&self);

        if let Err(error) = self
            .requests
            .frame(&command)
            .when(move || guard.is_current())
            .with_print_limit(AUTOMATIC_PRINT_ELEMENTS, move |client, record| {
                if record.class == "superseded" || !response.is_current() {
                    delete_variable_object(client, &varobj);
                    response.cancel();
                    return;
                }

                let child = record
                    .is_done()
                    .then(|| {
                        crate::debugger::variable_object(
                            &record,
                            &format!("*{}", response.parent.name),
                        )
                    })
                    .flatten();

                let Some(child) = child else {
                    delete_variable_object(client, &varobj);
                    response.fail(
                        record
                            .error_message()
                            .unwrap_or("GDB cannot dereference this pointer"),
                    );

                    return;
                };

                let attached = response.ui.upgrade().is_some_and(|ui| {
                    ui.show_variable_children_page(
                        &response.parent,
                        0,
                        std::slice::from_ref(&child),
                        false,
                    )
                });

                if attached {
                    register_owned_variable_object(
                        client,
                        response.parent.varobj.as_deref().expect("expanded object"),
                        &varobj,
                    );
                } else {
                    delete_variable_object(client, &varobj);
                }
            })
        {
            self.fail(&error.to_string());
        }
    }
}

fn parse_page(record: &MiRecord, limit: usize) -> Result<(Vec<Variable>, Option<bool>), ()> {
    let count: usize = record
        .field("numchild")
        .and_then(|value| value.as_const())
        .and_then(|count| count.parse().ok())
        .ok_or(())?;

    let actual = record
        .field("children")
        .map(|value| value.as_list().map(<[_]>::len).ok_or(()))
        .transpose()?
        .unwrap_or(0);

    if count != actual || count > limit {
        return Err(());
    }

    let children = crate::debugger::variable_children(record);

    let mut identities = HashSet::with_capacity(children.len());

    if children.len() != count
        || children.iter().any(|child| {
            child
                .varobj
                .as_deref()
                .is_none_or(|name| name.is_empty() || !identities.insert(name))
        })
    {
        return Err(());
    }

    let more = match record.field("has_more").and_then(|value| value.as_const()) {
        Some("1") => Some(true),
        Some("0") => Some(false),
        None if record.field("has_more").is_none() => None,
        _ => return Err(()),
    };

    Ok((children, more))
}

fn set_variable_update_range(
    client: &MiClient,
    ui: &Weak<Ui>,
    generation: u64,
    varobj: &str,
    loaded_children: usize,
) {
    if loaded_children == 0 {
        return;
    }

    let command = format!(
        "-var-set-update-range {} 0 {loaded_children}",
        crate::debugger::quote(varobj)
    );

    let Some(requests) = stop_requests(ui, client, generation) else {
        return;
    };

    let ui_for_guard = ui.clone();
    let varobj_for_guard = varobj.to_owned();

    let _ = requests
        .unscoped(&command)
        .when(move || {
            ui_for_guard
                .upgrade()
                .is_some_and(|ui| ui.has_variable_object(&varobj_for_guard))
        })
        .request(|_, _| {});
}

pub(super) fn variable_child_page_end(from: usize) -> Option<usize> {
    (from < MAX_VARIABLE_CHILDREN).then(|| {
        from.saturating_add(VARIABLE_CHILD_PAGE_SIZE)
            .min(MAX_VARIABLE_CHILDREN)
    })
}

fn request_lazy_local_variable_children(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    variable: Variable,
    from: usize,
) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    let generation = current_ui.model.current_stop_refresh_generation();

    if from != 0 || !current_ui.claim_local_variable_object(generation, &variable) {
        return;
    }

    drop(current_ui);
    let varobj = next_variable_object_name();

    let command = format!(
        "-var-create {varobj} * {}",
        crate::debugger::quote(&variable.name)
    );

    let Some(requests) = stop_requests(&ui, &client, generation) else {
        if let Some(ui) = ui.upgrade() {
            ui.finish_local_variable_object(generation, &variable);
        }

        return;
    };

    let ui_for_guard = ui.clone();
    let variable_for_guard = variable.clone();
    let ui_for_response = ui.clone();
    let variable_for_response = variable.clone();
    let client_for_response = Rc::clone(&client);
    let varobj_for_response = varobj.clone();

    if requests
        .frame(&command)
        .when(move || {
            ui_for_guard
                .upgrade()
                .is_some_and(|ui| ui.has_local_variable_identity(&variable_for_guard))
        })
        .with_print_limit(AUTOMATIC_PRINT_ELEMENTS, move |client, record| {
            if let Some(ui) = ui_for_response.upgrade() {
                ui.finish_local_variable_object(generation, &variable_for_response);
            }

            let created = record
                .is_done()
                .then(|| crate::debugger::variable_object(&record, &variable_for_response.name))
                .flatten()
                .map(|mut created| {
                    created.argument = variable_for_response.argument;
                    created.local_index = variable_for_response.local_index;

                    created
                });

            let Some(created) = created else {
                delete_variable_object(client, &varobj_for_response);

                if let Some(ui) = ui_for_response.upgrade()
                    && ui.model.is_stop_refresh_current(generation)
                {
                    ui.show_lazy_variable_children_error(
                        &variable_for_response,
                        record
                            .error_message()
                            .unwrap_or("GDB could not inspect this pointer"),
                    );
                }

                return;
            };

            let attached = ui_for_response.upgrade().is_some_and(|ui| {
                ui.attach_local_variable_object(generation, &variable_for_response, &created)
            });

            if attached {
                request_variable_children(
                    ui_for_response.clone(),
                    Rc::clone(&client_for_response),
                    created,
                    0,
                );
            } else {
                delete_variable_object(client, &varobj_for_response);
            }
        })
        .is_err()
        && let Some(ui) = ui.upgrade()
    {
        ui.finish_local_variable_object(generation, &variable);

        if ui.has_local_variable_identity(&variable) {
            ui.show_lazy_variable_children_error(&variable, "The MI channel is unavailable");
        }
    }
}

#[cfg(test)]
mod tests;
