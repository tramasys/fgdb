//! Stop-bound, incremental reads for the stack-memory inspector.

use super::*;
use crate::{debugger::MemoryBlock, model::stack::StackPage};

#[cfg(test)]
mod tests;

struct Inputs {
    registers: Vec<Register>,
    frames: Vec<StackFrame>,
    regions: Vec<MemoryRegion>,
    architecture: TargetArchitecture,
    endian: TargetEndian,
    register: &'static str,
    word_size: usize,
}

impl Inputs {
    fn new(
        ui: &Ui,
        registers: Vec<Register>,
        frames: Vec<StackFrame>,
        regions: Vec<MemoryRegion>,
    ) -> Result<Self, &'static str> {
        let endian = ui.target_endian().ok_or(
            "Stack decoding is unavailable because the target byte order could not be determined",
        )?;
        let word_size = match ui.target_pointer_bits() {
            32 => 4,
            64 => 8,
            _ => return Err("Stack decoding requires a supported target pointer width"),
        };
        let architecture = match ui.target_architecture() {
            TargetArchitecture::Unknown => TargetArchitecture::infer_from_register_names_with_bits(
                registers.iter().map(|register| register.name.as_str()),
                Some(ui.target_pointer_bits()),
            ),
            architecture => architecture,
        };
        let register = architecture
            .stack_pointer(registers.iter().map(|register| register.name.as_str()))
            .ok_or("Stack decoding is unavailable because no supported stack-pointer register was identified")?;

        Ok(Self {
            registers,
            frames,
            regions,
            architecture,
            endian,
            register,
            word_size,
        })
    }
}

pub(in crate::app) fn connect_stack_paging(ui: &Rc<Ui>, client: &Rc<MiClient>) {
    let weak = Rc::downgrade(ui);
    let client = client.weak();
    ui.connect_stack_paging(move |automatic| {
        let (Some(ui), Some(client)) = (weak.upgrade(), client.upgrade()) else {
            return;
        };
        let generation = ui.model.current_stop_refresh_generation();
        let Some(requests) = stop_requests(&weak, &client, generation) else {
            return;
        };
        if !requests.is_current() {
            return;
        }

        let Some(page) = ui.model.claim_stack_page(automatic) else {
            return;
        };
        let inputs = Inputs::new(
            &ui,
            ui.model
                .registers_for_details(generation)
                .unwrap_or_default(),
            ui.model.frames_for_details(generation).unwrap_or_default(),
            ui.model
                .memory_regions_for_details(generation)
                .unwrap_or_default(),
        );

        match inputs {
            Ok(inputs) if inputs.word_size == page.word_size => {
                ui.update_stack_paging();
                read_page(weak.clone(), requests, page, Rc::new(inputs), page.words);
            }
            Ok(_) => ui.show_stack_page_error(
                page,
                "The target pointer width changed. Refresh the debugger",
            ),
            Err(reason) => ui.show_stack_page_error(page, reason),
        }
    });
}

pub(in crate::app) fn request_stack_memory(
    weak: Weak<Ui>,
    requests: StopRequests,
    registers: Vec<Register>,
    frames: Vec<StackFrame>,
    regions: Vec<MemoryRegion>,
) {
    if !requests.is_current() {
        return;
    }

    let Some(ui) = weak.upgrade() else {
        return;
    };
    let generation = requests.generation();
    let inputs = match Inputs::new(&ui, registers, frames, regions) {
        Ok(inputs) => inputs,
        Err(reason) => {
            ui.show_stack_unavailable_for_refresh(generation, reason);
            return;
        }
    };
    let Some(base) = inputs
        .registers
        .iter()
        .find(|register| register.name == inputs.register)
        .and_then(|register| pointer_address(&register.value))
        .filter(|base| *base != 0)
    else {
        ui.show_stack_unavailable_for_refresh(generation, "The stack pointer is unavailable");
        return;
    };
    // The containing mapping is authoritative. Without a map, the cursor
    // permits only the initial preview rather than guessing how far to scan.
    let end = match stack_mapping_end(&inputs.regions, base) {
        Ok(end) => end,
        Err(reason) => {
            ui.show_stack_unavailable_for_refresh(generation, reason);
            return;
        }
    };

    if !ui
        .model
        .begin_stack_pages(generation, base, inputs.word_size, end)
    {
        ui.show_stack_unavailable_for_refresh(generation, "The stack-pointer range is invalid");
        return;
    }

    let page = ui.model.claim_stack_page(false);
    ui.show_stack_page_start();
    if let Some(page) = page {
        read_page(weak, requests, page, Rc::new(inputs), page.words);
    }
}

