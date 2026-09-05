use super::*;

impl DebuggerModel {
    pub(crate) fn stop_context(&self, generation: u64) -> Option<crate::debugger::StopContext> {
        self.stopped
            .active_stop_context
            .borrow()
            .as_ref()
            .filter(|context| context.generation() == generation)
            .cloned()
    }

    pub(crate) fn is_stop_refresh_current(&self, generation: u64) -> bool {
        if self.stopped.stop_refresh_generation.get() != generation {
            return false;
        }

        let selected_thread = self.processes.selected_thread_id.borrow();
        let selected_inferior = self.processes.selected_inferior_id.borrow();

        self.stopped
            .active_stop_context
            .borrow()
            .as_ref()
            .is_some_and(|context| {
                selected_thread.as_deref() == Some(context.thread_id())
                    && self.processes.selected_frame_level.get() == context.frame_level()
                    && context
                        .inferior_id()
                        .is_none_or(|inferior| selected_inferior.as_deref() == Some(inferior))
            })
    }

    pub(crate) fn current_stop_refresh_generation(&self) -> u64 {
        self.stopped.stop_refresh_generation.get()
    }

    pub(crate) fn cached_register_names(&self) -> Option<Rc<Vec<String>>> {
        self.stopped.cached_register_names.borrow().clone()
    }

    pub(crate) fn cache_register_names(&self, names: Rc<Vec<String>>) {
        if !names.is_empty() {
            self.stopped.cached_register_names.replace(Some(names));
        }
    }

    pub(crate) fn registers_for_details(&self, generation: u64) -> Option<Vec<Register>> {
        (self.is_stop_refresh_current(generation)
            && self.stopped.latest_registers_generation.get() == Some(generation))
        .then(|| self.stopped.latest_registers.borrow().clone())
        .filter(|registers| !registers.is_empty())
    }

    pub(crate) fn claim_register_details(&self, generation: u64) -> bool {
        self.is_stop_refresh_current(generation)
            && self
                .stopped
                .register_details_generation
                .replace(Some(generation))
                != Some(generation)
    }

    pub(crate) fn stack_for_details(&self, generation: u64) -> Option<Vec<StackEntry>> {
        (self.is_stop_refresh_current(generation)
            && self.stopped.latest_stack_generation.get() == Some(generation))
        .then(|| self.stopped.latest_stack.borrow().clone())
        .filter(|entries| !entries.is_empty())
    }

    pub(crate) fn claim_stack_details(&self, generation: u64) -> bool {
        self.is_stop_refresh_current(generation)
            && self
                .stopped
                .stack_details_generation
                .replace(Some(generation))
                != Some(generation)
    }

    pub(crate) fn claim_stack_memory_refresh(&self, generation: u64) -> bool {
        self.is_stop_refresh_current(generation)
            && self
                .stopped
                .stack_memory_refresh_generation
                .replace(Some(generation))
                != Some(generation)
    }

    pub(crate) fn claim_memory_watches_refresh(&self, generation: u64) -> bool {
        self.is_stop_refresh_current(generation)
            && self
                .stopped
                .memory_watches_refresh_generation
                .replace(Some(generation))
                != Some(generation)
    }

    pub(crate) fn claim_tls_runtime_refresh(&self, generation: u64) -> bool {
        self.is_stop_refresh_current(generation)
            && self
                .stopped
                .tls_runtime_refresh_generation
                .replace(Some(generation))
                != Some(generation)
    }

    pub(crate) fn start_stop_refresh(&self) -> u64 {
        self.stopped.active_stop_context.borrow_mut().take();
        let generation = self.stopped.stop_refresh_generation.get().wrapping_add(1);
        self.stopped.stop_refresh_generation.set(generation);
        let latest = self.stopped.latest_registers.borrow();
        let mut previous = self.stopped.previous_registers.borrow_mut();
        previous.clear();
        previous.reserve(latest.len());
        previous.extend(
            latest
                .iter()
                .map(|register| (register.name.clone(), register.value.clone())),
        );

        generation
    }

    pub(crate) fn bind_stop_context(&self, transport_epoch: u64) -> Option<StopContext> {
        let thread = self.current_thread_id()?;
        let frame = match self.processes.selected_frame_level.get() {
            u32::MAX => 0,
            level => level,
        };
        self.processes.selected_frame_level.set(frame);
        let context = StopContext::new(
            transport_epoch,
            self.current_stop_refresh_generation(),
            self.selected_inferior_id(),
            thread,
            frame,
        )?;
        self.stopped
            .active_stop_context
            .replace(Some(context.clone()));

        Some(context)
    }

