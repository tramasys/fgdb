use super::test_support::{open_debugger_observing, request, wait_until};
use super::*;
use return_values::ReturnSnapshot;

fn snapshot(client: &MiClient) -> String {
    console_output(
        client,
        "python __import__('_fgdb_languages_v1.returns', fromlist=['*']).snapshot()",
    )
}

fn console_output(client: &MiClient, command: &str) -> String {
    let result = Rc::new(RefCell::new(None));
    let response = Rc::clone(&result);

    client
        .request_console(command, move |_, record, output| {
            response.replace(Some((record, output)));
        })
        .unwrap();

    wait_until(|| result.borrow().is_some());
    let (record, output) = result.take().unwrap();
    assert!(record.is_done(), "{record:?}\n{output}");
    output
}

#[test]
fn captured_return_transport_rejects_ambiguous_and_invalid_payloads() {
    let good = "FGDB_RETURNS 1 i1 $2 3432\n";

    let ReturnSnapshot {
        thread,
        inferior,
        value,
        ..
    } = return_values::parse_snapshot(good).unwrap();

    assert_eq!(thread, "1");
    assert_eq!(inferior, "i1");
    assert_eq!(value.value, "42");
    assert_eq!(value.history_variable.as_deref(), Some("$2"));

    for bad in [
        "FGDB_RETURNS none",
        "FGDB_RETURNS all i1 $2 3432",
        "FGDB_RETURNS 1 i1 $$ 3432",
        "FGDB_RETURNS 1 i1 $0 3432",
        "FGDB_RETURNS 1 i1 $2 3",
        "FGDB_RETURNS 1 i1 $2 ff",
        "FGDB_RETURNS 1 i1 $2 3432 $2",
        "FGDB_RETURNS 1 i1 $2 3432 $$",
        "FGDB_RETURNS 1 i1 $2 3432 $1 $3",
    ] {
        assert!(return_values::parse_snapshot(bad).is_none(), "{bad}");
    }

    assert!(return_values::parse_snapshot(&format!("{good}{good}")).is_none());

    let verified = return_values::parse_snapshot("FGDB_RETURNS 1 i1 $2 3432 $1").unwrap();
    assert_eq!(verified.replaces, Some("$1"));
}

#[test]
#[ignore = "requires GDB and the Rust return-value fixture"]
fn live_finish_uses_verified_native_aggregate_fields_without_duplicate_results() {
    let native = Rc::new(RefCell::new(None));
    let observed = Rc::clone(&native);

    let (_debugger, client) = open_debugger_observing(
        "rust-return-value-target",
        "rust_return_value_target::return_pair",
        false,
        move |event| {
            if let MiEvent::Stopped {
                return_value: Some(value),
                ..
            } = event
            {
                observed.replace(Some(value.clone()));
            }
        },
    );

    assert!(return_values::parse_snapshot(&snapshot(&client)).is_none());
    assert_eq!(request(&client, "-exec-finish").class, "running");
    wait_until(|| native.borrow().is_some());
    let native = native.take().unwrap();
    let output = snapshot(&client);

    let reference = if let Some(verified) = return_values::parse_snapshot(&output) {
        assert_eq!(verified.replaces, native.history_variable.as_deref());
        verified.value.history_variable.unwrap()
    } else {
        native.history_variable.unwrap()
    };

    console_output(
        &client,
        &format!(
            "python v = gdb.history({}); assert int(v['x']) == 7 and int(v['y']) == 11, str(v)",
            &reference[1..]
        ),
    );

    assert!(return_values::parse_snapshot(&snapshot(&client)).is_none());
}

#[test]
#[ignore = "requires GDB and c-return-value-target, run separately"]
fn live_instruction_returns_are_typed_bounded_and_do_not_add_breakpoints() {
    let stopped = Rc::new(Cell::new(false));
    let observed = Rc::clone(&stopped);

    let (_debugger, client) =
        open_debugger_observing("c-return-value-target", "main", false, move |event| {
            if matches!(event, MiEvent::Stopped { .. }) {
                observed.set(true);
            }
        });

    let mut values = Vec::new();

    for _ in 0..14 {
        let output = snapshot(&client);

        if let Some(ReturnSnapshot {
            thread,
            inferior,
            value,
            ..
        }) = return_values::parse_snapshot(&output)
        {
            assert_eq!(thread, "1");
            assert_eq!(inferior, "i1");
            let expression = value.history_variable.as_deref().unwrap();

            let typed = request(
                &client,
                &format!("-var-create returned{} * {expression}", values.len()),
            );

            assert!(typed.is_done(), "{typed:?}");

            assert!(
                crate::debugger::variable_object(&typed, expression)
                    .unwrap()
                    .type_name
                    .is_some()
            );

            values.push(value.value);
        }

        stopped.set(false);
        assert_eq!(request(&client, "-exec-next-instruction").class, "running");
        wait_until(|| stopped.get());
    }

    assert!(values.iter().any(|value| value == "42"), "{values:?}");
    assert!(values.iter().any(|value| value == "3.25"), "{values:?}");

    assert!(
        values
            .iter()
            .any(|value| value.contains("fgdb return value")),
        "{values:?}"
    );

    assert!(
        values.iter().any(|value| value == "{x = 7, y = 11}"),
        "{values:?}"
    );

    assert_eq!(values.len(), 4, "void returns must not be guessed");

    let check = request(
        &client,
        &crate::debugger::console_command("python assert len(gdb.breakpoints()) == 1"),
    );

    assert!(check.is_done(), "{check:?}");
}

