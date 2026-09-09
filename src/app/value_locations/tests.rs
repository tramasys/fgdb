use super::*;
use crate::app::test_support::{open_debugger, request as mi, wait_until};
use crate::debugger::StopContext;

fn bound(client: &MiClient, valid: &Rc<Cell<bool>>) -> StopRequests {
    let frame = crate::debugger::current_frame(&mi(client, "-stack-info-frame")).unwrap();
    let valid = Rc::clone(valid);
    client.bind_stop_requests(
        StopContext::new(
            client.transport_epoch(),
            1,
            Some("i1".into()),
            "1".into(),
            frame.level,
        )
        .unwrap(),
        move |_| valid.get(),
    )
}

fn variable(client: &MiClient, expression: &str) -> Variable {
    let record = mi(
        client,
        &format!("-var-create - * {}", crate::debugger::quote(expression)),
    );

    crate::debugger::variable_object(&record, expression).unwrap()
}

fn locate(requests: StopRequests, variables: Vec<Variable>) -> Vec<ValueLocation> {
    let result = Rc::new(RefCell::new(None));
    let response = Rc::clone(&result);
    request(
        requests,
        variables,
        Rc::new(|| true),
        Box::new(move |locations| {
            response.replace(Some(locations));
        }),
    );

    wait_until(|| result.borrow().is_some());
    result.take().unwrap().unwrap()
}

fn address(client: &MiClient, expression: &str) -> u64 {
    let record = mi(
        client,
        &format!(
            "-data-evaluate-expression {}",
            crate::debugger::quote(expression)
        ),
    );

    let value = crate::debugger::evaluated_value(&record).unwrap();
    u64::from_str_radix(
        value
            .split_whitespace()
            .next()
            .unwrap()
            .trim_start_matches("0x"),
        16,
    )
    .unwrap()
}

#[test]
#[ignore = "requires GDB and built location/C++ fixtures"]
fn live_storage_is_not_the_pointer_target_and_queries_cannot_mutate_the_target() {
    let (_debugger, client) = open_debugger("c-variable-location-target", "location_checkpoint");
    let valid = Rc::new(Cell::new(true));
    let requests = bound(&client, &valid);
    let expressions = [
        "pointer",
        "*pointer",
        "null_pointer",
        "location_global",
        "record->count",
        "record->flags",
        "1 + 2",
    ];

    let variables: Vec<_> = expressions
        .iter()
        .map(|expression| variable(&client, expression))
        .collect();

    let locations = locate(requests.clone(), variables.clone());
    assert_eq!(locations[0].address(), Some(address(&client, "&pointer")));
    assert_eq!(locations[1].address(), Some(address(&client, "pointer")));
    assert_ne!(locations[0].address(), locations[1].address());
    assert_eq!(
        locations[2].address(),
        Some(address(&client, "&null_pointer"))
    );

    assert_eq!(
        locations[3].address(),
        Some(address(&client, "&location_global"))
    );

    assert_eq!(
        locations[4].address(),
        Some(address(&client, "&record->count"))
    );

    // GDB exposes the bitfield's containing storage unit, although C's &
    // operator cannot take a bitfield address. Do not discard that location.
    assert!(locations[5].address().is_some());
    assert_eq!(locations[6], ValueLocation::NonAddressable);

    // This artificial array extends beyond readable memory and exceeds GDB's
    // value-size budget. Its storage is still known without fetching bytes.
    let large = Variable {
        name: "*(char (*)[16777216])record".into(),
        varobj: None,
        ..variables[0].clone()
    };

    assert_eq!(
        locate(requests.clone(), vec![large])[0].address(),
        Some(address(&client, "record"))
    );

    let fallbacks =
        ["location_probe()", "location_global = 999", "missing_value"].map(|name| Variable {
            name: name.into(),
            varobj: None,
            ..variables[0].clone()
        });

    let rejected = locate(requests.clone(), fallbacks.into());
    assert!(
        rejected
            .iter()
            .all(|value| matches!(value, ValueLocation::Unknown(_)))
    );

    assert_eq!(
        crate::debugger::evaluated_value(&mi(
            &client,
            "-data-evaluate-expression location_probe_calls"
        ))
        .as_deref(),
        Some("0")
    );

    assert_eq!(
        crate::debugger::evaluated_value(&mi(&client, "-data-evaluate-expression location_global"))
            .as_deref(),
        Some("42")
    );

    assert!(mi(&client, &crate::debugger::console_command("python assert all(gdb.parameter(p) for p in ['may-call-functions', 'may-write-memory', 'may-write-registers'])")).is_done());

    let result = Rc::new(RefCell::new(None));
    let response = Rc::clone(&result);
    request(
        requests,
        variables,
        Rc::new(|| true),
        Box::new(move |locations| {
            response.replace(Some(locations));
        }),
    );

    valid.set(false);
    wait_until(|| result.borrow().is_some());
    assert_eq!(result.take().unwrap(), None);

    let (_debugger, client) =
        open_debugger("cpp-variable-viewer-target", "variable_viewer_checkpoint");
    let requests = bound(&client, &Rc::new(Cell::new(true)));
    let locations = locate(
        requests,
        vec![
            variable(&client, "native_values"),
            variable(&client, "words"),
        ],
    );

    assert!(locations.iter().all(|value| matches!(
        value,
        ValueLocation::Memory {
            referenced: true,
            ..
        }
    )));

    assert_eq!(
        locations[0].address(),
        Some(address(&client, "&native_values"))
    );

    assert_eq!(locations[1].address(), Some(address(&client, "&words")));
}

#[test]
#[ignore = "requires GDB and built Rust/Fortran/Zig/Odin fixtures"]
fn live_primary_language_storage_uses_debugger_types() {
    for (fixture, breakpoint, expression, caller) in [
        (
            "rust-array-viewer-target",
            "rust_arrays_ready",
            "large",
            true,
        ),
        (
            "fortran-array-viewer-target",
            "fortran_arrays_ready",
            "matrix",
            true,
        ),
        (
            "zig-variable-viewer-target",
            "zig_variable_viewer_target.zig:20",
            "particle",
            false,
        ),
        (
            "odin-variable-viewer-target",
            "odin_variable_viewer_target.odin:29",
            "particle",
            false,
        ),
    ] {
        let (_debugger, client) = open_debugger(fixture, breakpoint);
        if caller {
            assert!(mi(&client, "-stack-select-frame 1").is_done());
        }

        let requests = bound(&client, &Rc::new(Cell::new(true)));
        let locations = locate(requests, vec![variable(&client, expression)]);
        assert!(locations[0].address().is_some(), "{fixture}: {locations:?}");
    }
}
