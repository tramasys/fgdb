use super::*;
use crate::debugger::{InferiorInfo, InferiorState};

fn stop_reason(record: &str, hit_breakpoint: bool) -> Option<&'static str> {
    let record = crate::debugger::parse_record(record).unwrap();
    verified_stop_reason(&crate::debugger::threads(&record), hit_breakpoint)
}

#[test]
fn historical_breakpoints_do_not_prove_a_stopped_process() {
    assert_eq!(stop_reason(r#"^done,threads=[]"#, true), None);

    let stopped = r#"^done,threads=[{id="1",state="stopped"}],current-thread-id="1""#;
    assert_eq!(stop_reason(stopped, true), Some("breakpoint-hit"));
    assert_eq!(stop_reason(stopped, false), Some("stopped"));

    assert_eq!(
        stop_reason(r#"^done,threads=[{id="1",state="stopped"}]"#, true),
        None
    );

    assert_eq!(
        stop_reason(
            r#"^done,threads=[{id="1",state="running"},{id="2",state="stopped"}],current-thread-id="1""#,
            true,
        ),
        None
    );
}

#[test]
fn startup_probe_rejects_changes_in_backend_selection_and_execution() {
    let model = DebuggerModel::new(None);
    model.set_controls_ready(true);

    model.show_inferiors(
        ["i1", "i2"]
            .into_iter()
            .map(|id| InferiorInfo {
                id: id.to_owned(),
                pid: None,
                executable: None,
                exit_code: None,
                state: InferiorState::NotStarted,
                threads: Vec::new(),
            })
            .collect(),
    );

    let context = ProbeContext::new(&model, 1);
    assert!(context.is_current(&model, 1));
    assert!(!context.is_current(&model, 2));
    model.set_command_pending(true);
    assert!(!context.is_current(&model, 1));
    model.set_command_pending(false);
    model.set_inferior_action_pending(Some(InferiorActionPending::Selection));
    assert!(!context.is_current(&model, 1));
    model.clear_inferior_action_pending();
    model.set_resynchronizing(true);
    assert!(!context.is_current(&model, 1));
    model.set_resynchronizing(false);
    model.set_session_pending(true);
    assert!(!context.is_current(&model, 1));
    model.set_session_pending(false);
    model.set_current_thread_id(Some("1"));
    assert!(!context.is_current(&model, 1));
    model.set_current_thread_id(None);
    model.set_selected_inferior("i2");
    assert!(!context.is_current(&model, 1));
    model.set_selected_inferior("i1");
    assert!(context.is_current(&model, 1));
    model.begin_execution_transition();
    assert!(!context.is_current(&model, 1));
    model.finish_execution_transition();
    model.set_controls_running(true);
    assert!(!context.is_current(&model, 1));
    model.set_controls_running(false);
    model.start_stop_refresh();
    assert!(!context.is_current(&model, 1));
}

#[test]
#[ignore = "requires Python-enabled GDB and the built C memory-search fixture"]
fn live_exited_inferior_retains_breakpoint_history_without_a_stopped_thread() {
    let executable =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug-fixtures/c-memory-search-target");

    let script = r#"gdb.execute('break main', to_string=True)
for attempt in range(2):
    gdb.execute('run', to_string=True)
    gdb.write(gdb.execute('interpreter-exec mi "-thread-info --thread-group i1"', to_string=True))
    gdb.execute('continue', to_string=True)
    gdb.execute('inferior 1', to_string=True)
    assert int(gdb.parse_and_eval('$_hit_bpnum')) > 0
    assert not gdb.selected_inferior().threads()
    gdb.write(gdb.execute('interpreter-exec mi "-thread-info --thread-group i1"', to_string=True))
gdb.write('FGDB_SWITCH_RERUN_OK\n')
"#;

    let mut command = std::process::Command::new("gdb");

    command
        .args([
            "--nx",
            "--quiet",
            "--batch",
            "-ex",
            "set confirm off",
            "-ex",
            "set debuginfod enabled off",
            "-ex",
        ])
        .arg(format!("python exec({})", crate::debugger::quote(script)))
        .arg(executable);

    let output =
        crate::language::toolchain::probe::output(&mut command, std::time::Duration::from_secs(15))
            .expect("live inferior-switch check failed or timed out");

    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("FGDB_SWITCH_RERUN_OK"), "{output}");

    let stops: Vec<_> = output
        .lines()
        .filter(|line| line.starts_with("^done,threads="))
        .map(|line| stop_reason(line, true))
        .collect();

    assert_eq!(
        stops,
        [Some("breakpoint-hit"), None, Some("breakpoint-hit"), None]
    );
}
