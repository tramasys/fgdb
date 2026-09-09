use super::*;
use crate::app::test_support::{open_debugger, request, wait_until};
use crate::debugger::StopContext;

fn preview(requests: &StopRequests, address: u64) -> Option<String> {
    let result = Rc::new(RefCell::new(None));
    let response = Rc::clone(&result);
    requests
        .frame(&pointers::string_command(address))
        .with_print_limit(POINTER_STRING_PREVIEW_ELEMENTS, move |_, record| {
            response.replace(Some(crate::debugger::evaluated_value(&record)));
        })
        .unwrap();

    wait_until(|| result.borrow().is_some());
    result.take().unwrap()
}

#[test]
#[ignore = "requires local GDB and the C, C++ and Rust variable-viewer fixtures"]
fn live_shared_stack_reads_match_independent_paths_and_do_not_cross_stops() {
    for (fixture, checkpoint, level) in [
        ("c-variable-location-target", "location_checkpoint", 0),
        (
            "cpp-variable-viewer-target",
            "variable_viewer_checkpoint",
            0,
        ),
        ("rust-variable-viewer-target", "rust_types_ready", 1),
    ] {
        let (debugger, client) = open_debugger(fixture, checkpoint);
        assert!(request(&client, &format!("-stack-select-frame {level}")).is_done());
        let context =
            StopContext::new(client.transport_epoch(), 1, None, "1".into(), level).unwrap();

        let current = Rc::new(Cell::new(true));
        let authority = Rc::clone(&current);
        let requests = client.bind_stop_requests(context, move |_| authority.get());
        let pid = crate::debugger::inferior_pid(&request(&client, "-list-thread-groups")).unwrap();
        let regions = read_memory_regions(pid, debugger.pid());
        let memory =
            crate::debugger::memory_block(&request(&client, "-data-read-memory-bytes $sp 512"))
                .unwrap();

        let entries = build_stack_entries(
            &memory,
            8,
            TargetEndian::Little,
            TargetArchitecture::X86_64,
            &[],
            &[],
            &regions,
        );

        let indices = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry.region.is_some()
                    && pointer_address(&entry.value).is_some_and(|value| value != 0)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();

        assert!(!indices.is_empty());
        let mut expected = entries.clone();
        let mut independent_reads = 0;

        // Independent reference: the previous implementation walks each path
        // from SP, including nulls, loops, unreadable hops and string previews.
        for &index in &indices {
            let entry = &mut expected[index];
            for depth in 0..=MAX_POINTER_CHAIN_DEPTH {
                independent_reads += 1;
                let value = crate::debugger::evaluated_value(&request(
                    &client,
                    &pointers::stack_command("sp", entry.offset, depth),
                ));

                let Some((value, address)) =
                    value.and_then(|value| pointer_address(&value).map(|address| (value, address)))
                else {
                    break;
                };

                if entry
                    .pointer_chain
                    .iter()
                    .filter_map(|value| pointer_address(value))
                    .any(|previous| previous == address)
                {
                    entry.pointer_chain.push("[loop detected]".into());
                    break;
                }

                entry.pointer_chain.push(value);
                if let Some(address) =
                    stack_string_address(entry, address, depth, TargetEndian::Little, 8)
                {
                    if let Some(value) =
                        preview(&requests, address).filter(|value| value.contains('"'))
                    {
                        entry.pointer_chain.pop();
                        entry.pointer_chain.push(value);
                        entry.memory_kind = MemoryKind::String;
                    }

                    break;
                }

                if address == 0 {
                    break;
                }
            }

            if entry
                .pointer_chain
                .iter()
                .skip(1)
                .filter_map(|value| pointer_address(value))
                .any(|value| looks_like_string_word(value, TargetEndian::Little, 8))
            {
                entry.memory_kind = MemoryKind::String;
            }
        }

        let state = || {
            Rc::new(RefCell::new(StackRefresh {
                ui: Weak::new(),
                requests: requests.clone(),
                entries: entries.clone(),
                stack_register: "sp",
                pending: indices.iter().copied().collect(),
                active: 0,
                word_size: 8,
                endian: TargetEndian::Little,
                progress: progress::Progress::default(),
                reads: reads::Reads::default(),
            }))
        };

        let refresh = state();
        schedule_stack_chains(&client, Rc::clone(&refresh));
        wait_until(|| {
            let state = refresh.borrow();
            state.active == 0 && state.pending.is_empty()
        });

        assert_eq!(refresh.borrow().entries, expected, "{fixture}");
        assert!(refresh.borrow().reads.len() <= independent_reads);
        eprintln!(
            "{fixture}: {} shared reads vs {independent_reads} independent reads",
            refresh.borrow().reads.len()
        );

        let stale = state();
        schedule_stack_chains(&client, Rc::clone(&stale));
        current.set(false);
        // Drain the already-submitted responses before examining the state.
        request(&client, "-stack-info-frame");
        assert_eq!(stale.borrow().entries, entries);
    }
}
