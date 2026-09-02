use super::*;

pub(super) fn assign_string(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    variable: Variable,
    bytes: Vec<u8>,
    kind: crate::ui::StringAssignmentKind,
) {
    let Some(generation) = ui.upgrade().and_then(|current_ui| {
        let generation = current_ui.current_stop_refresh_generation();
        current_ui.stop_context(generation).map(|_| generation)
    }) else {
        show_string_assignment_error(&ui, "The selected stop context changed");
        return;
    };
    if let Some(varobj) = variable.varobj.as_deref() {
        let command = format!(
            "-var-info-path-expression {}",
            crate::debugger::quote(varobj)
        );
        let ui_for_response = ui.clone();
        let ui_for_guard = ui.clone();
        let client_for_response = Rc::clone(&client);
        if let Err(error) = client.request_for_stop(
            &command,
            generation,
            move || stop_refresh_is_current(&ui_for_guard, generation),
            move |_, record| {
                if record.class == "superseded" {
                    return;
                }
                let Some(path) = crate::debugger::variable_path_expression(&record) else {
                    show_string_assignment_error(
                        &ui_for_response,
                        record
                            .error_message()
                            .unwrap_or("GDB could not resolve the selected string expression"),
                    );
                    return;
                };
                assign_resolved_string(
                    ui_for_response,
                    client_for_response,
                    generation,
                    variable.clone(),
                    path,
                    bytes.clone(),
                    kind,
                );
            },
        ) {
            show_string_assignment_error(&ui, &error.to_string());
        }
    } else {
        let expression = variable.name.clone();
        assign_resolved_string(ui, client, generation, variable, expression, bytes, kind);
    }
}

fn assign_resolved_string(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    generation: u64,
    variable: Variable,
    expression: String,
    bytes: Vec<u8>,
    kind: crate::ui::StringAssignmentKind,
) {
    match kind {
        crate::ui::StringAssignmentKind::Buffer => {
            resolve_string_address(ui, client, generation, variable, expression, bytes);
        }
        crate::ui::StringAssignmentKind::CppString => {
            assign_cpp_string(ui, client, generation, variable, expression, bytes);
        }
        crate::ui::StringAssignmentKind::RustString => {
            assign_rust_string(ui, client, generation, variable, expression, bytes);
        }
    }
}

fn assign_rust_string(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    generation: u64,
    variable: Variable,
    expression: String,
    bytes: Vec<u8>,
) {
    let already_a_reference = variable
        .type_name
        .as_deref()
        .is_some_and(|type_name| type_name.contains(['&', '*']));
    let address = if already_a_reference {
        format!("({expression})")
    } else {
        format!("&({expression})")
    };
    let command = format!(
        "-data-evaluate-expression {}",
        crate::debugger::quote(&format!("$fgdb_rs_string = {address}"))
    );
    let Some(command) = frame_scoped_stop_command(&ui, generation, &command) else {
        show_string_assignment_error(&ui, "The selected stop context changed");
        return;
    };
    let ui_for_response = ui.clone();
    let ui_for_guard = ui.clone();
    let client_for_response = Rc::clone(&client);
    if let Err(error) = client.request_control_for_stop(
        &command,
        generation,
        move || stop_refresh_is_current(&ui_for_guard, generation),
        move |_, record| {
            if record.class == "superseded" {
                return;
            }
            if !record.is_done() {
                show_string_assignment_error(
                    &ui_for_response,
                    record
                        .error_message()
                        .unwrap_or("GDB could not resolve the selected Rust String"),
                );
                return;
            }
            resolve_rust_string_buffer(
                ui_for_response,
                client_for_response,
                generation,
                variable.name.clone(),
                bytes,
            );
        },
    ) {
        show_string_assignment_error(&ui, &error.to_string());
    }
}

