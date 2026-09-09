use super::*;
use crate::debugger::StopContext;

#[test]
fn pointer_commands_keep_existing_depths_and_pin_their_syntax() {
    for (depth, expression) in [
        "(void*)($rsp)",
        "*(void**)($rsp)",
        "*(void**)(*(void**)($rsp))",
        "*(void**)(*(void**)(*(void**)($rsp)))",
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            register_command("rsp", depth),
            format!("-data-evaluate-expression --language c \"{expression}\"")
        );
    }

    assert_eq!(
        stack_command("rsp", 8, 0),
        "-data-evaluate-expression --language c \"*(void**)($rsp+0x8)\""
    );

    assert_eq!(
        stack_command("rsp", 8, 1),
        "-data-evaluate-expression --language c \"*(void**)(*(void**)($rsp+0x8))\""
    );

    assert_eq!(
        stack_command("sp", usize::MAX, 0),
        format!(
            "-data-evaluate-expression --language c \"*(void**)($sp+0x{:x})\"",
            usize::MAX
        )
    );

    for address in [0, 0x1000, u64::MAX] {
        assert_eq!(
            read_command(address),
            format!("-data-evaluate-expression --language c \"*(void**)0x{address:x}\"")
        );

        assert_eq!(
            string_command(address),
            format!("-data-evaluate-expression --language c \"(char*)0x{address:x}\"")
        );
    }
}

#[test]
fn pointer_commands_retain_frame_scope_and_quote_data() {
    let context = StopContext::new(1, 2, None, "3".into(), 4).unwrap();

    for command in [
        register_command("sp", 1),
        stack_command("sp", 8, 1),
        string_command(0x1000),
    ] {
        assert_eq!(
            context.scope_frame(&command),
            command.replacen(
                "-data-evaluate-expression",
                "-data-evaluate-expression --thread 3 --frame 4",
                1,
            )
        );
    }

    assert_eq!(
        register_command("name\"\\suffix", 0),
        "-data-evaluate-expression --language c \"(void*)($name\\\"\\\\suffix)\""
    );
}

