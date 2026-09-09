use super::*;
use crate::debugger::parse_record;

#[test]
fn stack_extent_is_the_containing_readable_mapping_not_a_pointer_target_or_next_mapping() {
    let regions = [
        (0x1000, 0x2000, "---p"),
        (0x2000, 0x4000, "rw-p"),
        (0x4000, 0x5000, "r--p"),
    ]
    .map(|(start, end, permissions)| MemoryRegion {
        start,
        end,
        permissions: permissions.into(),
        // Worker-thread stacks are often anonymous, without a [stack] name.
        path: None,
        kind: MemoryKind::Writable,
        referenced_by: Vec::new(),
    });

    assert_eq!(stack_mapping_end(&regions, 0x2200), Ok(Some(0x4000)));
    assert!(stack_mapping_end(&regions, 0x1ff0).is_err());
    assert!(stack_mapping_end(&regions, 0x5000).is_err());
    assert_eq!(stack_mapping_end(&[], 0x2200), Ok(None));
}

#[test]
fn memory_pages_keep_only_contiguous_complete_words() {
    let adjacent = parse_record(r#"1^done,memory=[{begin="0x1004",end="0x100a",offset="0x4",contents="05060708090a"},{begin="0x1000",end="0x1004",offset="0x0",contents="01020304"}]"#).unwrap();
    let prefix = contiguous_prefix(&adjacent, 0x1000..0x1010, 8).unwrap();
    assert_eq!(prefix.bytes, [1, 2, 3, 4, 5, 6, 7, 8]);
    let hole = parse_record(r#"1^done,memory=[{begin="0x1000",end="0x1004",offset="0x0",contents="01020304"},{begin="0x1008",end="0x100c",offset="0x8",contents="05060708"}]"#).unwrap();
    assert_eq!(
        contiguous_prefix(&hole, 0x1000..0x1010, 4).unwrap().bytes,
        [1, 2, 3, 4]
    );
    assert!(contiguous_prefix(&hole, 0x1000..0x1010, 8).is_err());

    for response in [
        r#"1^done,memory=[]"#,
        r#"1^error,msg="Cannot access memory""#,
        r#"1^done,memory=[{begin="0x1008",end="0x100c",offset="0x8",contents="05060708"}]"#,
        r#"1^done,memory=[{begin="0x1000",end="0x1004",offset="0x1",contents="01020304"}]"#,
        r#"1^done,memory=[{begin="0x1000",end="0x1020",offset="0x0",contents="01020304"}]"#,
    ] {
        assert!(
            contiguous_prefix(&parse_record(response).unwrap(), 0x1000..0x1010, 4).is_err(),
            "{response}"
        );
    }
}

#[test]
#[ignore = "requires GDB and the built C memory-search fixture"]
fn live_stack_pages_match_a_contiguous_read_and_are_stop_bound() {
    use crate::app::test_support::{open_debugger, request, wait_until};
    use crate::debugger::StopContext;

    let (_debugger, client) = open_debugger("c-memory-search-target", "search_checkpoint");
    let base = pointer_address(
        &crate::debugger::evaluated_value(&request(&client, "-data-evaluate-expression $sp"))
            .unwrap(),
    )
    .unwrap();
    let bytes = |address, size| {
        let record = request(
            &client,
            &format!("-data-read-memory-bytes --thread 1 --frame 0 0x{address:x} {size}"),
        );
        contiguous_prefix(&record, address..address + size, 8)
            .unwrap()
            .bytes
    };
    let mut paged = bytes(base, 512);
    paged.extend_from_slice(&bytes(base + 512, 512));
    assert_eq!(paged, bytes(base, 1024));
    let frame = crate::debugger::current_frame(&request(&client, "-stack-info-frame")).unwrap();
    let current = Rc::new(Cell::new(true));
    let authority = Rc::clone(&current);
    let requests = client.bind_stop_requests(
        StopContext::new(
            client.transport_epoch(),
            1,
            Some("i1".into()),
            "1".into(),
            frame.level,
        )
        .unwrap(),
        move |_| authority.get(),
    );
    let response = Rc::new(RefCell::new(None));
    let result = Rc::clone(&response);
    requests
        .frame(&format!("-data-read-memory-bytes 0x{base:x} 512"))
        .request(move |_, record| {
            result.replace(Some(record));
        })
        .unwrap();
    current.set(false);
    wait_until(|| response.borrow().is_some());
    assert_eq!(response.borrow().as_ref().unwrap().class, "superseded");
}

#[test]
#[ignore = "requires local GDB, procfs and the built C memory-search fixture"]
fn live_stack_reads_stop_at_the_containing_mapping_end() {
    use crate::app::test_support::{open_debugger, request};
    use crate::model::{DebuggerModel, DebuggerStateDelta, TargetConnection};

    let (debugger, client) = open_debugger("c-memory-search-target", "search_checkpoint");
    let pid = crate::debugger::inferior_pid(&request(&client, "-list-thread-groups")).unwrap();
    let regions = read_memory_regions(pid, debugger.pid());
    let base = pointer_address(
        &crate::debugger::evaluated_value(&request(&client, "-data-evaluate-expression $sp"))
            .unwrap(),
    )
    .unwrap();
    let end = stack_mapping_end(&regions, base).unwrap().unwrap();
    let model = DebuggerModel::new(None);
    model.set_controls_ready(true);
    model.apply_debugger_state_delta(DebuggerStateDelta::establish_stopped_target(
        TargetConnection::Local,
    ));
    model.set_debug_state_stale(false);
    model.set_current_thread_id(Some("1"));
    let generation = model.start_stop_refresh();
    model.bind_stop_context(client.transport_epoch()).unwrap();
    assert!(model.begin_stack_pages(generation, base, 8, Some(end)));
    let mut pages = 0;

    while let Some(page) = model.claim_stack_page(false) {
        pages += 1;
        assert!(
            pages < 512,
            "Unexpectedly large stack extent in the small fixture"
        );
        assert!(page.range().end <= end);
        let record = request(
            &client,
            &format!(
                "-data-read-memory-bytes 0x{:x} {}",
                page.address,
                page.words * page.word_size
            ),
        );
        let memory = contiguous_prefix(&record, page.range(), page.word_size).unwrap();
        let mut entries = build_stack_entries(
            &memory,
            page.word_size,
            TargetEndian::Little,
            TargetArchitecture::X86_64,
            &[],
            &[],
            &regions,
        );

        for entry in &mut entries {
            entry.index += page.index;
            entry.offset += page.index * page.word_size;
        }

        assert_eq!(entries.len(), page.words);
        assert!(model.append_stack_page(page, &entries));
    }

    assert!(pages > 1);
    let status = model.stack_page_status();
    assert_eq!(status.loaded as u64, (end - base) / 8);
    assert_eq!(status.range, Some(base..end));
    assert_eq!(
        status.stop,
        Some(crate::model::stack::StackStop::MappingEnd)
    );
    assert!(!status.can_load);
}
