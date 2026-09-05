use super::*;

pub(super) fn run_to_source_line(ui: Weak<Ui>, client: &MiClient, path: PathBuf, line: u32) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    if !current_ui.movement_commands_available() {
        current_ui.set_status(
            "Run to line unavailable",
            "The inferior must be paused before execution can run to a source line.",
            Some("status-error"),
        );

        return;
    }

    if !client.is_ready() {
        current_ui.set_status(
            "Run to line unavailable",
            "Wait for the GDB/MI channel to become ready.",
            Some("status-error"),
        );

        return;
    }

    let location = format!("{}:{line}", path.display());

    // Let GDB's location resolver decide where execution can stop, just as it
    // does for source breakpoints. Absence from a line-table query is not a
    // reason to reject a location before GDB has tried to resolve it.
    crate::ui::controls::issue_execution_command(
        &current_ui,
        client,
        &run_to_source_command(&location),
        &format!("Running to {location}"),
    );
}

fn run_to_source_command(location: &str) -> String {
    format!("-exec-until {}", crate::debugger::quote(location))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    #[ignore = "requires Python-enabled GDB and the built Rust and C++ variable viewer fixtures"]
    fn live_run_to_source_line_and_invalid_destination() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));

        for (fixture, source, marker) in [
            (
                "rust-variable-viewer-target",
                "rust_variable_viewer_target.rs",
                "let linked_list =",
            ),
            (
                "cpp-variable-viewer-target",
                "cpp_variable_viewer_target.cpp",
                "std::array<ViewerNode, 4> linear_nodes",
            ),
        ] {
            let source = root.join("examples").join(source);
            let contents = std::fs::read_to_string(&source).unwrap();
            let line = contents
                .lines()
                .position(|line| line.contains(marker))
                .unwrap()
                + 1;
            let location = format!("{}:{line}", source.display());
            let mi_command = |location: &str| {
                format!(
                    "interpreter-exec mi {}",
                    crate::debugger::quote(&run_to_source_command(location))
                )
            };
            let valid = mi_command(&location);
            let invalid_line = mi_command(&format!("{}:999999", source.display()));
            let missing_file = mi_command(&format!("{}.missing:{line}", source.display()));
            let script = format!(
                r#"gdb.execute('start', to_string=True)
reply = gdb.execute({}, to_string=True)
assert '^error' not in reply, reply
assert gdb.selected_frame().find_sal().line == {line}
assert not gdb.breakpoints(), 'Run to line left a breakpoint behind'
before = int(gdb.parse_and_eval('$pc'))
for command in [{}, {}]:
    reply = gdb.execute(command, to_string=True)
    assert '^error' in reply, reply
    assert int(gdb.parse_and_eval('$pc')) == before
    assert not gdb.selected_thread().is_running()

gdb.write('FGDB_RUN_TO_LINE_OK\n')
"#,
                crate::debugger::quote(&valid),
                crate::debugger::quote(&invalid_line),
                crate::debugger::quote(&missing_file),
            );
            let mut command = std::process::Command::new("gdb");
            command
                .args(["--nx", "--quiet", "--batch"])
                .arg(root.join("target/debug-fixtures").join(fixture))
                .args(["-ex", "set debuginfod enabled off", "-ex"])
                .arg(format!("python exec({})", crate::debugger::quote(&script)));
            let output = crate::compiler_probe::output(&mut command, Duration::from_secs(15))
                .expect("live run-to-line check failed or timed out");
            let output = String::from_utf8(output).unwrap();
            assert!(
                output.contains("FGDB_RUN_TO_LINE_OK"),
                "{fixture}: {output}"
            );
        }
    }
}