#[test]
#[ignore = "requires GDB and c-return-value-target, run separately"]
fn live_source_steps_and_instruction_step_out_capture_verified_results() {
    for (checkpoint, command, steps, expected) in [
        (
            "main",
            "-exec-next",
            4,
            vec!["42", "3.25", "fgdb return value", "{x = 7, y = 11}"],
        ),
        ("return_integer", "-exec-step-instruction", 3, vec!["42"]),
        ("return_float", "-exec-next", 2, vec!["3.25"]),
    ] {
        let stopped = Rc::new(Cell::new(false));
        let observed = Rc::clone(&stopped);

        let (_debugger, client) =
            open_debugger_observing("c-return-value-target", checkpoint, false, move |event| {
                if matches!(event, MiEvent::Stopped { .. }) {
                    observed.set(true);
                }
            });

        assert!(return_values::parse_snapshot(&snapshot(&client)).is_none());
        let mut returned = Vec::new();

        for _ in 0..steps {
            stopped.set(false);
            assert_eq!(request(&client, command).class, "running");
            wait_until(|| stopped.get());

            if let Some(ReturnSnapshot { value, .. }) =
                return_values::parse_snapshot(&snapshot(&client))
            {
                returned.push(value.value);
            }
        }

        assert_eq!(
            returned.len(),
            expected.len(),
            "{checkpoint} {command}: {returned:?}"
        );

        for (value, expected) in returned.iter().zip(expected) {
            assert!(value.contains(expected), "{checkpoint} {command}: {value}");
        }
    }
}

#[test]
#[ignore = "requires GDB and all language return-value fixtures"]
fn live_aggregate_returns_preserve_native_language_layouts_and_children() {
    for (fixture, checkpoint) in [
        ("c-return-value-target", "return_pair"),
        ("cpp-return-value-target", "return_pair"),
        (
            "rust-return-value-target",
            "rust_return_value_target::return_pair",
        ),
        ("d-return-value-target", "d_return_value_target.return_pair"),
        (
            "dmd-return-value-target",
            "d_return_value_target.return_pair",
        ),
        (
            "ada-return-value-target",
            "ada_return_value_target.return_pair",
        ),
        ("fortran-return-value-target", "return_values::return_pair"),
        (
            "zig-return-value-target",
            "zig_return_value_target.return_pair",
        ),
        ("odin-return-value-target", "main::return_pair"),
    ] {
        let stopped = Rc::new(Cell::new(false));
        let observed = Rc::clone(&stopped);

        let (_debugger, client) =
            open_debugger_observing(fixture, checkpoint, false, move |event| {
                if matches!(event, MiEvent::Stopped { .. }) {
                    observed.set(true);
                }
            });

        let mut returned = None;

        console_output(
            &client,
            "python r = __import__('_fgdb_languages_v1.returns', fromlist=['*']); assert r._prepare() is not None, gdb.newest_frame().name()",
        );

        for _ in 0..64 {
            let output = snapshot(&client);

            if let Some(ReturnSnapshot { value, .. }) = return_values::parse_snapshot(&output) {
                returned = Some(value);
                break;
            }

            stopped.set(false);
            let step = request(&client, "-exec-next-instruction");
            assert_eq!(step.class, "running", "{fixture}: {step:?}");
            wait_until(|| stopped.get());
        }

        let returned = returned.unwrap_or_else(|| panic!("{fixture} did not capture its pair"));
        let reference = returned.history_variable.as_deref().unwrap();
        let typed = request(&client, &format!("-var-create returned * {reference}"));
        let variable = crate::debugger::variable_object(&typed, reference).unwrap();
        assert!(variable.type_name.is_some(), "{fixture}: {typed:?}");
        assert!(variable.num_children > 0, "{fixture}: {typed:?}");

        let children = request(&client, "-var-list-children --all-values returned");
        assert!(children.is_done(), "{fixture}: {children:?}");
        assert!(!crate::debugger::variable_children(&children).is_empty());

        let check = request(
            &client,
            &crate::debugger::console_command(&format!(
                "python v = gdb.history({}); assert int(v['x']) == 7 and int(v['y']) == 11, str(v); assert len(gdb.breakpoints()) == 1",
                &reference[1..],
            )),
        );

        assert!(check.is_done(), "{fixture}: {check:?}");
    }
}

