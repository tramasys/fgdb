use super::*;
use crate::debugger::{
    MiRecord, StopContext, ValueTypeKind, ValueTypeMetadata, location::ValueLocation,
};
use crate::language::{Language, python};
use test_support::{open_debugger, request as mi, wait_until};

fn control(requests: &StopRequests, command: &str) -> MiRecord {
    let result = Rc::new(RefCell::new(None));
    let response = Rc::clone(&result);

    requests
        .frame(command)
        .control(move |_, record| {
            response.replace(Some(record));
        })
        .unwrap();

    wait_until(|| result.borrow().is_some());
    result.take().unwrap()
}

fn metadata(requests: &StopRequests, expression: &str, members: Option<&str>) -> ValueTypeMetadata {
    let script = type_metadata::metadata_python(expression, members);

    let command = crate::debugger::console_command(&format!(
        "python exec({})",
        crate::debugger::quote(&script),
    ));

    let result = Rc::new(RefCell::new(None));
    let response = Rc::clone(&result);

    requests
        .frame(&command)
        .capture(move |_, record, output| {
            assert!(record.is_done(), "{record:?}\n{output}");
            response.replace(Some(type_metadata::parse_metadata_output(&output)));
        })
        .unwrap();

    wait_until(|| result.borrow().is_some());
    result.take().unwrap().expect("valid target type metadata")
}

fn variable(client: &MiClient, expression: &str) -> Variable {
    let record = mi(
        client,
        &format!("-var-create - * {}", crate::debugger::quote(expression)),
    );

    crate::debugger::variable_object(&record, expression).unwrap_or_else(|| panic!("{record:?}"))
}

