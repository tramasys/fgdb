use super::*;
use crate::debugger::{
    MemoryBlock, TargetArchitecture, TargetEndian, context::build_stack_entries,
};

fn model() -> DebuggerModel {
    let model = DebuggerModel::new(None);
    model.set_controls_ready(true);
    model.apply_debugger_state_delta(DebuggerStateDelta::establish_stopped_target(
        TargetConnection::Local,
    ));
    model.set_debug_state_stale(false);
    model.set_current_thread_id(Some("1"));
    model.start_stop_refresh();
    model.bind_stop_context(1).unwrap();
    model
}

fn entries(page: StackPage) -> Vec<StackEntry> {
    let memory = MemoryBlock {
        begin: page.address,
        bytes: vec![0; page.words * page.word_size],
    };
    let mut entries = build_stack_entries(
        &memory,
        page.word_size,
        TargetEndian::Little,
        TargetArchitecture::X86_64,
        &[],
        &[],
        &[],
    );
    for entry in &mut entries {
        entry.index += page.index;
        entry.offset += page.index * page.word_size;
    }

    entries
}

#[test]
fn pages_are_bounded_to_mapping_and_target_addresses() {
    for word_size in [4, 8] {
        let mut paging = StackPaging::new(
            1,
            0x1000,
            word_size,
            Some(0x1000 + (word_size * 70 + 1) as u64),
        )
        .unwrap();
        let first = paging.claim(0, false).unwrap();
        assert_eq!(first.words, STACK_PAGE_WORDS);
        assert!(paging.claim(0, false).is_none());
        paging.pending = None;
        let last = paging.claim(64, false).unwrap();
        assert_eq!(last.words, 6);
        assert_eq!(last.address, first.range().end);
        paging.pending = None;
        assert!(paging.claim(70, false).is_none());
    }

    assert!(StackPaging::new(1, 0, 8, None).is_none());
    assert!(StackPaging::new(1, 0x1000, 16, None).is_none());
    assert!(StackPaging::new(1, 0x1000, 8, Some(0x1000)).is_none());
    assert!(StackPaging::new(1, 1_u64 << 32, 4, None).is_none());
    assert!(StackPaging::new(1, (1_u64 << 32) + 1, 4, None).is_none());
    let mut narrow = StackPaging::new(1, (1_u64 << 32) - 8, 4, Some(u64::MAX)).unwrap();
    assert_eq!(narrow.claim(0, false).unwrap().range().end, 1_u64 << 32);
    let mut wide = StackPaging::new(1, u64::MAX - 16, 8, None).unwrap();
    assert_eq!(wide.claim(0, false).unwrap().range().end, u64::MAX);
    let mut unknown = StackPaging::new(1, 0x1000, 8, None).unwrap();
    assert_eq!(unknown.words, STACK_PAGE_WORDS);
    assert_eq!(unknown.stop, StackStop::UnknownBoundary);
    assert!(unknown.claim(STACK_PAGE_WORDS, false).is_none());

    let mut large = StackPaging::new(1, 0x1000, 8, Some(u64::MAX)).unwrap();
    assert_eq!(large.stop, StackStop::TableCapacity);
    assert!(large.claim(large.words, false).is_none());
    let last = large.claim(large.words - 1, false).unwrap();
    assert_eq!(last.words, 1);
    assert!(last.index * last.word_size <= usize::MAX - last.word_size);
}

#[test]
fn appending_and_late_details_do_not_replace_other_pages() {
    let model = model();
    let generation = model.current_stop_refresh_generation();
    assert!(model.begin_stack_pages(generation, 0x1000, 8, Some(0x2000)));
    let first = model.claim_stack_page(false).unwrap();
    assert!(model.stack_page_status().loading);
    assert!(model.claim_stack_page(false).is_none());
    assert!(model.append_stack_page(first, &entries(first)));
    assert!(!model.append_stack_page(first, &entries(first)));
    let mut first_details = model.claim_stack_details(generation).unwrap();
    assert!(model.claim_stack_details(generation).is_none());
    let second = model.claim_stack_page(true).unwrap();
    assert_eq!(second.index, STACK_PAGE_WORDS);
    assert!(model.append_stack_page(second, &entries(second)));
    assert!(model.claim_stack_details(generation).is_none());

    first_details[0].pointer_chain.push("0x1234".into());
    first_details[9].pointer_chain.push("0x5678".into());
    assert!(model.publish_stack_details(generation, &first_details[9..10]));
    assert!(
        model.stopped.latest_stack.borrow()[0]
            .pointer_chain
            .is_empty()
    );
    assert_eq!(
        model.stopped.latest_stack.borrow()[9].pointer_chain,
        ["0x5678"]
    );
    assert!(model.claim_stack_details(generation).is_none());
    assert!(model.publish_stack_details(generation, &first_details));
    assert_eq!(model.stack_page_status().loaded, 128);
    assert_eq!(
        model.stopped.latest_stack.borrow()[0].pointer_chain,
        ["0x1234"]
    );
    assert_eq!(model.stopped.latest_stack.borrow()[64].offset, 512);
    model.complete_stack_details(generation);
    let second_details = model.claim_stack_details(generation).unwrap();
    assert_eq!(second_details[0].index, 64);
    model.complete_stack_details(generation);
    assert!(model.claim_stack_details(generation).is_none());
    assert!(!model.stack_details_pending(generation));
}

