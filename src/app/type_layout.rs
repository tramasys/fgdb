//! On-demand type inspection through the shared, language-aware value path.

use super::value_path::{value_python, variable_path_root};
use super::*;
use crate::debugger::type_layout::TypeLayout;

pub(super) fn connect(ui: &Rc<Ui>, client: &Rc<MiClient>) {
    let weak = Rc::downgrade(ui);
    let client = Rc::clone(client);

    ui.connect_type_layout(move |variable, target| {
        request(weak.clone(), &client, variable, target);
    });
}

fn request(
    ui: Weak<Ui>,
    client: &MiClient,
    variable: Variable,
    target: gtk::glib::WeakRef<gtk::Box>,
) {
    let Some(current_ui) = ui
        .upgrade()
        .filter(|ui| ui.variable_action_is_current(&variable))
    else {
        return;
    };

    let Some(requests) = stop_requests(
        &ui,
        client,
        current_ui.model.current_stop_refresh_generation(),
    ) else {
        return;
    };

    if let Some(varobj) = variable.varobj.as_deref() {
        let (root, members) = variable_path_root(&variable, varobj);
        let members = members.map(str::to_owned);
        let command = format!("-var-info-path-expression {}", crate::debugger::quote(root));
        let response_requests = requests.clone();
        let response_ui = ui.clone();
        let response_target = target.clone();

        let result = requests.unscoped(&command).request(move |_, record| {
            if let Some(path) = crate::debugger::variable_path_expression(&record) {
                inspect(
                    response_ui,
                    response_requests,
                    variable,
                    &path,
                    members.as_deref(),
                    response_target,
                );
            } else {
                show_error(
                    &response_target,
                    record
                        .error_message()
                        .unwrap_or("Value path unavailable at this stop"),
                );
            }
        });

        if let Err(error) = result {
            show_error(&target, &error.to_string());
        }
    } else {
        let expression = variable.name.clone();
        inspect(ui, requests, variable, &expression, None, target);
    }
}

fn inspect(
    ui: Weak<Ui>,
    requests: StopRequests,
    variable: Variable,
    expression: &str,
    members: Option<&str>,
    target: gtk::glib::WeakRef<gtk::Box>,
) {
    let python = script(
        &value_python(expression, members),
        variable.return_value.is_some(),
    );

    let command = crate::debugger::console_command(&format!(
        "python exec({}, {{}})",
        crate::debugger::quote(&python)
    ));

    let guard_ui = ui.clone();
    let generation = requests.generation();
    let response_target = target.clone();
    let guard_target = target.clone();

    let result = requests
        .frame(&command)
        .when(move || {
            guard_target.upgrade().is_some()
                && guard_ui
                    .upgrade()
                    .is_some_and(|ui| ui.variable_action_is_current(&variable))
        })
        .capture(move |_, record, output| {
            let Some(ui) = ui.upgrade() else { return };

            if let Some(target) = response_target.upgrade() {
                let result = if record.is_done() && record.field("fgdb-output-truncated").is_none()
                {
                    parse(&output)
                        .ok_or_else(|| "Type layout output is unavailable or incomplete".into())
                } else {
                    Err(record
                        .error_message()
                        .unwrap_or("Type layout unavailable")
                        .to_owned())
                };

                ui.show_type_layout(generation, &target, result);
            }
        });

    if let Err(error) = result {
        show_error(&target, &error.to_string());
    }
}

fn script(value: &str, historical: bool) -> String {
    format!(
        "{}\nwith contextlib.ExitStack() as guard:\n for parameter in ('may-call-functions', 'may-write-memory', 'may-write-registers'):\n  guard.enter_context(gdb.with_parameter(parameter, False))\n layout({value}, {})\n",
        include_str!("../debugger/type_layout.py"),
        if historical { "True" } else { "False" }
    )
}

fn parse(output: &str) -> Option<TypeLayout> {
    let mut lines = output
        .lines()
        .filter_map(|line| line.strip_prefix("FGDB_LAYOUT "));
    let mut fields = lines.next()?.splitn(3, ' ');
    let address = fields.next()?;
    let address = if address == "-" {
        None
    } else {
        Some(u64::from_str_radix(address.strip_prefix("0x")?, 16).ok()?)
    };
    let bytes = fields.next()?.parse().ok()?;
    let encoded = fields.next()?;

    if encoded.len() > 132 * 1024 || lines.next().is_some() {
        return None;
    }

    let text = String::from_utf8(decode_hex(encoded)?).ok()?;

    TypeLayout::parse(&text, address, bytes)
}

