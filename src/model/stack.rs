//! Bounded stack-memory paging, independent of GTK and the request transport.

use super::*;

#[cfg(test)]
mod tests;

pub(crate) const STACK_PAGE_WORDS: usize = 64;

/// A page claimed from a validated cursor. Its end cannot overflow the target
/// address space, and its index and length fit the table's index representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StackPage {
    pub generation: u64,
    pub index: usize,
    pub address: u64,
    pub words: usize,
    pub word_size: usize,
}

impl StackPage {
    pub(crate) fn range(self) -> std::ops::Range<u64> {
        self.address..self.address + (self.words * self.word_size) as u64
    }
}

pub(super) struct StackPaging {
    generation: u64,
    base: u64,
    word_size: usize,
    words: usize,
    end: Option<u64>,
    stop: StackStop,
    pending: Option<StackPage>,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StackStop {
    MappingEnd,
    UnknownBoundary,
    TableCapacity,
}

impl StackPaging {
    fn new(generation: u64, base: u64, word_size: usize, end: Option<u64>) -> Option<Self> {
        if !matches!(word_size, 4 | 8) || base == 0 || end.is_some_and(|end| end <= base) {
            return None;
        }

        let address_end = if word_size == 4 {
            1_u64 << 32
        } else {
            u64::MAX
        };
        if base >= address_end {
            return None;
        }

        let end = end.map(|end| end.min(address_end));
        let available_words = (end.unwrap_or(address_end) - base) / word_size as u64;
        // GTK indexes rows with guint, and byte offsets must fit usize. This
        // is a representation limit, not a quota on ordinary stack mappings.
        // No rows or memory are allocated for the unvisited part of the range.
        let capacity = (usize::MAX / word_size).min(u32::MAX as usize);
        let (words, stop) = if end.is_none() {
            (
                available_words.min(STACK_PAGE_WORDS as u64) as usize,
                StackStop::UnknownBoundary,
            )
        } else if available_words > capacity as u64 {
            (capacity, StackStop::TableCapacity)
        } else {
            (available_words as usize, StackStop::MappingEnd)
        };

        Some(Self {
            generation,
            base,
            word_size,
            words,
            end,
            stop,
            pending: None,
            error: None,
        })
    }

    fn claim(&mut self, loaded: usize, automatic: bool) -> Option<StackPage> {
        if self.pending.is_some() || loaded >= self.words || (automatic && self.error.is_some()) {
            return None;
        }

        let page = StackPage {
            generation: self.generation,
            index: loaded,
            address: self.base + (loaded * self.word_size) as u64,
            words: STACK_PAGE_WORDS.min(self.words - loaded),
            word_size: self.word_size,
        };
        self.pending = Some(page);
        self.error = None;

        Some(page)
    }
}

#[derive(Default)]
pub(crate) struct StackPageStatus {
    pub loaded: usize,
    pub loading: bool,
    pub refreshing: bool,
    pub can_load: bool,
    pub stop: Option<StackStop>,
    pub total: Option<usize>,
    pub range: Option<std::ops::Range<u64>>,
    pub error: Option<String>,
}

impl DebuggerModel {
    pub(crate) fn begin_stack_pages(
        &self,
        generation: u64,
        base: u64,
        word_size: usize,
        end: Option<u64>,
    ) -> bool {
        if !self.is_stop_refresh_current(generation) {
            return false;
        }

        let Some(paging) = StackPaging::new(generation, base, word_size, end) else {
            return false;
        };

        self.stopped.stack_paging.replace(Some(paging));
        self.stopped.latest_stack.borrow_mut().clear();
        self.stopped.latest_stack_generation.set(Some(generation));
        self.stopped.stack_details_generation.set(Some(generation));
        self.stopped.stack_details_count.set(0);
        self.stopped.stack_details_active.set(false);

        true
    }

