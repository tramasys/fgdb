use super::*;
use crate::debugger::{EnumVariant, ValueTypeKind, ValueTypeMetadata};
use crate::ui::VariableEditorRequest;

const METADATA_PREFIX: &str = "FGDB_TYPE_METADATA:";
const MAX_METADATA_BYTES: usize = 256 * 1024;

pub(super) fn request_value_type_metadata(ui: Weak<Ui>, client: Rc<MiClient>, variable: Variable) {
    let Some(request) = ui
        .upgrade()
        .and_then(|ui| ui.begin_variable_editor_request())
    else {
        return;
    };

    let generation = request.generation;

    let Some(varobj) = variable.varobj.as_deref() else {
        request_resolved_metadata(ui, client, request, variable.name.clone(), variable);
        return;
    };

    let command = format!(
        "-var-info-path-expression {}",
        crate::debugger::quote(varobj)
    );

    let ui_for_response = ui.clone();
    let ui_for_guard = ui.clone();
    let client_for_response = Rc::clone(&client);
    let variable_for_response = variable.clone();

    if client
        .request_for_stop(
            &command,
            generation,
            move || {
                ui_for_guard
                    .upgrade()
                    .is_some_and(|ui| ui.variable_editor_request_is_current(request))
            },
            move |_, record| {
                if !editor_request_is_current(&ui_for_response, request) {
                    return;
                }

                let expression = crate::debugger::variable_path_expression(&record)
                    .unwrap_or_else(|| variable_for_response.name.clone());

                request_resolved_metadata(
                    ui_for_response,
                    client_for_response,
                    request,
                    expression,
                    variable_for_response,
                );
            },
        )
        .is_err()
    {
        present_editor(&ui, request, variable, None);
    }
}

pub(super) fn assign_float_bytes(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    variable: Variable,
    raw_bytes: Vec<u8>,
) {
    let Some(generation) = ui.upgrade().and_then(|current_ui| {
        let generation = current_ui.current_stop_refresh_generation();

        current_ui
            .can_edit_variable(generation)
            .then_some(generation)
    }) else {
        show_float_assignment_error(&ui, "The selected stop context changed");
        return;
    };

    let Some(varobj) = variable.varobj.as_deref() else {
        assign_resolved_float(
            ui,
            client,
            generation,
            variable.name.clone(),
            variable,
            raw_bytes,
        );

        return;
    };

    let command = format!(
        "-var-info-path-expression {}",
        crate::debugger::quote(varobj)
    );

    let ui_for_response = ui.clone();
    let ui_for_guard = ui.clone();
    let client_for_response = Rc::clone(&client);
    let variable_for_response = variable;
    let raw_for_response = raw_bytes;

    if client
        .request_for_stop(
            &command,
            generation,
            move || assignments::edit_context_is_current(&ui_for_guard, generation),
            move |_, record| {
                if record.class == "superseded" {
                    return;
                }

                let Some(expression) = crate::debugger::variable_path_expression(&record) else {
                    show_float_assignment_error(
                        &ui_for_response,
                        record
                            .error_message()
                            .unwrap_or("GDB could not resolve the selected value"),
                    );

                    return;
                };

                assign_resolved_float(
                    ui_for_response,
                    client_for_response,
                    generation,
                    expression,
                    variable_for_response,
                    raw_for_response,
                );
            },
        )
        .is_err()
    {
        show_float_assignment_error(&ui, "MI channel is unavailable");
    }
}

fn assign_resolved_float(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    generation: u64,
    expression: String,
    variable: Variable,
    raw_bytes: Vec<u8>,
) {
    let python = format!(
        r#"import gdb
v=gdb.parse_and_eval(bytes.fromhex("{}").decode())
b=bytes.fromhex("{}")
assert int(v.type.sizeof) == len(b), "floating-point storage width changed"
assert v.address is not None, "value has no writable memory address"
little="little endian" in gdb.execute("show endian",to_string=True).lower()
gdb.selected_inferior().write_memory(v.address,b[::-1] if little else b)"#,
        hex(expression.as_bytes()),
        hex(&raw_bytes),
    );

    let command = crate::debugger::console_command(&format!(
        "python exec(bytes.fromhex(\"{}\").decode())",
        hex(python.as_bytes())
    ));

    let Some(command) = frame_scoped_stop_command(&ui, generation, &command) else {
        show_float_assignment_error(&ui, "The selected stop context changed");
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
                    &format!("Updated the exact bit pattern of {}", variable.name),
                    Some("status-ready"),
                );

                refresh_stopped_state(&ui_for_response, client);
            } else if record.class != "superseded" {
                ui.set_status(
                    "Assignment failed",
                    record.error_message().unwrap_or(
                        "GDB could not write the raw bits (the value may live only in a register)",
                    ),
                    Some("status-error"),
                );
            }
        },
    ) {
        show_float_assignment_error(&ui, &error.to_string());
    }
}