fn resolve_rust_string_buffer(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    generation: u64,
    name: String,
    bytes: Vec<u8>,
) {
    let python = format!(
        "python p=gdb.default_visualizer(gdb.parse_and_eval(\"*$fgdb_rs_string\")); assert p is not None and hasattr(p, \"_data_ptr\") and getattr(p, \"_length\", None) == {}; gdb.set_convenience_variable(\"fgdb_rs_data\", p._data_ptr)",
        bytes.len()
    );
    let command = crate::debugger::console_command(&python);
    let Some(command) = frame_scoped_stop_command(&ui, generation, &command) else {
        show_string_assignment_error(&ui, "The selected stop context changed");
        return;
    };
    let ui_for_response = ui.clone();
    let ui_for_guard = ui.clone();
    let client_for_response = Rc::clone(&client);
    if let Err(error) = client.request_control_for_stop(
        &command,
        generation,
        move || stop_refresh_is_current(&ui_for_guard, generation),
        move |_, record| {
            if record.class == "superseded" {
                return;
            }
            if !record.is_done() {
                show_string_assignment_error(
                    &ui_for_response,
                    "The active Rust pretty-printer could not expose a matching String buffer",
                );
                return;
            }
            let ui_for_address = ui_for_response.clone();
            let ui_for_address_guard = ui_for_response.clone();
            let client_for_address = Rc::clone(&client_for_response);
            let name_for_address = name.clone();
            let bytes_for_address = bytes.clone();
            let evaluate = "-data-evaluate-expression $fgdb_rs_data";
            let Some(evaluate) = frame_scoped_stop_command(&ui_for_response, generation, evaluate)
            else {
                return;
            };
            if let Err(error) = client_for_response.request_for_stop(
                &evaluate,
                generation,
                move || stop_refresh_is_current(&ui_for_address_guard, generation),
                move |_, record| {
                    if record.class == "superseded" {
                        return;
                    }
                    let Some(address) = crate::debugger::evaluated_value(&record)
                        .as_deref()
                        .and_then(pointer_address)
                    else {
                        show_string_assignment_error(
                            &ui_for_address,
                            record
                                .error_message()
                                .unwrap_or("GDB could not resolve the Rust String buffer address"),
                        );
                        return;
                    };
                    write_string_bytes(
                        ui_for_address,
                        client_for_address,
                        generation,
                        name_for_address.clone(),
                        address,
                        bytes_for_address,
                        false,
                    );
                },
            ) {
                show_string_assignment_error(&ui_for_response, &error.to_string());
            }
        },
    ) {
        show_string_assignment_error(&ui, &error.to_string());
    }
}

fn assign_cpp_string(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    generation: u64,
    variable: Variable,
    expression: String,
    bytes: Vec<u8>,
) {
    let literal = gdb_byte_string_literal(&bytes);
    let access = if variable
        .type_name
        .as_deref()
        .is_some_and(|type_name| type_name.contains('*'))
    {
        format!("({expression})->")
    } else {
        format!("({expression}).")
    };
    let assignment = format!("(void){access}assign(\"{literal}\", {})", bytes.len());
    let command = format!(
        "-data-evaluate-expression {}",
        crate::debugger::quote(&assignment)
    );
    let Some(command) = frame_scoped_stop_command(&ui, generation, &command) else {
        show_string_assignment_error(&ui, "The selected stop context changed");
        return;
    };
    let name = variable.name;
    let ui_for_response = ui.clone();
    let ui_for_guard = ui.clone();
    if let Err(error) = client.request_control_for_stop(
        &command,
        generation,
        move || stop_refresh_is_current(&ui_for_guard, generation),
        move |client, record| {
            let Some(ui) = ui_for_response.upgrade() else {
                return;
            };
            if record.is_done() {
                ui.set_status(
                    "Paused",
                    &format!("Updated the contents of {name}"),
                    Some("status-ready"),
                );
                refresh_stopped_state(&ui_for_response, client);
            } else if record.class != "superseded" {
                ui.set_status(
                    "String assignment failed",
                    record
                        .error_message()
                        .unwrap_or("GDB could not call std::string::assign"),
                    Some("status-error"),
                );
            }
        },
    ) {
        show_string_assignment_error(&ui, &error.to_string());
    }
}