#[test]
#[ignore = "requires GDB and the C/C++ aggregate-return fixtures"]
fn live_aggregate_transfers_cover_register_classes_and_reject_unproven_layouts() {
    for fixture in [
        "c-aggregate-return-target",
        "cpp-aggregate-return-target",
        "rust-return-value-target",
    ] {
        for (checkpoint, expected) in [
            (
                "return_wide",
                "int(v['x']) == 0x1122334455667788 and int(v['y']) == 0x2233445566778899",
            ),
            (
                "return_floats",
                "float(v['x']) == 3.25 and float(v['y']) == -1.5",
            ),
            (
                "return_mixed",
                "int(v['x']) == 42 and float(v['y']) == 3.25",
            ),
            (
                "return_reversed",
                "float(v['x']) == 3.25 and int(v['y']) == 42",
            ),
            (
                "return_nested",
                "int(v['x'][0]) == 7 and int(v['x'][1]) == 11 and float(v['y'][0]) == 3.25 and float(v['y'][1]) == -1.5",
            ),
        ] {
            if fixture == "rust-return-value-target" && checkpoint != "return_floats" {
                continue;
            }

            let checkpoint = if fixture == "rust-return-value-target" {
                "rust_return_value_target::return_floats"
            } else {
                checkpoint
            };

            let stopped = Rc::new(Cell::new(false));
            let observed = Rc::clone(&stopped);

            let (_debugger, client) =
                open_debugger_observing(fixture, checkpoint, false, move |event| {
                    if matches!(event, MiEvent::Stopped { .. }) {
                        observed.set(true);
                    }
                });

            let mut returned = None;

            for _ in 0..64 {
                if let Some(ReturnSnapshot { value, .. }) =
                    return_values::parse_snapshot(&snapshot(&client))
                {
                    returned = Some(value);
                    break;
                }

                stopped.set(false);
                let step = request(&client, "-exec-next-instruction");
                assert_eq!(step.class, "running", "{fixture} {checkpoint}: {step:?}");
                wait_until(|| stopped.get());
            }

            let returned = returned.unwrap_or_else(|| panic!("{fixture} {checkpoint}"));
            let reference = returned.history_variable.unwrap();

            console_output(
                &client,
                &format!(
                    "python v = gdb.history({}); assert {expected}, str(v)",
                    &reference[1..]
                ),
            );
        }
    }

    let (_debugger, client) =
        open_debugger_observing("c-aggregate-return-target", "main", false, |_| {});

    console_output(
        &client,
        &format!(
            "python exec({})",
            crate::debugger::quote(include_str!("../debugger/return_transfers_tests.py"))
        ),
    );

    for (fixture, checkpoint) in [
        ("c-aggregate-return-target", "return_large"),
        ("c-aggregate-return-target", "return_union"),
        ("cpp-aggregate-return-target", "return_nontrivial"),
        (
            "zig-return-value-target",
            "zig_return_value_target.return_native_pair",
        ),
    ] {
        let (_debugger, client) = open_debugger_observing(fixture, checkpoint, false, |_| {});

        console_output(
            &client,
            "python r = __import__('_fgdb_languages_v1.returns', fromlist=['*']); assert r._prepare() is None, gdb.newest_frame().name()",
        );
    }
}

#[test]
#[ignore = "manual GDB-only capture preparation benchmark, requires return-value fixtures"]
fn benchmark_return_capture_preparation() {
    for (fixture, checkpoint) in [
        ("c-return-value-target", "return_integer"),
        ("c-return-value-target", "return_pair"),
        ("c-aggregate-return-target", "return_mixed"),
        (
            "rust-return-value-target",
            "rust_return_value_target::return_pair",
        ),
    ] {
        let (_debugger, client) = open_debugger_observing(fixture, checkpoint, false, |_| {});

        let output = console_output(
            &client,
            "python import timeit, statistics; r = __import__('_fgdb_languages_v1.returns', fromlist=['*']); assert r._prepare() is not None; print(statistics.median(timeit.repeat(r._prepare, repeat=5, number=500)) * 2000)",
        );

        println!(
            "{fixture} {checkpoint} preparation {} microseconds",
            output.trim()
        );
    }
}