fn show_float_assignment_error(ui: &Weak<Ui>, message: &str) {
    if let Some(ui) = ui.upgrade() {
        ui.set_status("Assignment failed", message, Some("status-error"));
    }
}

fn request_resolved_metadata(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    request: VariableEditorRequest,
    expression: String,
    variable: Variable,
) {
    if !editor_request_is_current(&ui, request) {
        return;
    }

    let generation = request.generation;
    let python = metadata_python(&expression);

    let command = crate::debugger::console_command(&format!(
        "python exec(bytes.fromhex(\"{}\").decode(), {{}})",
        hex(python.as_bytes())
    ));

    let Some(command) = frame_scoped_stop_command(&ui, generation, &command) else {
        return;
    };

    let ui_for_response = ui.clone();
    let ui_for_guard = ui.clone();
    let variable_for_response = variable.clone();

    if client
        .request_console_for_stop(
            &command,
            generation,
            move || editor_request_is_current(&ui_for_guard, request),
            move |_, record, output| {
                let metadata = (record.is_done()
                    && record.field("fgdb-output-truncated").is_none())
                .then(|| parse_metadata_output(&output))
                .flatten();

                present_editor(&ui_for_response, request, variable_for_response, metadata);
            },
        )
        .is_err()
    {
        present_editor(&ui, request, variable, None);
    }
}

fn editor_request_is_current(ui: &Weak<Ui>, request: VariableEditorRequest) -> bool {
    ui.upgrade()
        .is_some_and(|ui| ui.variable_editor_request_is_current(request))
}

fn present_editor(
    ui: &Weak<Ui>,
    request: VariableEditorRequest,
    variable: Variable,
    metadata: Option<ValueTypeMetadata>,
) {
    if let Some(ui) = ui.upgrade() {
        ui.present_variable_editor(request, variable, metadata);
    }
}

pub(super) fn metadata_python(expression: &str) -> String {
    format!(
        r#"import gdb
e=bytes.fromhex("{}").decode()
v=gdb.parse_and_eval(e)
t=v.type.strip_typedefs()
kind="other"
if t.code == gdb.TYPE_CODE_ENUM: kind="enum"
elif t.code == gdb.TYPE_CODE_FLT: kind="float"
elif t.code == gdb.TYPE_CODE_BOOL: kind="boolean"
elif t.code == gdb.TYPE_CODE_CHAR: kind="character"
elif t.code == gdb.TYPE_CODE_INT: kind="integer"
try: bits=str(int(t.sizeof)*8)
except Exception: bits=""
try: signed="1" if t.is_signed else "0"
except Exception: signed=""
raw=""
if kind == "float":
 try:
  b=bytes(v.bytes)
  little="little endian" in gdb.execute("show endian",to_string=True).lower()
  raw=(b[::-1] if little else b).hex()
 except Exception: pass
try: language=gdb.current_language().encode("utf-8","surrogateescape").hex()
except Exception: language=""
variants=[]
size=0
if kind == "enum":
 for f in t.fields()[:512]:
  if f.name is not None:
   assert len(f.name) <= 8192, "enum variant name exceeds the editor budget"
   entry=f.name.encode("utf-8","surrogateescape").hex()+"="+str(f.enumval)
   size+=len(entry)+1
   assert size <= {MAX_METADATA_BYTES}, "enum variants exceed the editor budget"
   variants.append(entry)
meta=";".join(["1",kind,bits,signed,raw,language]+variants)
assert len(meta) <= {MAX_METADATA_BYTES}, "type metadata exceeds the editor budget"
gdb.write("{METADATA_PREFIX}"+meta+"\n")"#,
        hex(expression.as_bytes()),
    )
}

