use super::*;

mod pages;
mod progress;
use pointers::reads;

#[cfg(test)]
mod tests;

pub(in crate::app) use pages::{connect_stack_paging, request_stack_memory};

pub(in crate::app) struct StackRefresh {
    ui: Weak<Ui>,
    requests: StopRequests,
    entries: Vec<StackEntry>,
    stack_register: &'static str,
    pending: VecDeque<usize>,
    active: usize,
    word_size: usize,
    endian: TargetEndian,
    progress: progress::Progress,
    reads: reads::Reads,
}

pub(in crate::app) fn enrich_stack(
    ui: Weak<Ui>,
    client: &MiClient,
    requests: StopRequests,
    stack_register: &'static str,
    word_size: usize,
    endian: TargetEndian,
) {
    let generation = requests.generation();
    if !requests.is_current() {
        return;
    }

    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    if !current_ui.stack_details_visible() || !current_ui.model.stack_details_pending(generation) {
        return;
    }

    // Only one bounded enrichment batch is active, even when scrolling queues
    // another memory page. Already attempted words are never re-requested.
    let (entries, indices) = loop {
        let Some(entries) = current_ui.model.claim_stack_details(generation) else {
            return;
        };
        let indices = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                pointer_address(&entry.value).is_some_and(|value| value != 0)
                    && (entry.region.is_some()
                        || !entry.value_registers.is_empty()
                        || entry.return_frame.is_some())
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if !indices.is_empty() {
            break (entries, indices);
        }

        current_ui.show_stack_details(generation, &entries);
        current_ui.model.complete_stack_details(generation);
    };

    drop(current_ui);
    let refresh = Rc::new(RefCell::new(StackRefresh {
        ui,
        requests,
        entries,
        stack_register,
        pending: indices.into(),
        active: 0,
        word_size,
        endian,
        progress: progress::Progress::default(),
        reads: reads::Reads::default(),
    }));
    schedule_stack_chains(client, refresh);
}

fn schedule_stack_chains(client: &MiClient, refresh: Rc<RefCell<StackRefresh>>) {
    loop {
        let next = {
            let mut state = refresh.borrow_mut();
            if state.active >= POINTER_ENRICHMENT_CONCURRENCY {
                None
            } else {
                let next = state.pending.pop_front();
                if next.is_some() {
                    state.active += 1;
                }

                next
            }
        };
        let Some(index) = next else {
            return;
        };
        request_stack_chain(client, Rc::clone(&refresh), index, 0);
    }
}

pub(in crate::app) fn request_stack_chain(
    client: &MiClient,
    refresh: Rc<RefCell<StackRefresh>>,
    entry_index: usize,
    depth: usize,
) {
    if !refresh.borrow().requests.is_current() {
        return;
    }

    let (requests, address, cached) = {
        let mut state = refresh.borrow_mut();
        let entry = &state.entries[entry_index];
        let address = if depth == 0 {
            entry.address
        } else {
            // The preceding hop is already resolved. Re-evaluating the path
            // from SP would reread every earlier word at each depth.
            entry
                .pointer_chain
                .last()
                .and_then(|value| pointer_address(value))
                .expect("a continued chain has a decoded address")
        };
        let cached = state.reads.request(
            address,
            reads::Position {
                index: entry_index,
                depth,
            },
        );

        (state.requests.clone(), address, cached)
    };

    match cached {
        reads::Lookup::Pending => return,
        reads::Lookup::Ready(value) => {
            apply_stack_chain_value(client, refresh, entry_index, depth, value);
            return;
        }
        reads::Lookup::Start => {}
    }

    let command = pointers::read_command(address);

    let refresh_for_handler = Rc::clone(&refresh);
    if requests
        .frame(&command)
        .enrich(move |client, record| {
            if record.class == "superseded" {
                return;
            }

            let value = record
                .is_done()
                .then(|| crate::debugger::evaluated_value(&record))
                .flatten();
            complete_stack_read(client, &refresh_for_handler, address, value);
        })
        .is_err()
    {
        complete_stack_read(client, &refresh, address, None);
    }
}

fn complete_stack_read(
    client: &MiClient,
    refresh: &Rc<RefCell<StackRefresh>>,
    address: u64,
    value: Option<String>,
) {
    let waiters = refresh.borrow_mut().reads.complete(address, value.clone());

    for position in waiters {
        apply_stack_chain_value(
            client,
            Rc::clone(refresh),
            position.index,
            position.depth,
            value.clone(),
        );
    }
}