    pub(crate) fn frames(&self) -> Rc<[StackFrame]> {
        Rc::clone(&self.stopped.latest_frames.borrow())
    }

    pub(crate) fn publish_frames(&self, generation: Option<u64>, frames: &[StackFrame]) -> bool {
        if generation.is_some_and(|generation| !self.is_stop_refresh_current(generation)) {
            return false;
        }

        let mut latest = self.stopped.latest_frames.borrow_mut();

        if latest.as_ref() != frames {
            *latest = Rc::from(frames);
        }

        self.stopped.latest_frames_generation.set(generation);
        true
    }

    fn update_registers(&self, registers: &[Register]) -> bool {
        let mut latest = self.stopped.latest_registers.borrow_mut();
        let changed = !same_register_values(&latest, registers);

        if changed {
            *latest = registers.to_vec();
        }

        changed
    }

    pub(crate) fn publish_registers(&self, registers: &[Register]) -> bool {
        self.stopped.latest_registers_generation.set(None);
        self.update_registers(registers)
    }

    pub(crate) fn publish_registers_for_refresh(
        &self,
        generation: u64,
        registers: &[Register],
    ) -> Option<bool> {
        if !self.is_stop_refresh_current(generation) {
            return None;
        }

        let first = self.stopped.latest_registers_generation.get() != Some(generation);
        let changed = self.update_registers(registers);
        self.stopped
            .latest_registers_generation
            .set(Some(generation));

        Some(first || changed)
    }

    pub(crate) fn publish_stack(&self, generation: Option<u64>, entries: &[StackEntry]) -> bool {
        if generation.is_some_and(|generation| !self.is_stop_refresh_current(generation)) {
            return false;
        }

        let mut latest = self.stopped.latest_stack.borrow_mut();

        if latest.as_slice() != entries {
            *latest = entries.to_vec();
        }

        self.stopped.latest_stack_generation.set(generation);
        true
    }

    pub(crate) fn publish_memory_regions(
        &self,
        generation: u64,
        regions: &[MemoryRegion],
    ) -> Option<bool> {
        if !self.is_stop_refresh_current(generation) {
            return None;
        }

        let mut latest = self.stopped.memory_regions.borrow_mut();
        let changed = latest.as_slice() != regions;

        if changed {
            *latest = regions.to_vec();
        }

        self.stopped.memory_regions_generation.set(Some(generation));
        Some(changed)
    }

    pub(crate) fn clear_memory_regions(&self) {
        self.stopped.memory_regions.borrow_mut().clear();
        self.stopped.memory_regions_generation.set(None);
    }

    pub(crate) fn clear_previous_registers(&self) {
        self.stopped.previous_registers.borrow_mut().clear();
    }

    pub(crate) fn clear_register_names(&self) {
        self.stopped.cached_register_names.borrow_mut().take();
    }

    pub(crate) fn registers(&self) -> std::cell::Ref<'_, Vec<Register>> {
        self.stopped.latest_registers.borrow()
    }

    pub(crate) fn memory_regions(&self) -> std::cell::Ref<'_, Vec<MemoryRegion>> {
        self.stopped.memory_regions.borrow()
    }

    pub(crate) fn previous_registers(&self) -> std::cell::Ref<'_, HashMap<String, String>> {
        self.stopped.previous_registers.borrow()
    }

    pub(crate) fn memory_regions_for_details(&self, generation: u64) -> Option<Vec<MemoryRegion>> {
        (self.is_stop_refresh_current(generation)
            && self.stopped.memory_regions_generation.get() == Some(generation))
        .then(|| self.stopped.memory_regions.borrow().clone())
    }

    pub(crate) fn frames_for_details(&self, generation: u64) -> Option<Vec<StackFrame>> {
        (self.is_stop_refresh_current(generation)
            && self.stopped.latest_frames_generation.get() == Some(generation))
        .then(|| self.frames().to_vec())
    }

    pub(crate) fn registers_are_current(&self, generation: u64) -> bool {
        self.is_stop_refresh_current(generation)
            && self.stopped.latest_registers_generation.get() == Some(generation)
    }

    pub(super) fn invalidate_stop_context(&self) {
        self.stopped.active_stop_context.borrow_mut().take();
    }
}

fn same_register_values(left: &[Register], right: &[Register]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.name == right.name && left.value == right.value)
}
