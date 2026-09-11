//! Typed return inspection uses absolute GDB history references, never a call.

use super::*;

pub(super) struct ReturnSnapshot<'a> {
    pub thread: &'a str,
    pub inferior: &'a str,
    pub value: crate::debugger::ReturnValue,
    pub replaces: Option<&'a str>,
}

pub(super) fn refresh(ui: &Weak<Ui>, client: &MiClient, generation: u64) {
    let Some(current) = ui.upgrade() else {
        return;
    };

    for varobj in current.take_deferred_variable_object_deletions() {
        delete_variable_object(client, &varobj);
    }

    let enabled = current.return_values_enabled();
    let revision = current.model.return_value_revision();
    let discard = current.return_value_capture_setting_dirty();

    if (!enabled && !current.return_value_capture_setting_dirty())
        || !current.model.gdb_capabilities().language_printers
        || current.model.target_connection() != crate::model::TargetConnection::Local
    {
        return;
    }

    let Some(requests) = stop_requests(ui, client, generation) else {
        return;
    };

    let command = crate::language::python::return_values_command(enabled, discard);
    let response = ui.clone();
    let guard = ui.clone();

    let _ = requests
        .thread(&command)
        .when(move || {
            guard.upgrade().is_some_and(|ui| {
                ui.return_values_enabled() == enabled
                    && ui.model.return_value_revision() == revision
            })
        })
        .capture(move |_, record, output| {
            if !record.is_done() || record.field("fgdb-output-truncated").is_some() {
                return;
            }

            if let Some(ui) = response.upgrade() {
                ui.acknowledge_return_value_capture_setting(enabled);
            }

            let Some(ReturnSnapshot {
                thread,
                inferior,
                value,
                replaces,
            }) = parse_snapshot(&output)
            else {
                return;
            };

            if let Some(ui) = response.upgrade()
                && ui.model.is_stop_refresh_current(generation)
                && ui.return_values_enabled()
                && ui.model.current_thread_id().as_deref() == Some(thread)
                && ui.model.selected_inferior_id().as_deref() == Some(inferior)
            {
                ui.model
                    .record_verified_return_value(value, thread, inferior, replaces);

                ui.refresh_return_values();
            }
        });
}

pub(super) fn parse_snapshot(output: &str) -> Option<ReturnSnapshot<'_>> {
    if output.len() > 128 * 1024 {
        return None;
    }

    let mut records = output
        .lines()
        .filter_map(|line| line.strip_prefix("FGDB_RETURNS "));

    let fields = records.next()?;

    if records.next().is_some() {
        return None;
    }

    let mut fields = fields.splitn(5, ' ');
    let thread = fields.next()?;
    let inferior = fields.next()?;
    let history = fields.next()?;
    let digits = |value: &str| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit());

    let absolute_reference = |value: &str| {
        value
            .strip_prefix('$')
            .is_some_and(|value| digits(value) && !value.starts_with('0'))
    };

    if !digits(thread)
        || !inferior.strip_prefix('i').is_some_and(digits)
        || !absolute_reference(history)
    {
        return None;
    }

    let value = String::from_utf8(super::type_metadata::decode_hex(fields.next()?)?).ok()?;
    let replaces = fields.next();

    if replaces.is_some_and(|reference| !absolute_reference(reference) || reference == history) {
        return None;
    }

    Some(ReturnSnapshot {
        thread,
        inferior,
        value: crate::debugger::ReturnValue {
            value,
            history_variable: Some(history.into()),
        },
        replaces,
    })
}

pub(super) fn connect(ui: &Rc<Ui>, client: &Rc<MiClient>) {
    let weak = Rc::downgrade(ui);
    let capture_ui = weak.clone();
    let capture_client = Rc::clone(client);
    let client = Rc::clone(client);

    ui.connect_return_values(
        move |variable| {
            request(weak.clone(), Rc::clone(&client), variable, false);
        },
        move || {
            if let Some(ui) = capture_ui.upgrade() {
                refresh(
                    &capture_ui,
                    &capture_client,
                    ui.model.current_stop_refresh_generation(),
                );
            }
        },
    );
}

pub(super) fn request(ui: Weak<Ui>, client: Rc<MiClient>, variable: Variable, expand: bool) {
    let Some(current) = ui.upgrade() else {
        return;
    };

    if !current.claim_return_value_object(&variable) {
        return;
    }

    let generation = current.model.current_stop_refresh_generation();

    let Some(requests) = stop_requests(&ui, &client, generation) else {
        current.attach_return_value_object(generation, &variable, None);
        return;
    };

    for varobj in current.take_deferred_variable_object_deletions() {
        delete_variable_object(&client, &varobj);
    }

    drop(current);
    let name = next_variable_object_name();
    let command = format!(
        "-var-create {name} * {}",
        crate::debugger::quote(&variable.name)
    );

    let guard = ui.clone();
    let guarded_variable = variable.clone();
    let response = ui.clone();
    let original = variable.clone();
    let client_for_response = Rc::clone(&client);

    if requests
        .frame(&command)
        .when(move || {
            guard
                .upgrade()
                .is_some_and(|ui| ui.variable_action_is_current(&guarded_variable))
        })
        .with_print_limit(AUTOMATIC_PRINT_ELEMENTS, move |client, record| {
            let created = record
                .is_done()
                .then(|| crate::debugger::variable_object(&record, &original.name))
                .flatten()
                .map(|mut created| {
                    created.return_value = original.return_value;
                    created
                });

            let attached = response.upgrade().is_some_and(|ui| {
                ui.attach_return_value_object(generation, &original, created.clone())
            });

            if !attached {
                delete_variable_object(client, &name);

                if expand && let Some(ui) = response.upgrade() {
                    ui.show_lazy_variable_children_error(
                        &original,
                        record
                            .error_message()
                            .unwrap_or("The saved return value is unavailable"),
                    );
                }

                return;
            }

            if expand && let Some(created) = created {
                request_variable_children(response, client_for_response, created, 0);
            }
        })
        .is_err()
        && let Some(ui) = ui.upgrade()
    {
        ui.attach_return_value_object(generation, &variable, None);
    }
}
