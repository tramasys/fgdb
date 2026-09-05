use super::*;

pub(super) fn edit_context_is_current(ui: &Weak<Ui>, generation: u64) -> bool {
    ui.upgrade()
        .is_some_and(|ui| ui.model.can_edit_variable(generation))
}

pub(super) fn assign_string(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    variable: Variable,
    bytes: Vec<u8>,
    kind: crate::ui::StringAssignmentKind,
) {
    let Some(generation) = ui.upgrade().and_then(|current_ui| {
        let generation = current_ui.model.current_stop_refresh_generation();

        current_ui
            .model
            .can_edit_variable(generation)
            .then_some(generation)
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
            move || assignments::edit_context_is_current(&ui_for_guard, generation),
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
                    variable,
                    path,
                    bytes,
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
    if std::str::from_utf8(&bytes).is_err() {
        show_string_assignment_error(&ui, "Rust strings must contain valid UTF-8");
        return;
    }

    let python = rust_string_assignment_python(&expression, &bytes);
    let command = crate::debugger::console_command(&format!(
        "python exec(bytes.fromhex(\"{}\").decode(), {{}})",
        type_metadata::hex(python.as_bytes())
    ));

    let Some(command) = frame_scoped_stop_command(&ui, generation, &command) else {
        show_string_assignment_error(&ui, "The selected stop context changed");
        return;
    };

    let ui_for_response = ui.clone();
    let ui_for_guard = ui.clone();

    if let Err(error) = client.request_control_for_stop(
        &command,
        generation,
        move || assignments::edit_context_is_current(&ui_for_guard, generation),
        move |client, record| {
            if record.is_done() {
                if let Some(ui) = ui_for_response.upgrade() {
                    ui.set_status(
                        "Paused",
                        &format!("Updated the contents of {}", variable.name),
                        Some("status-ready"),
                    );
                }

                refresh_stopped_state(&ui_for_response, client);
            } else if record.class != "superseded" {
                show_string_assignment_error(
                    &ui_for_response,
                    record
                        .error_message()
                        .unwrap_or("GDB could not write the Rust String buffer"),
                );
            }
        },
    ) {
        show_string_assignment_error(&ui, &error.to_string());
    }
}

fn rust_string_assignment_python(expression: &str, bytes: &[u8]) -> String {
    format!(
        r#"import gdb
v=gdb.parse_and_eval(bytes.fromhex("{}").decode())
for _ in range(8):
 t=v.type.strip_typedefs()
 if t.code == gdb.TYPE_CODE_PTR: v=v.dereference()
 elif t.code in (gdb.TYPE_CODE_REF,gdb.TYPE_CODE_RVALUE_REF): v=v.referenced_value()
 else: break
b=bytes.fromhex("{}")
b.decode("utf-8")
p=gdb.default_visualizer(v)
assert p is not None and hasattr(p,"_data_ptr"), "Rust pretty-printer cannot expose this String buffer"
assert getattr(p,"_length",None) == len(b), "Rust String length changed"
if b: gdb.selected_inferior().write_memory(p._data_ptr,b)"#,
        type_metadata::hex(expression.as_bytes()),
        type_metadata::hex(bytes),
    )
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
        move || assignments::edit_context_is_current(&ui_for_guard, generation),
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
        move || assignments::edit_context_is_current(&ui_for_guard, generation),
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
    let Some(command) = frame_scoped_stop_command(&ui, generation, &command) else {
        show_string_assignment_error(&ui, "The selected stop context changed");
        return;
    };

    let ui_for_response = ui.clone();
    let ui_for_guard = ui.clone();

    if let Err(error) = client.request_control_for_stop(
        &command,
        generation,
        move || assignments::edit_context_is_current(&ui_for_guard, generation),
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
    use super::*;

    #[test]
    fn encodes_arbitrary_std_string_bytes_for_gdb() {
        assert_eq!(
            gdb_byte_string_literal(b"A\0B\\\"\xff"),
            r#"A\000B\\\"\377"#
        );
    }

    #[test]
    #[ignore = "requires Python-enabled GDB and the built rust-variable-viewer-target fixture"]
    fn live_rust_string_and_type_metadata_roundtrip() {
        use std::{process::Command, time::Duration};

        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let toolchain =
            crate::rust_toolchain::RustToolchain::discover(root, Duration::from_secs(2)).unwrap();
        let fixture = root.join("target/debug-fixtures/rust-variable-viewer-target");
        assert!(
            fixture.is_file(),
            "build the Rust variable viewer fixture first"
        );
        let context =
            crate::debugger::StopContext::new(1, 1, Some("i1".into()), "1".into(), 1).unwrap();
        let scoped_python = |script: String| {
            let command = crate::debugger::console_command(&format!(
                "python exec(bytes.fromhex(\"{}\").decode(), {{}})",
                type_metadata::hex(script.as_bytes())
            ));
            format!(
                "interpreter-exec mi {}",
                crate::debugger::quote(&context.scope_frame(&command))
            )
        };

        let mut command = Command::new("gdb");
        command
            .args(["--nx", "--quiet", "--batch"])
            .args(toolchain.gdb_printer_arguments())
            .arg(&fixture)
            .arg("-ex")
            .arg(format!(
                "source {}",
                toolchain
                    .sysroot()
                    .join("lib/rustlib/etc/gdb_load_rust_pretty_printers.py")
                    .display()
            ))
            .args([
                "-ex",
                "set debuginfod enabled off",
                "-ex",
                "break rust_types_ready",
                "-ex",
                "run",
                "-ex",
                "frame 0",
                "-ex",
                "add-inferior",
                "-ex",
                "inferior 2",
            ]);

        let local_bytes = vec![b'L'; "UTF-8: Zürich λ 🚀".len()];
        let argument_bytes = vec![b'A'; "argument String with UTF-8 λ".len()];
        command
            .arg("-ex")
            .arg(scoped_python(rust_string_assignment_python(
                "local_string",
                &local_bytes,
            )));
        command
            .arg("-ex")
            .arg(scoped_python(rust_string_assignment_python(
                "string_arg",
                &argument_bytes,
            )));

        for invalid in [vec![0xff; local_bytes.len()], vec![b'x']] {
            let script = rust_string_assignment_python("local_string", &invalid);
            command.arg("-ex").arg(scoped_python(format!(
                r#"try:
 exec(bytes.fromhex("{}").decode(), {{}})
except (UnicodeDecodeError, AssertionError): pass
else: raise AssertionError("invalid Rust string edit was accepted")"#,
                type_metadata::hex(script.as_bytes())
            )));
        }

        command
            .arg("-ex")
            .arg(scoped_python(type_metadata::metadata_python(
                "local_primitives.float32",
            )));
        command.arg("-ex").arg(scoped_python(format!(r#"import gdb
local=gdb.default_visualizer(gdb.parse_and_eval("local_string"))
argument=gdb.default_visualizer(gdb.parse_and_eval("*string_arg"))
assert bytes(gdb.selected_inferior().read_memory(local._data_ptr,local._length)) == bytes.fromhex("{}")
assert bytes(gdb.selected_inferior().read_memory(argument._data_ptr,argument._length)) == bytes.fromhex("{}")
assert "fgdb_rs_string" not in gdb.execute("show convenience",to_string=True)
gdb.write("FGDB_EDIT_ROUNDTRIP_OK\n")"#, type_metadata::hex(&local_bytes), type_metadata::hex(&argument_bytes))));
        let output = crate::compiler_probe::output(&mut command, Duration::from_secs(15))
            .expect("live GDB smoke test failed or timed out");
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("FGDB_EDIT_ROUNDTRIP_OK"), "{output}");
        assert!(!output.contains("^error"), "{output}");
        let console_output = output
            .lines()
            .filter_map(|line| {
                let stream = line.strip_prefix('~')?;
                let record =
                    crate::debugger::parse_record(&format!("^done,value={stream}")).ok()?;
                record.field("value")?.as_const().map(str::to_owned)
            })
            .collect::<String>();
        let metadata = type_metadata::parse_metadata_output(&console_output).unwrap();
        assert_eq!(metadata.kind, crate::debugger::ValueTypeKind::Float);
        assert_eq!(metadata.bits, Some(32));
        assert_eq!(
            metadata.raw_bytes.unwrap(),
            1.25_f32.to_bits().to_be_bytes()
        );
    }
}