fn stack_mapping_end(regions: &[MemoryRegion], base: u64) -> Result<Option<u64>, &'static str> {
    match memory_region_for_address(regions, base) {
        Some(mapping) if mapping.permissions.starts_with('r') => Ok(Some(mapping.end)),
        Some(_) => Err("The stack-pointer mapping is not readable"),
        None if regions.is_empty() => Ok(None),
        None => Err("No stack-pointer mapping was found in the available memory map"),
    }
}

fn read_page(
    weak: Weak<Ui>,
    requests: StopRequests,
    page: StackPage,
    inputs: Rc<Inputs>,
    words: usize,
) {
    let command = format!(
        "-data-read-memory-bytes 0x{:x} {}",
        page.address,
        words * page.word_size
    );
    let guard = weak.clone();
    let response = weak.clone();
    let next_requests = requests.clone();

    if requests
        .frame(&command)
        .when(move || {
            guard
                .upgrade()
                .is_some_and(|ui| ui.model.stack_page_pending(page))
        })
        .enrich(move |client, record| {
            let Some(ui) = response.upgrade() else {
                return;
            };
            if record.class == "superseded" {
                ui.show_stack_page_error(page, "The stack-memory request was cancelled");
                return;
            }

            // A core file or remote target may omit bytes inside a mapping.
            // Recover a smaller prefix with at most log2(batch) retries, and
            // never skip a hole or extend the request beyond the mapping.
            if record.class == "error" && words > 1 {
                read_page(response, next_requests, page, inputs, words / 2);
                return;
            }

            let range = StackPage { words, ..page }.range();
            let memory = contiguous_prefix(&record, range, page.word_size);
            let memory = match memory {
                Ok(memory) => memory,
                Err(reason) => {
                    ui.show_stack_page_error(page, record.error_message().unwrap_or(reason));
                    return;
                }
            };
            let mut entries = build_stack_entries(
                &memory,
                page.word_size,
                inputs.endian,
                inputs.architecture,
                &inputs.registers,
                &inputs.frames,
                &inputs.regions,
            );

            for entry in &mut entries {
                entry.index += page.index;
                entry.offset += page.index * page.word_size;
            }

            if !ui.show_stack_page(page, &entries) {
                ui.show_stack_page_error(page, "GDB returned an inconsistent stack-memory page");
                return;
            }

            enrich_stack(
                response,
                client,
                next_requests,
                inputs.register,
                page.word_size,
                inputs.endian,
            );
        })
        .is_err()
        && let Some(ui) = weak.upgrade()
    {
        ui.show_stack_page_error(
            page,
            "The MI channel could not issue the stack-memory request",
        );
    }
}

fn contiguous_prefix(
    record: &MiRecord,
    range: std::ops::Range<u64>,
    word_size: usize,
) -> Result<MemoryBlock, &'static str> {
    let blocks = crate::debugger::memory_blocks(record, range.clone())?;
    let mut memory = MemoryBlock {
        begin: range.start,
        bytes: Vec::new(),
    };

    for block in blocks {
        if block.begin != range.start + memory.bytes.len() as u64 {
            break;
        }

        memory.bytes.extend_from_slice(&block.bytes);
    }

    memory
        .bytes
        .truncate(memory.bytes.len() / word_size * word_size);

    if memory.bytes.is_empty() {
        return Err("GDB could not read a complete word at the next stack address");
    }

    Ok(memory)
}
