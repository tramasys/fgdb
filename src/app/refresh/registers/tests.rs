use super::*;
use crate::app::test_support::{open_debugger, request, wait_until};
use crate::debugger::StopContext;

#[test]
#[ignore = "requires local GDB and the C, C++ and Rust variable-viewer fixtures"]
fn live_shared_register_reads_match_independent_chains() {
    for (fixture, checkpoint, level) in [
        ("c-variable-location-target", "location_checkpoint", 0),
        (
            "cpp-variable-viewer-target",
            "variable_viewer_checkpoint",
            0,
        ),
        ("rust-variable-viewer-target", "rust_types_ready", 1),
    ] {
        let (_debugger, client) = open_debugger(fixture, checkpoint);
        assert!(request(&client, &format!("-stack-select-frame {level}")).is_done());
        let context =
            StopContext::new(client.transport_epoch(), 1, None, "1".into(), level).unwrap();
        let current = Rc::new(Cell::new(true));
        let authority = Rc::clone(&current);
        let requests = client.bind_stop_requests(context, move |_| authority.get());
        let names = crate::debugger::register_names(&request(&client, "-data-list-register-names"));
        let registers =
            crate::debugger::registers(&request(&client, "-data-list-register-values x"), &names)
                .into_iter()
                .filter(|register| {
                    is_pointer_register(&register.name, TargetArchitecture::X86_64)
                        && pointer_address(&register.value).is_some_and(|address| address != 0)
                })
                .collect::<Vec<_>>();
        assert!(!registers.is_empty());
        let mut expected = registers.clone();
        let mut independent_reads = 0;

        for register in &mut expected {
            for depth in 0..=MAX_POINTER_CHAIN_DEPTH {
                if depth > 0 {
                    independent_reads += 1;
                }

                let value = crate::debugger::evaluated_value(&request(
                    &client,
                    &pointers::register_command(&register.name, depth),
                ));
                let Some((value, address)) =
                    value.and_then(|value| pointer_address(&value).map(|address| (value, address)))
                else {
                    break;
                };

                if register
                    .pointer_chain
                    .iter()
                    .filter_map(|previous| pointer_address(previous))
                    .any(|previous| previous == address)
                {
                    register.pointer_chain.push("[loop detected]".into());
                    break;
                }

                register.pointer_chain.push(value);
                if let Some(storage) = register_string_address(
                    register,
                    address,
                    depth,
                    TargetEndian::Little,
                    64,
                    TargetArchitecture::X86_64,
                ) {
                    let result = Rc::new(RefCell::new(None));
                    let response = Rc::clone(&result);
                    requests
                        .frame(&pointers::string_command(storage))
                        .with_print_limit(POINTER_STRING_PREVIEW_ELEMENTS, move |_, record| {
                            response.replace(Some(crate::debugger::evaluated_value(&record)));
                        })
                        .unwrap();
                    wait_until(|| result.borrow().is_some());
                    if let Some(value) = result.take().unwrap().filter(|value| value.contains('"'))
                    {
                        register.pointer_chain.pop();
                        register.pointer_chain.push(value);
                    }

                    break;
                }

                if address == 0 {
                    break;
                }
            }
        }

        let refresh = Rc::new(RefCell::new(RegisterRefresh {
            ui: Weak::new(),
            requests,
            pending: (0..registers.len()).collect(),
            registers,
            active: 0,
            architecture: TargetArchitecture::X86_64,
            endian: TargetEndian::Little,
            pointer_bits: 64,
            reads: reads::Reads::default(),
        }));
        schedule_register_chains(&client, Rc::clone(&refresh));
        wait_until(|| {
            let state = refresh.borrow();
            state.pending.is_empty() && state.active == 0
        });

        let state = refresh.borrow();
        assert_eq!(state.registers, expected, "{fixture}");
        assert!(state.reads.len() <= independent_reads);
        eprintln!(
            "{fixture}: {} shared memory probes vs {independent_reads} independent",
            state.reads.len()
        );
        drop(state);

        // An old reply cannot patch a new stop, even if a read was shared.
        let original = refresh.borrow().registers.clone();
        request_register_chain(&client, Rc::clone(&refresh), 0, 0);
        current.set(false);
        assert!(request(&client, "-gdb-show language").is_done());
        assert_eq!(refresh.borrow().registers, original);
    }
}