fn apply_stack_chain_value(
    client: &MiClient,
    refresh: Rc<RefCell<StackRefresh>>,
    entry_index: usize,
    depth: usize,
    value: Option<String>,
) {
    if !refresh.borrow().requests.is_current() {
        return;
    }

    let mut continue_chain = false;
    let mut string_address = None;
    if let Some(value) = value
        && let Some(address) = pointer_address(&value)
    {
        let mut state = refresh.borrow_mut();
        let endian = state.endian;
        let word_size = state.word_size;
        let entry = &mut state.entries[entry_index];
        let chain = &mut entry.pointer_chain;
        if chain
            .iter()
            .filter_map(|previous| pointer_address(previous))
            .any(|previous| previous == address)
        {
            chain.push(String::from("[loop detected]"));
        } else {
            chain.push(value);
            string_address = stack_string_address(entry, address, depth, endian, word_size);
            continue_chain =
                string_address.is_none() && address != 0 && depth < MAX_POINTER_CHAIN_DEPTH;
        }
    }

    if let Some(address) = string_address {
        request_stack_string(client, refresh, entry_index, address);
    } else if continue_chain {
        request_stack_chain(client, refresh, entry_index, depth + 1);
    } else {
        complete_stack_sequence(client, &refresh, entry_index);
    }
}

pub(in crate::app) fn stack_string_address(
    entry: &StackEntry,
    decoded_word: u64,
    depth: usize,
    endian: TargetEndian,
    word_size: usize,
) -> Option<u64> {
    if depth == 0
        || !looks_like_string_word(decoded_word, endian, word_size)
        || matches!(entry.memory_kind, MemoryKind::Code | MemoryKind::Rwx)
    {
        return None;
    }

    entry
        .pointer_chain
        .len()
        .checked_sub(2)
        .and_then(|index| entry.pointer_chain.get(index))
        .and_then(|value| pointer_address(value))
}

pub(in crate::app) fn request_stack_string(
    client: &MiClient,
    refresh: Rc<RefCell<StackRefresh>>,
    entry_index: usize,
    address: u64,
) {
    if !refresh.borrow().requests.is_current() {
        return;
    }

    let command = pointers::string_command(address);
    let requests = refresh.borrow().requests.clone();
    let refresh_for_handler = Rc::clone(&refresh);

    if requests
        .frame(&command)
        .with_print_limit(POINTER_STRING_PREVIEW_ELEMENTS, move |client, record| {
            if record.class == "superseded" {
                return;
            }

            if let Some(value) = record
                .is_done()
                .then(|| crate::debugger::evaluated_value(&record))
                .flatten()
                .filter(|value| value.contains('"'))
            {
                let mut state = refresh_for_handler.borrow_mut();
                let entry = &mut state.entries[entry_index];
                entry.pointer_chain.pop();
                entry.pointer_chain.push(value);
                entry.memory_kind = MemoryKind::String;
            }

            complete_stack_sequence(client, &refresh_for_handler, entry_index);
        })
        .is_err()
    {
        complete_stack_sequence(client, &refresh, entry_index);
    }
}

pub(in crate::app) fn complete_stack_sequence(
    client: &MiClient,
    refresh: &Rc<RefCell<StackRefresh>>,
    entry_index: usize,
) {
    let completed = {
        let mut state = refresh.borrow_mut();
        if !state.requests.is_current() {
            return;
        }

        let endian = state.endian;
        let word_size = state.word_size;
        let entry = &mut state.entries[entry_index];

        if entry
            .pointer_chain
            .iter()
            .skip(1)
            .filter_map(|value| pointer_address(value))
            .any(|value| looks_like_string_word(value, endian, word_size))
        {
            entry.memory_kind = MemoryKind::String;
        }

        state.progress.completed(entry_index);
        state.active = state.active.saturating_sub(1);

        if state.active == 0 && state.pending.is_empty() {
            let ui = state.ui.clone();
            state.progress.finish();
            // No presentation work or subsequent pages survive a closed UI.
            if ui.upgrade().is_none() {
                return;
            }
            Some((
                ui,
                state.requests.clone(),
                state.stack_register,
                word_size,
                endian,
                std::mem::take(&mut state.entries),
            ))
        } else {
            None
        }
    };

    if let Some((ui, requests, register, word_size, endian, entries)) = completed {
        if let Some(current_ui) = ui.upgrade() {
            current_ui.show_stack_details(requests.generation(), &entries);
            current_ui
                .model
                .complete_stack_details(requests.generation());
        }

        enrich_stack(ui, client, requests, register, word_size, endian);
    } else {
        schedule_stack_chains(client, Rc::clone(refresh));
        progress::schedule(refresh);
    }
}