#[test]
#[ignore = "requires Python-enabled GDB and the C, C++, Rust, Fortran, Zig and Odin fixtures"]
fn live_pointer_probes_preserve_results_language_and_print_settings() {
    use crate::{
        app::test_support::{open_debugger, request, wait_until},
        debugger::{MiRecord, StopRequests, context::pointer_address, evaluated_value},
    };
    use std::{cell::RefCell, rc::Rc};

    fn inspect(requests: &StopRequests, command: &str, preview: bool) -> MiRecord {
        let result = Rc::new(RefCell::new(None));
        let response = Rc::clone(&result);
        let completed = move |_: &crate::debugger::MiClient, record| {
            response.replace(Some(record));
        };

        let scoped = requests.frame(command);

        if preview {
            scoped
                .with_print_limit(super::super::POINTER_STRING_PREVIEW_ELEMENTS, completed)
                .unwrap();
        } else {
            scoped.request(completed).unwrap();
        }

        wait_until(|| result.borrow().is_some());
        result.take().unwrap()
    }

    fn outcome(record: MiRecord) -> Result<String, String> {
        if record.is_done() {
            Ok(evaluated_value(&record).expect("pointer probe returned no value"))
        } else {
            assert_eq!(record.class, "error", "{record:?}");
            Err(record.error_message().unwrap().to_owned())
        }
    }

    for (fixture, checkpoint) in [
        ("c-variable-location-target", "location_checkpoint"),
        ("cpp-variable-viewer-target", "variable_viewer_checkpoint"),
        ("rust-variable-viewer-target", "rust_types_ready"),
        (
            "fortran-variable-viewer-target",
            "fortran_variable_viewer_target",
        ),
        (
            "zig-variable-viewer-target",
            "zig_variable_viewer_target.main",
        ),
        ("odin-variable-viewer-target", "main::main"),
    ] {
        let (_debugger, client) = open_debugger(fixture, checkpoint);
        let requests = client.bind_stop_requests(
            StopContext::new(
                client.transport_epoch(),
                1,
                Some("i1".into()),
                "1".into(),
                0,
            )
            .unwrap(),
            |_| true,
        );

        let sp = pointer_address(
            &outcome(inspect(&requests, &register_command("sp", 0), false)).unwrap(),
        )
        .unwrap();

        let pc = evaluated_value(&request(&client, "-data-evaluate-expression $pc")).unwrap();
        let word_size = evaluated_value(&request(&client, &evaluate("sizeof(void*)")))
            .unwrap()
            .parse::<usize>()
            .unwrap();

        let mut probes = Vec::new();

        for depth in 0..=super::super::MAX_POINTER_CHAIN_DEPTH {
            probes.push((register_command("sp", depth), false));
            probes.push((stack_command("sp", 0, depth), false));
        }

        probes.extend([
            (stack_command("sp", word_size, 0), false),
            (register_command("pc", 0), false),
            (string_command(sp), true),
            (string_command(0), true),
            (string_command(1), true),
            (register_command("fgdb_missing_register", 1), false),
            (read_command(sp), false),
        ]);

        if fixture.starts_with("rust-") {
            // Prove the failure in an actual Rust frame, not just a manually
            // selected parser. Generated C casts must not inherit this mode.
            let previous = register_command("sp", 0).replace(" --language c", "");
            assert!(outcome(inspect(&requests, &previous, false)).is_err());
        }

        assert!(request(&client, "-gdb-set language c").is_done());
        assert!(request(&client, "-gdb-set print elements 17").is_done());
        let reference: Vec<_> = probes
            .iter()
            .map(|(command, preview)| {
                // Compare against the original commands in their intended
                // language. This includes terminal read errors and symbols.
                let previous = command.replace(" --language c", "");
                outcome(inspect(&requests, &previous, *preview))
            })
            .collect();

        let expected = |command: &str| {
            probes
                .iter()
                .zip(&reference)
                .find_map(|((candidate, _), value)| (candidate == command).then_some(value))
                .unwrap()
        };

        assert!(expected(&register_command("sp", 0)).is_ok());
        assert!(expected(&stack_command("sp", 0, 0)).is_ok());
        assert_eq!(
            expected(&stack_command("sp", 0, 0)),
            expected(&register_command("sp", 1)),
            "stack word and register dereference differ"
        );
        assert_eq!(
            expected(&read_command(sp)),
            expected(&stack_command("sp", 0, 0))
        );

        assert!(
            expected(&string_command(sp))
                .as_ref()
                .unwrap()
                .contains('"')
        );

        assert!(expected(&register_command("fgdb_missing_register", 1)).is_err());

        for language in ["auto", "c", "c++", "rust", "fortran", "ada", "pascal"] {
            assert!(request(&client, &format!("-gdb-set language {language}")).is_done());
            let before = evaluated_value(&request(&client, "-gdb-show language")).unwrap();

            for ((command, preview), expected) in probes.iter().zip(&reference) {
                assert_eq!(
                    &outcome(inspect(&requests, command, *preview)),
                    expected,
                    "{fixture} / {language}: {command}"
                );

                assert_eq!(
                    evaluated_value(&request(&client, "-gdb-show language")).as_deref(),
                    Some(before.as_str())
                );

                assert_eq!(
                    evaluated_value(&request(&client, "-gdb-show print elements")).as_deref(),
                    Some("17")
                );

                if matches!(language, "auto" | "c" | "c++") {
                    let previous = command.replace(" --language c", "");
                    let previous = outcome(inspect(&requests, &previous, *preview));

                    if language != "auto" || previous.is_ok() {
                        assert_eq!(
                            &previous, expected,
                            "changed existing result in {fixture} / {language}: {command}"
                        );
                    }
                }

                // Ordinary evaluation must still use the selected language,
                // even after a probe failed or used a temporary print limit.
                let native = request(&client, "-data-evaluate-expression \"1 as usize\"");

                if language == "rust" {
                    assert_eq!(evaluated_value(&native).as_deref(), Some("1"));
                } else if matches!(language, "c" | "c++") {
                    assert_eq!(native.class, "error");
                }
            }
        }

        assert!(request(&client, "-gdb-set language auto").is_done());
        assert_eq!(
            evaluated_value(&request(&client, "-data-evaluate-expression $pc")),
            Some(pc)
        );
    }
}