#[test]
#[ignore = "requires Python-enabled GDB and the GDC, DMD and GNAT variable-viewer fixtures"]
fn live_d_and_ada_mi_edits_locations_metadata_and_stop_guards() {
    for (fixture, checkpoint, language) in [
        ("d-variable-viewer-target", "d_values_ready", Language::D),
        ("dmd-variable-viewer-target", "d_values_ready", Language::D),
        (
            "ada-variable-viewer-target",
            "ada_values_ready",
            Language::Ada,
        ),
    ] {
        let (_debugger, client) = open_debugger(fixture, checkpoint);
        assert!(mi(&client, "-stack-select-frame 1").is_done());
        let frame = crate::debugger::current_frame(&mi(&client, "-stack-info-frame")).unwrap();

        assert_eq!(
            Language::from_path(Path::new(frame.source_path().unwrap())),
            language
        );

        let locals =
            crate::debugger::variables(&mi(&client, "-stack-list-variables --simple-values"));

        let counter = locals
            .iter()
            .find(|variable| variable.name == "counter")
            .unwrap();

        assert!(!counter.needs_variable_object());
        let valid = Rc::new(Cell::new(true));
        let current = Rc::clone(&valid);

        let requests = client.bind_stop_requests(
            StopContext::new(
                client.transport_epoch(),
                1,
                Some("i1".into()),
                "1".into(),
                1,
            )
            .unwrap(),
            move |_| current.get(),
        );

        let array = variable(&client, "values");

        let children = crate::debugger::variable_children(&mi(
            &client,
            &format!(
                "-var-list-children --all-values {} 0 2",
                crate::debugger::quote(array.varobj.as_deref().unwrap()),
            ),
        ));

        assert_eq!(children.len(), 2);
        assert_eq!(children[0].value, "-20");

        let (root, members) =
            value_path::variable_path_root(&children[0], children[0].varobj.as_deref().unwrap());

        let path = crate::debugger::variable_path_expression(&mi(
            &client,
            &format!("-var-info-path-expression {}", crate::debugger::quote(root)),
        ))
        .unwrap();

        assert_eq!(
            metadata(&requests, &path, members).kind,
            ValueTypeKind::Integer
        );

        let pointer = variable(&client, "pointer");
        assert!(pointer.is_pointer());
        let result = Rc::new(RefCell::new(None));
        let response = Rc::clone(&result);

        value_locations::request(
            requests.clone(),
            vec![
                array.clone(),
                children[0].clone(),
                children[1].clone(),
                pointer,
            ],
            Rc::new(|| true),
            Box::new(move |locations| {
                response.replace(Some(locations));
            }),
        );

        wait_until(|| result.borrow().is_some());
        let locations = result.take().unwrap().unwrap();

        let addresses = locations
            .iter()
            .map(ValueLocation::address)
            .collect::<Option<Vec<_>>>()
            .unwrap();

        assert_eq!(addresses[0], addresses[1]);
        assert_eq!(addresses[2] - addresses[1], 4);
        assert_ne!(addresses[0], addresses[3]);

        for (expression, kind, bits) in [
            ("counter", ValueTypeKind::Integer, 64),
            ("enabled", ValueTypeKind::Boolean, 8),
            ("scale", ValueTypeKind::Float, 64),
        ] {
            let metadata = metadata(&requests, expression, None);
            assert_eq!(metadata.kind, kind);
            assert_eq!(metadata.bits, Some(bits));

            assert_eq!(
                metadata.language.as_deref().map(Language::from_gdb),
                Some(language)
            );
        }

        if language == Language::Ada {
            assert_eq!(
                metadata(&requests, "length", None).kind,
                ValueTypeKind::Integer
            );

            assert!(variable(&client, "missing").is_null_pointer());
        }

        if fixture != "dmd-variable-viewer-target" {
            let expression = if language == Language::Ada {
                "state"
            } else {
                "mode"
            };

            let metadata = metadata(&requests, expression, None);
            assert_eq!(metadata.kind, ValueTypeKind::Enum);
            assert_eq!(metadata.enum_variants.len(), 2);
        }

        let before = mi(&client, "-gdb-show language").field("value").cloned();
        assert!(mi(&client, "-stack-select-frame 0").is_done());

        for (expression, value) in [("counter", "43"), ("enabled", "false")] {
            let record = control(&requests, &python::assignment_command(expression, value));
            assert!(record.is_done(), "{fixture}: {record:?}");
        }

        assert_eq!(
            mi(&client, "-gdb-show language").field("value").cloned(),
            before
        );

        assert_eq!(
            crate::debugger::current_frame(&mi(&client, "-stack-info-frame"))
                .unwrap()
                .level,
            0
        );

        assert!(mi(&client, "-stack-select-frame 1").is_done());

        assert_eq!(
            crate::debugger::evaluated_value(&mi(&client, "-data-evaluate-expression counter"))
                .as_deref(),
            Some("43")
        );

        assert_eq!(
            crate::debugger::evaluated_value(&mi(&client, "-data-evaluate-expression enabled"))
                .as_deref(),
            Some("false")
        );

        let child = children[0].varobj.as_deref().unwrap();

        assert!(
            mi(
                &client,
                &format!("-var-assign {} 77", crate::debugger::quote(child))
            )
            .is_done()
        );

        assert_eq!(
            mi(
                &client,
                &format!(
                    "-var-update --all-values {}",
                    array.varobj.as_deref().unwrap()
                )
            )
            .class,
            "done"
        );

        if language == Language::D {
            let slice = variable(&client, "slice");

            let elements = crate::debugger::variable_children(&mi(
                &client,
                &format!(
                    "-var-list-children --all-values {} 0 2",
                    crate::debugger::quote(slice.varobj.as_deref().unwrap()),
                ),
            ));

            assert_eq!(elements.len(), 2);
            assert_eq!(elements[0].value, "-10");
            let element = elements[0].varobj.as_deref().unwrap();

            let edit = mi(
                &client,
                &format!("-var-assign {} 88", crate::debugger::quote(element)),
            );

            assert!(edit.is_done(), "{fixture}: {edit:?}");

            assert_eq!(
                crate::debugger::evaluated_value(&mi(
                    &client,
                    "-data-evaluate-expression slice.ptr[0]"
                ))
                .as_deref(),
                Some("88")
            );
        }

        assert!(mi(&client, "-stack-select-frame 0").is_done());
        assert_eq!(mi(&client, "-exec-finish").class, "running");
        wait_until(|| crate::debugger::current_frame(&mi(&client, "-stack-info-frame")).is_some());
        assert_eq!(mi(&client, "-exec-next").class, "running");
        wait_until(|| crate::debugger::current_frame(&mi(&client, "-stack-info-frame")).is_some());
        let after = crate::debugger::current_frame(&mi(&client, "-stack-info-frame")).unwrap();
        assert_eq!(after.source_path(), frame.source_path());
        assert!(after.line > frame.line, "{fixture}: {frame:?} -> {after:?}");
        valid.set(false);
        let stale = control(&requests, &python::assignment_command("counter", "999"));
        assert_eq!(stale.class, "superseded");

        assert_ne!(
            crate::debugger::evaluated_value(&mi(&client, "-data-evaluate-expression counter"))
                .as_deref(),
            Some("999")
        );
    }
}