#[test]
#[ignore = "requires GDB and c-return-value-target, run separately"]
fn live_return_capture_guards_reject_register_clobbers_and_disabled_capture() {
    let stopped = Rc::new(Cell::new(false));
    let observed = Rc::clone(&stopped);

    let (_debugger, client) =
        open_debugger_observing("c-return-value-target", "main", false, move |event| {
            if matches!(event, MiEvent::Stopped { .. }) {
                observed.set(true);
            }
        });

    let script = r#"r = __import__('_fgdb_languages_v1.returns', fromlist=['*'])
class FakeArchitecture:
 def __init__(self, asm): self.asm = asm
 def disassemble(self, start, end_pc): return [{'addr': start, 'length': 4, 'asm': self.asm}]
class FakeFrame:
 def __init__(self, asm): self.asm = asm
 def architecture(self): return FakeArchitecture(self.asm)
for asm, register in [('mov %eax,-4(%rbp)', 'rax'), ('mov DWORD PTR [rbp-4],eax', 'rax'), ('movq %xmm0,%rax', 'xmm0'), ('str x0, [sp, #8]', 'x0')]:
 assert r._preserved_until(FakeFrame(asm), 100, 104, register), asm
for asm, register in [('mov 8(%rip),%eax # 0x123 <value>', 'rax'), ('mov eax,DWORD PTR [rip+8]', 'rax'), ('movsd %xmm1,%xmm0', 'xmm0'), ('mov v0.16b, v1.16b', 'd0'), ('mov h0, h1', 's0'), ('call 0x123', 'rax'), ('jmp 0x123', 'rax'), ('str x0, [sp], #8', 'x0')]:
 assert not r._preserved_until(FakeFrame(asm), 100, 104, register), asm
assert not r._preserved_until(FakeFrame('nop'), 100, 200, 'rax')
r.snapshot(False)
assert r._candidate is None
assert len(gdb.breakpoints()) == 1
"#;

    let command = crate::debugger::console_command(&format!(
        "python exec({})",
        crate::debugger::quote(script)
    ));

    let record = request(&client, &command);
    assert!(record.is_done(), "{record:?}");
    stopped.set(false);
    assert_eq!(request(&client, "-exec-next-instruction").class, "running");
    wait_until(|| stopped.get());

    let check = request(
        &client,
        &crate::debugger::console_command(
            "python assert __import__('_fgdb_languages_v1.returns', fromlist=['*'])._completed is None",
        ),
    );

    assert!(check.is_done(), "{check:?}");
}

#[test]
#[ignore = "requires GDB and c-return-value-target, run separately"]
fn live_finish_reports_scalar_pointer_and_aggregate_returns_but_not_void() {
    let stops = Rc::new(RefCell::new(Vec::new()));
    let observed = Rc::clone(&stops);

    let (_debugger, client) =
        open_debugger_observing("c-return-value-target", "main", false, move |event| {
            if let MiEvent::Stopped {
                reason,
                return_value,
                ..
            } = event
            {
                observed
                    .borrow_mut()
                    .push((reason.clone(), return_value.clone()));
            }
        });

    assert!(request(&client, "-break-delete 1").is_done());

    for (function, expected) in [
        ("return_integer", Some("42")),
        ("return_float", Some("3.25")),
        ("return_string", Some("\"fgdb return value\"")),
        ("return_pair", Some("{x = 7, y = 11}")),
        ("return_void", None),
    ] {
        let inserted = request(&client, &format!("-break-insert {function}"));
        assert!(inserted.is_done(), "{inserted:?}");
        stops.borrow_mut().clear();
        assert_eq!(request(&client, "-exec-continue").class, "running");
        wait_until(|| !stops.borrow().is_empty());
        assert_eq!(stops.borrow()[0].0.as_deref(), Some("breakpoint-hit"));
        assert!(stops.borrow()[0].1.is_none());
        stops.borrow_mut().clear();

        let command = if function == "return_float" {
            crate::debugger::console_command("finish")
        } else {
            "-exec-finish".into()
        };

        assert!(request(&client, &command).is_success());
        wait_until(|| !stops.borrow().is_empty());
        let stops = stops.borrow();
        let (reason, returned) = &stops[0];
        assert_eq!(reason.as_deref(), Some("function-finished"));

        if let Some(expected) = expected {
            let returned = returned.as_ref().expect("reported return value");

            assert!(
                returned.value.contains(expected),
                "{function}: {returned:?}"
            );

            assert!(
                returned
                    .history_variable
                    .as_deref()
                    .is_some_and(|variable| variable.starts_with('$'))
            );
        } else {
            assert!(returned.is_none());
        }
    }
}