fn show_error(target: &gtk::glib::WeakRef<gtk::Box>, message: &str) {
    if let Some(target) = target.upgrade() {
        Ui::show_type_layout_error(&target, message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_protocol_is_bounded_and_preserves_absent_storage() {
        let missing = parse("FGDB_LAYOUT - 4 7479706509696e7409340934").unwrap();
        assert_eq!(missing.address, None);
        assert_eq!(missing.bytes, 4);
        assert!(missing.text().contains("Type  int"));
        let zero = parse("FGDB_LAYOUT 0x0 4 7479706509696e7409340934").unwrap();
        assert_eq!(zero.address, Some(0));
        assert_eq!(zero.rows, missing.rows);
        assert_eq!(parse("FGDB_LAYOUT - 4 gg"), None);
        assert_eq!(parse("FGDB_LAYOUT - 4 74\nFGDB_LAYOUT - 4 74"), None);

        for text in [
            "",
            "type",
            "field\t0\t8\tx\tdouble",
            "type\tint\t4\t4\npointee\tint\t4\npointee\tint\t4",
            "type\tint\t4\t4\nfield\t0\t4\tx",
            "type\tint\t4\t4\nnote\tbad\0text",
        ] {
            assert!(TypeLayout::parse(text, None, 4).is_none(), "{text:?}");
        }

        let text = format!("type\tint\t4\t4\n{}", "note\tx\n".repeat(257));
        assert!(TypeLayout::parse(&text, None, 4).is_none());
    }

    #[test]
    #[ignore = "requires Python-enabled GDB and the nine language fixtures"]
    fn live_layouts_use_native_language_metadata_without_side_effects() {
        use std::{process::Command, time::Duration};

        for (fixture, breakpoint, caller, expression, expected) in [
            (
                "c-aggregate-return-target",
                "return_mixed",
                true,
                "floats",
                "volatile Floats",
            ),
            (
                "c-variable-location-target",
                "location_checkpoint",
                false,
                "record",
                "flags",
            ),
            (
                "cpp-variable-viewer-target",
                "variable_viewer_checkpoint",
                false,
                "*linear_head",
                "samples",
            ),
            (
                "rust-variable-viewer-target",
                "rust_types_ready",
                true,
                "local_primitives",
                "unsigned",
            ),
            (
                "d-variable-viewer-target",
                "d_values_ready",
                true,
                "particle",
                "position",
            ),
            (
                "ada-variable-viewer-target",
                "ada_values_ready",
                true,
                "sample",
                "position",
            ),
            (
                "c3-variable-viewer-target",
                "c3_variable_viewer_target.c3_values_ready",
                true,
                "particle",
                "position",
            ),
            (
                "fortran-variable-viewer-target",
                "fortran_variable_viewer_target.f90:30",
                false,
                "sample",
                "position",
            ),
            (
                "zig-variable-viewer-target",
                "zig_variable_viewer_target.zig:18",
                false,
                "particle",
                "position",
            ),
            (
                "odin-variable-viewer-target",
                "odin_variable_viewer_target.odin:27",
                false,
                "particle",
                "position",
            ),
        ] {
            let executable = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/debug-fixtures")
                .join(fixture);
            let python = script(&value_python(expression, None), false);
            let mut command = Command::new("gdb");
            command
                .args(["--nx", "--batch", "-ex", "set debuginfod enabled off"])
                .arg(executable)
                .arg("-ex")
                .arg(format!("break {breakpoint}"))
                .args(["-ex", "run"]);

            if caller {
                command.args(["-ex", "up"]);
            }

            command.arg("-ex").arg(format!("python exec({}, {{}})", crate::debugger::quote(&python)))
                .args(["-ex", "python assert all(gdb.parameter(p) for p in ('may-call-functions', 'may-write-memory', 'may-write-registers'))"]);

            let output = crate::bounded::process::output(&mut command, Duration::from_secs(15))
                .expect(fixture);
            let output = String::from_utf8(output).unwrap();
            let layout = parse(&output).unwrap_or_else(|| panic!("{fixture}: {output}"));
            let text = layout.text();
            assert!(text.contains(expected), "{fixture}: {text}");
            assert!(
                layout.address.is_some() && layout.bytes > 0,
                "{fixture}: {text}"
            );

            if fixture == "c-variable-location-target" {
                assert!(text.contains("3b"), "{text}");
                assert!(text.contains("<padding>"), "{text}");
                assert!(text.contains("No pointee memory was read"), "{text}");
            }
        }
    }
}