    pub(crate) fn claim_stack_page(&self, automatic: bool) -> Option<StackPage> {
        let generation = self.current_stop_refresh_generation();
        if !self.stopped_inspection_available() || !self.is_stop_refresh_current(generation) {
            return None;
        }

        self.stopped
            .stack_paging
            .borrow_mut()
            .as_mut()
            .filter(|paging| paging.generation == generation)?
            .claim(self.stopped.latest_stack.borrow().len(), automatic)
    }

    pub(crate) fn stack_page_pending(&self, page: StackPage) -> bool {
        self.is_stop_refresh_current(page.generation)
            && self
                .stopped
                .stack_paging
                .borrow()
                .as_ref()
                .is_some_and(|paging| paging.pending == Some(page))
    }

    pub(crate) fn fail_stack_page(&self, page: StackPage, reason: &str) -> bool {
        if !self.stack_page_pending(page) {
            return false;
        }

        let mut paging = self.stopped.stack_paging.borrow_mut();
        let paging = paging.as_mut().expect("a pending page has a cursor");
        paging.pending = None;
        paging.error = Some(reason.to_owned());

        true
    }

    pub(crate) fn append_stack_page(&self, page: StackPage, entries: &[StackEntry]) -> bool {
        if !self.stack_page_pending(page) || entries.is_empty() || entries.len() > page.words {
            return false;
        }

        let mut latest = self.stopped.latest_stack.borrow_mut();
        if latest.len() != page.index
            || entries.iter().enumerate().any(|(index, entry)| {
                entry.index != page.index + index
                    || entry.address != page.address + (index * page.word_size) as u64
                    || entry.offset != (page.index + index) * page.word_size
                    || entry.pointer_bits != (page.word_size * 8) as u32
            })
        {
            return false;
        }

        latest.extend_from_slice(entries);
        let mut paging = self.stopped.stack_paging.borrow_mut();
        let paging = paging.as_mut().expect("a pending page has a cursor");
        paging.pending = None;
        // A short prefix is useful, but a hole must never be silently skipped.
        paging.error = (entries.len() < page.words)
            .then(|| String::from("Only part of the requested stack memory was readable"));

        true
    }

    pub(crate) fn stack_page_status(&self) -> StackPageStatus {
        let live = self.execution().ready && self.execution().state.inferior_started();
        let paging = self.stopped.stack_paging.borrow();
        let Some(paging) = paging
            .as_ref()
            .filter(|paging| self.is_stop_refresh_current(paging.generation))
        else {
            return StackPageStatus {
                refreshing: live
                    && !self.stopped.latest_stack.borrow().is_empty()
                    && (!self.stopped_inspection_available()
                        || self.stopped.latest_stack_generation.get()
                            != Some(self.current_stop_refresh_generation())),
                ..StackPageStatus::default()
            };
        };

        let loaded = self.stopped.latest_stack.borrow().len();
        let available = self.stopped_inspection_available();

        StackPageStatus {
            loaded,
            loading: paging.pending.is_some() && available,
            refreshing: live && (paging.pending.is_some() || !available),
            can_load: available && paging.pending.is_none() && loaded < paging.words,
            stop: (loaded == paging.words).then_some(paging.stop),
            total: (paging.stop == StackStop::MappingEnd).then_some(paging.words),
            range: paging.end.map(|end| paging.base..end),
            error: paging.error.clone(),
        }
    }

    pub(crate) fn publish_stack_details(&self, generation: u64, entries: &[StackEntry]) -> bool {
        if !self.is_stop_refresh_current(generation)
            || self.stopped.latest_stack_generation.get() != Some(generation)
        {
            return false;
        }

        let mut latest = self.stopped.latest_stack.borrow_mut();
        if entries.iter().any(|entry| {
            latest.get(entry.index).is_none_or(|current| {
                current.address != entry.address || current.value != entry.value
            })
        }) {
            return false;
        }

        for entry in entries {
            latest[entry.index].clone_from(entry);
        }

        true
    }

    pub(crate) fn complete_stack_details(&self, generation: u64) {
        if self.is_stop_refresh_current(generation) {
            self.stopped.stack_details_active.set(false);
        }
    }
}