fn gdb_byte_string_literal(bytes: &[u8]) -> String {
    let mut literal = String::with_capacity(bytes.len());
    for byte in bytes {
        match *byte {
            b'\\' => literal.push_str("\\\\"),
            b'\"' => literal.push_str("\\\""),
            0x20..=0x7e => literal.push(char::from(*byte)),
            _ => literal.push_str(&format!("\\{byte:03o}")),
        }
    }
    literal
}

fn resolve_string_address(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    generation: u64,
    variable: Variable,
    expression: String,
    bytes: Vec<u8>,
) {
    let address_expression = if variable.is_pointer() {
        format!("(void*)({expression})")
    } else {
        format!("(void*)&({expression})")
    };
    let command = format!(
        "-data-evaluate-expression {}",
        crate::debugger::quote(&address_expression)
    );
    let Some(command) = frame_scoped_stop_command(&ui, generation, &command) else {
        show_string_assignment_error(&ui, "The selected stop context changed");
        return;
    };
    let ui_for_response = ui.clone();
    let ui_for_guard = ui.clone();
    let client_for_response = Rc::clone(&client);
    if let Err(error) = client.request_for_stop(
        &command,
        generation,
        move || stop_refresh_is_current(&ui_for_guard, generation),
        move |_, record| {
            if record.class == "superseded" {
                return;
            }
            let Some(address) = crate::debugger::evaluated_value(&record)
                .as_deref()
                .and_then(pointer_address)
            else {
                show_string_assignment_error(
                    &ui_for_response,
                    record
                        .error_message()
                        .unwrap_or("GDB could not resolve the string buffer address"),
                );
                return;
            };
            write_string_bytes(
                ui_for_response,
                client_for_response,
                generation,
                variable.name.clone(),
                address,
                bytes,
                true,
            );
        },
    ) {
        show_string_assignment_error(&ui, &error.to_string());
    }
}

fn write_string_bytes(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    generation: u64,
    name: String,
    address: u64,
    mut bytes: Vec<u8>,
    nul_terminate: bool,
) {
    if nul_terminate {
        bytes.push(0);
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    let command = format!("-data-write-memory-bytes 0x{address:x} {encoded}");
    let ui_for_response = ui.clone();
    let ui_for_guard = ui.clone();
    if let Err(error) = client.request_control_for_stop(
        &command,
        generation,
        move || stop_refresh_is_current(&ui_for_guard, generation),
        move |client, record| {
            let Some(ui) = ui_for_response.upgrade() else {
                return;
            };
            if record.is_done() {
                ui.set_status(
                    "Paused",
                    &format!("Updated the contents of {name}"),
                    Some("status-ready"),
                );
                refresh_stopped_state(&ui_for_response, client);
            } else if record.class != "superseded" {
                ui.set_status(
                    "String assignment failed",
                    record
                        .error_message()
                        .unwrap_or("GDB could not write the string buffer"),
                    Some("status-error"),
                );
            }
        },
    ) {
        show_string_assignment_error(&ui, &error.to_string());
    }
}

fn show_string_assignment_error(ui: &Weak<Ui>, detail: &str) {
    if let Some(ui) = ui.upgrade() {
        ui.set_status("String assignment failed", detail, Some("status-error"));
    }
}

#[cfg(test)]
mod tests {
    use super::gdb_byte_string_literal;

    #[test]
    fn encodes_arbitrary_std_string_bytes_for_gdb() {
        assert_eq!(
            gdb_byte_string_literal(b"A\0B\\\"\xff"),
            r#"A\000B\\\"\377"#
        );
    }
}