#[test]
fn short_reads_errors_and_retry_preserve_the_cursor_and_loaded_data() {
    let model = model();
    let generation = model.current_stop_refresh_generation();
    model.begin_stack_pages(generation, 0x1000, 8, Some(0x2000));
    let first = model.claim_stack_page(false).unwrap();
    let entries = entries(first);
    assert!(!model.append_stack_page(first, &[]));
    assert!(model.append_stack_page(first, &entries[..3]));
    assert_eq!(model.stack_page_status().loaded, 3);
    assert!(model.stack_page_status().error.is_some());
    assert!(model.claim_stack_page(true).is_none());
    let retry = model.claim_stack_page(false).unwrap();
    assert_eq!(retry.address, 0x1018);
    assert!(model.fail_stack_page(retry, "unreadable"));
    assert!(!model.fail_stack_page(first, "late failure"));
    assert_eq!(model.stack_page_status().loaded, 3);
    assert!(model.claim_stack_page(true).is_none());
    let retry = model.claim_stack_page(false).unwrap();
    let mut invalid = entries.clone();
    invalid[0].index = 8000;
    assert!(!model.append_stack_page(retry, &invalid));
    assert!(model.stack_page_pending(retry));
}

#[test]
fn stop_selection_execution_and_backend_changes_revoke_page_authority() {
    let model = model();
    for change in 0..5 {
        model.set_controls_ready(true);
        model.set_controls_running(false);
        model.set_debug_state_stale(false);
        model.start_stop_refresh();
        let generation = model.bind_stop_context(2).unwrap().generation();
        model.begin_stack_pages(generation, 0x1000, 8, None);
        let page = model.claim_stack_page(false).unwrap();
        match change {
            0 => {
                model.start_stop_refresh();
            }
            1 => model.set_current_thread_id(Some("2")),
            2 => model.select_frame(1),
            3 => {
                model.set_controls_running(true);
            }
            _ => {
                model.set_controls_ready(false);
            }
        }

        assert!(!model.append_stack_page(page, &entries(page)));
        assert!(!model.publish_stack_details(generation, &entries(page)));
        assert!(model.claim_stack_page(false).is_none());
    }
}

#[test]
fn known_mapping_bounds_replace_the_arbitrary_row_limit_without_preallocation() {
    let model = model();
    let generation = model.current_stop_refresh_generation();
    model.begin_stack_pages(generation, 0x1000, 8, Some(0x1011));
    let page = model.claim_stack_page(false).unwrap();
    assert_eq!(page.words, 2);
    assert!(model.append_stack_page(page, &entries(page)));
    let status = model.stack_page_status();
    assert_eq!(status.stop, Some(StackStop::MappingEnd));
    assert_eq!(status.total, Some(2));
    assert_eq!(status.range, Some(0x1000..0x1011));
    assert!(!status.can_load);

    let total = 8257;
    model.begin_stack_pages(generation, 0x1000, 8, Some(0x1000 + total * 8));
    let previous_capacity = model.stopped.latest_stack.borrow().capacity();
    assert!(previous_capacity < total as usize);

    while let Some(page) = model.claim_stack_page(false) {
        assert!(page.words <= STACK_PAGE_WORDS);
        assert!(page.range().end <= 0x1000 + total * 8);
        assert!(model.append_stack_page(page, &entries(page)));
    }

    let status = model.stack_page_status();
    assert_eq!(status.loaded, total as usize);
    assert_eq!(status.total, Some(total as usize));
    assert_eq!(status.stop, Some(StackStop::MappingEnd));
    assert!(!status.can_load);
    model.publish_stack(None, &[]);
    assert!(!model.stack_page_status().can_load);
    assert!(model.claim_stack_page(false).is_none());
}

#[test]
fn missing_mapping_only_allows_the_initial_preview() {
    let model = model();
    let generation = model.current_stop_refresh_generation();
    model.begin_stack_pages(generation, 0x1000, 8, None);
    let first = model.claim_stack_page(false).unwrap();
    assert!(model.append_stack_page(first, &entries(first)));
    assert!(model.claim_stack_page(true).is_none());
    assert!(model.claim_stack_page(false).is_none());
    let status = model.stack_page_status();
    assert_eq!(status.stop, Some(StackStop::UnknownBoundary));
    assert_eq!(status.total, None);
    assert_eq!(status.range, None);
}

#[test]
fn refresh_status_distinguishes_transient_stops_from_backend_unavailability() {
    let model = model();
    let generation = model.current_stop_refresh_generation();
    model.begin_stack_pages(generation, 0x1000, 8, Some(0x2000));
    let page = model.claim_stack_page(false).unwrap();
    assert!(model.stack_page_status().refreshing);
    assert!(model.append_stack_page(page, &entries(page)));
    assert!(!model.stack_page_status().refreshing);
    assert!(model.stack_page_status().can_load);
    model.start_stop_refresh();
    assert!(model.stack_page_status().refreshing);
    assert!(!model.stack_page_status().can_load);
    model.set_controls_ready(false);
    assert!(!model.stack_page_status().refreshing);
    assert!(!model.stack_page_status().can_load);
}