fn parse_metadata(value: &str) -> Option<ValueTypeMetadata> {
    if value.len() > MAX_METADATA_BYTES {
        return None;
    }

    let value = value.trim();
    let mut fields = value.split(';');

    if fields.next()? != "1" {
        return None;
    }

    let kind = match fields.next()? {
        "integer" => ValueTypeKind::Integer,
        "float" => ValueTypeKind::Float,
        "enum" => ValueTypeKind::Enum,
        "boolean" => ValueTypeKind::Boolean,
        "character" => ValueTypeKind::Character,
        "other" => ValueTypeKind::Other,
        _ => return None,
    };

    let bits = match fields.next()? {
        "" => None,
        bits => Some(bits.parse().ok().filter(|bits| *bits > 0)?),
    };

    let signed = match fields.next()? {
        "1" => Some(true),
        "0" => Some(false),
        "" => None,
        _ => return None,
    };

    let raw_bytes = match fields.next()? {
        "" => None,
        raw => Some(decode_hex(raw)?),
    };

    let language = match fields.next()? {
        "" => None,
        language => Some(String::from_utf8(decode_hex(language)?).ok()?),
    };

    let enum_variants = fields
        .take(513)
        .map(|field| {
            let (name, value) = field.split_once('=')?;
            let name = String::from_utf8(decode_hex(name)?).ok()?;
            let digits = value.strip_prefix('-').unwrap_or(value);

            if name.is_empty()
                || digits.is_empty()
                || !digits.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }

            Some(EnumVariant {
                name,
                value: value.to_owned(),
            })
        })
        .collect::<Option<Vec<_>>>()?;

    if enum_variants.len() > 512 {
        return None;
    }

    Some(ValueTypeMetadata {
        kind,
        bits,
        signed,
        language,
        raw_bytes,
        enum_variants,
    })
}

pub(super) fn parse_metadata_output(output: &str) -> Option<ValueTypeMetadata> {
    let mut records = output
        .lines()
        .filter_map(|line| line.strip_prefix(METADATA_PREFIX));
    let metadata = parse_metadata(records.next()?)?;
    records.next().is_none().then_some(metadata)
}

pub(super) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut output, byte| {
            let _ = write!(output, "{byte:02x}");

            output
        },
    )
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }

    let (pairs, remainder) = value.as_bytes().as_chunks::<2>();
    debug_assert!(remainder.is_empty());

    pairs
        .iter()
        .map(|digits| {
            let digits = std::str::from_utf8(digits).ok()?;

            u8::from_str_radix(digits, 16).ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_target_type_metadata_and_enum_variants() {
        let metadata =
            parse_metadata("1;enum;16;0;;632b2b;4d6f64653a3a4f6666=0;4d6f64653a3a5265616479=3")
                .unwrap();
        assert_eq!(metadata.kind, ValueTypeKind::Enum);
        assert_eq!(metadata.bits, Some(16));
        assert_eq!(metadata.signed, Some(false));
        assert_eq!(metadata.language.as_deref(), Some("c++"));
        assert_eq!(metadata.enum_variants[1].name, "Mode::Ready");
        assert_eq!(metadata.enum_variants[1].value, "3");
    }

    #[test]
    fn preserves_float_storage_bytes_in_canonical_order() {
        let metadata = parse_metadata("1;float;64;1;3ff4000000000000;63").unwrap();

        assert_eq!(
            metadata.raw_bytes.unwrap(),
            1.25_f64.to_bits().to_be_bytes()
        );
    }

    #[test]
    fn metadata_transport_preserves_long_enums() {
        let variants = (0..512)
            .map(|index| {
                format!(
                    "{}={index}",
                    hex(format!("Mode::Variant{index}").as_bytes())
                )
            })
            .collect::<Vec<_>>()
            .join(";");
        let output =
            format!("unrelated diagnostic\n{METADATA_PREFIX}1;enum;32;0;;632b2b;{variants}\n");
        let metadata = parse_metadata_output(&output).unwrap();
        assert_eq!(metadata.enum_variants.len(), 512);
        assert_eq!(metadata.enum_variants[511].name, "Mode::Variant511");
    }

    #[test]
    fn metadata_transport_rejects_partial_ambiguous_and_oversized_records() {
        let record = format!("{METADATA_PREFIX}1;enum;32;0;;63;4d6f6465=0\n");
        assert!(parse_metadata_output(&format!("{record}{record}")).is_none());
        assert!(parse_metadata("1;enum;32;0;;63;4d6f...").is_none());
        assert!(parse_metadata(&"x".repeat(MAX_METADATA_BYTES + 1)).is_none());
        assert!(parse_metadata("1;float;32;1;nothex;63").is_none());
    }
}
