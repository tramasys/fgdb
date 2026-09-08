//! Shared module symbol state and resolution contracts. Consumers interpret
//! their own required types and symbols, independently of debug-file loading.

mod backend;
mod download;
mod resolver;

pub(crate) use backend::Configuration;
pub(crate) use resolver::SymbolResolver;

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    rc::{Rc, Weak},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::{config::settings::SymbolDownloads, debugger::SharedLibrary};

const MAX_PENDING: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ModuleKey {
    pub inferior: String,
    pub target: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

impl ModuleKey {
    pub(crate) fn new(inferior: &str, module: &SharedLibrary) -> Self {
        Self {
            inferior: inferior.to_owned(),
            target: module.target_name.clone(),
            from: module.from.clone(),
            to: module.to.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResolveMode {
    Configured,
    LocalOnly,
    DownloadOnce,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Queued,
    Checking,
    Loading,
    Downloading,
    Verifying,
    Cancelling,
}

impl Phase {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Checking => "Checking debug information",
            Self::Loading => "Loading symbols",
            Self::Downloading => "Fetching debug information",
            Self::Verifying => "Verifying debug information",
            Self::Cancelling => "Cancelling",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum DebugInfo {
    #[default]
    Unverified,
    Missing,
    Embedded,
    Separate(PathBuf),
}

impl DebugInfo {
    /// Presence and identity do not guarantee that every DIE or external
    /// split-DWARF dependency can be read. Consumers check their own needs.
    pub(crate) fn is_present(&self) -> bool {
        matches!(self, Self::Embedded | Self::Separate(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Loaded,
    SymbolsOnly,
    NeedsConsent,
    Disabled,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug)]
pub(crate) struct ModuleSymbols {
    pub host: PathBuf,
    pub symbols_loaded: bool,
    pub debug_info: DebugInfo,
    pub build_id: Option<String>,
    pub phase: Option<Phase>,
    pub outcome: Option<Outcome>,
    pub message: String,
}

impl ModuleSymbols {
    pub(crate) fn status(&self) -> SymbolStatus {
        SymbolStatus::new(self.symbols_loaded, &self.debug_info)
    }
}

/// Symbol loading and the presence of matching DWARF data are separate facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SymbolStatus {
    NotLoaded,
    SymbolsLoaded,
    SymbolsOnly,
    DebugInfo,
}

impl SymbolStatus {
    pub(crate) fn new(symbols_loaded: bool, debug_info: &DebugInfo) -> Self {
        if !symbols_loaded {
            Self::NotLoaded
        } else {
            match debug_info {
                DebugInfo::Unverified => Self::SymbolsLoaded,
                DebugInfo::Missing => Self::SymbolsOnly,
                DebugInfo::Embedded | DebugInfo::Separate(_) => Self::DebugInfo,
            }
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NotLoaded => "NOT LOADED",
            Self::SymbolsLoaded => "SYMBOLS LOADED",
            Self::SymbolsOnly => "SYMBOLS ONLY",
            Self::DebugInfo => "DEBUG INFO PRESENT",
        }
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::NotLoaded => "GDB has not loaded this module's symbols",
            Self::SymbolsLoaded => {
                "GDB has loaded this module's symbols. DWARF presence and identity have not been checked by fgdb"
            }
            Self::SymbolsOnly => {
                "GDB has loaded this module's symbols, but no matching supported DWARF metadata was found attached to it"
            }
            Self::DebugInfo => {
                "GDB has matching DWARF data for this module. Type availability and split-DWARF dependencies are checked when inspected"
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum SymbolEvent {
    Configuration,
    Updated(ModuleKey, Phase),
    Changed(ModuleKey),
    Finished(ModuleKey, Outcome, String),
    BatchStarted,
    BatchFinished(BatchSummary),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct BatchSummary {
    pub loaded: usize,
    pub unresolved: usize,
    pub failed: usize,
    pub skipped: usize,
    pub cancelled: usize,
    pub changed: bool,
    pub errors: Vec<String>,
}

impl BatchSummary {
    pub(crate) fn message(&self) -> String {
        let mut message = format!(
            "Symbol loading: {} with debug information, {} unresolved, {} failed, {} skipped, {} cancelled",
            self.loaded, self.unresolved, self.failed, self.skipped, self.cancelled,
        );

        for error in &self.errors {
            message.push('\n');
            message.push_str(error);
        }

        message
    }
}

type Listener = Rc<dyn Fn(&SymbolEvent)>;

#[derive(Default)]
struct Listeners {
    next: Cell<u64>,
    callbacks: RefCell<HashMap<u64, Listener>>,
}

/// Dropping a subscription disconnects it without retaining the service.
pub(crate) struct SymbolSubscription {
    listeners: Weak<Listeners>,
    id: u64,
}

impl Drop for SymbolSubscription {
    fn drop(&mut self) {
        if let Some(listeners) = self.listeners.upgrade() {
            listeners.callbacks.borrow_mut().remove(&self.id);
        }
    }
}

#[derive(Clone)]
struct Ticket {
    id: u64,
    generation: u64,
    key: ModuleKey,
    active: Arc<AtomicBool>,
}

#[derive(Default)]
pub(crate) struct SymbolState {
    generation: Cell<u64>,
    revision: Cell<u64>,
    next: Cell<u64>,
    modules: RefCell<HashMap<ModuleKey, Rc<ModuleSymbols>>>,
    pending: RefCell<HashMap<ModuleKey, Ticket>>,
    batch_active: Cell<bool>,
    // There is one admitted batch. Wake it when queue capacity or module
    // ownership changes instead of polling each library on a timer.
    batch_waiter: RefCell<Option<std::task::Waker>>,
    listeners: Rc<Listeners>,
    pub(crate) policy: Cell<SymbolDownloads>,
    pub(crate) extra_directories: RefCell<Vec<PathBuf>>,
    pub(crate) configuration: RefCell<Option<Configuration>>,
}

impl SymbolState {
    pub(crate) fn batch_in_progress(&self) -> bool {
        self.batch_active.get()
    }

    fn wake_batch(&self) {
        let waiter = self.batch_waiter.borrow_mut().take();

        if let Some(waiter) = waiter {
            waiter.wake();
        }
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision.get()
    }

    pub(crate) fn lifetime(&self) -> u64 {
        self.generation.get()
    }

    pub(crate) fn subscribe(
        &self,
        callback: impl Fn(&SymbolEvent) + 'static,
    ) -> SymbolSubscription {
        let id = self.listeners.next.get().wrapping_add(1);
        self.listeners.next.set(id);
        self.listeners
            .callbacks
            .borrow_mut()
            .insert(id, Rc::new(callback));

        SymbolSubscription {
            listeners: Rc::downgrade(&self.listeners),
            id,
        }
    }

    fn emit(&self, event: SymbolEvent) {
        let generation = self.generation.get();
        let listeners: Vec<_> = self
            .listeners
            .callbacks
            .borrow()
            .values()
            .cloned()
            .collect();

        for listener in listeners {
            if self.generation.get() != generation {
                break;
            }

            listener(&event);
        }
    }

    pub(crate) fn snapshot(&self, key: &ModuleKey) -> Option<Rc<ModuleSymbols>> {
        self.modules.borrow().get(key).cloned()
    }

    pub(crate) fn observe(&self, inferior: &str, modules: &[SharedLibrary]) -> bool {
        let mut old = self.modules.borrow_mut();
        let live: std::collections::HashSet<_> = modules
            .iter()
            .map(|module| ModuleKey::new(inferior, module))
            .collect();

        // Keep the selected module inventory, not a growing archive of every
        // inferior ever visited. GDB remains authoritative when switching back.
        let previous_count = old.len();
        old.retain(|key, _| live.contains(key));
        let mut changed = old.len() != previous_count;

        for module in modules {
            let key = ModuleKey::new(inferior, module);
            let host = PathBuf::from(module.host_name.as_deref().unwrap_or(&module.target_name));
            let entry = old.entry(key).or_insert_with(|| {
                changed = true;
                Rc::new(ModuleSymbols {
                    host: host.clone(),
                    symbols_loaded: module.symbols_loaded,
                    debug_info: DebugInfo::Unverified,
                    build_id: None,
                    phase: None,
                    outcome: None,
                    message: String::new(),
                })
            });

            if entry.host == host && entry.symbols_loaded == module.symbols_loaded {
                continue;
            }

            changed = true;
            let entry = Rc::make_mut(entry);

            if entry.host != host || (entry.symbols_loaded && !module.symbols_loaded) {
                entry.debug_info = DebugInfo::Unverified;
                entry.build_id = None;
            }

            entry.host = host;
            entry.symbols_loaded = module.symbols_loaded;
        }

        self.pending.borrow_mut().retain(|key, ticket| {
            let keep = old.contains_key(key);

            if !keep {
                ticket.active.store(false, Ordering::Release);
            }

            keep
        });

        drop(old);
        self.wake_batch();
        changed
    }

    pub(crate) fn reset(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));

        for ticket in self.pending.borrow_mut().drain().map(|(_, ticket)| ticket) {
            ticket.active.store(false, Ordering::Release);
        }

        self.modules.borrow_mut().clear();
        self.configuration.borrow_mut().take();
        self.batch_active.set(false);
        self.wake_batch();
    }

    pub(crate) fn forget_inferior(&self, inferior: &str) {
        self.modules
            .borrow_mut()
            .retain(|key, _| key.inferior != inferior);

        self.pending.borrow_mut().retain(|key, ticket| {
            if key.inferior == inferior {
                ticket.active.store(false, Ordering::Release);
                false
            } else {
                true
            }
        });
        self.wake_batch();
    }

    pub(crate) fn cancel(&self, key: &ModuleKey) {
        let ticket = self.pending.borrow().get(key).cloned();

        if let Some(ticket) = ticket {
            ticket.active.store(false, Ordering::Release);

            if self
                .snapshot(key)
                .is_some_and(|state| state.phase == Some(Phase::Queued))
            {
                self.finish(
                    &ticket,
                    Outcome::Cancelled,
                    String::from("Queued symbol request cancelled"),
                    false,
                );
            } else {
                self.phase(&ticket, Phase::Cancelling);
            }
        }
    }

    fn begin(&self, key: ModuleKey) -> Result<Option<Ticket>, String> {
        if self.pending.borrow().contains_key(&key) {
            return Ok(None);
        }

        if self.pending.borrow().len() >= MAX_PENDING {
            return Err(format!(
                "At most {MAX_PENDING} symbol requests can be queued"
            ));
        }

        if !self.modules.borrow().contains_key(&key) {
            return Err(String::from(
                "This module is no longer in the current module list",
            ));
        }

        let id = self.next.get().wrapping_add(1);
        self.next.set(id);

        let ticket = Ticket {
            id,
            generation: self.generation.get(),
            key,
            active: Arc::new(AtomicBool::new(true)),
        };

        self.pending
            .borrow_mut()
            .insert(ticket.key.clone(), ticket.clone());

        self.phase(&ticket, Phase::Queued);
        Ok(Some(ticket))
    }

    fn owns(&self, ticket: &Ticket) -> bool {
        self.generation.get() == ticket.generation
            && self
                .pending
                .borrow()
                .get(&ticket.key)
                .is_some_and(|current| current.id == ticket.id)
    }

    fn current(&self, ticket: &Ticket) -> bool {
        ticket.active.load(Ordering::Acquire) && self.owns(ticket)
    }

    fn phase(&self, ticket: &Ticket, phase: Phase) {
        if self.owns(ticket) {
            if let Some(module) = self.modules.borrow_mut().get_mut(&ticket.key) {
                Rc::make_mut(module).phase = Some(phase);
            }

            self.emit(SymbolEvent::Updated(ticket.key.clone(), phase));
        }
    }

    fn finish(&self, ticket: &Ticket, outcome: Outcome, mut message: String, changed: bool) {
        if !self.owns(ticket) {
            return;
        }

        self.pending.borrow_mut().remove(&ticket.key);
        ticket.active.store(false, Ordering::Release);

        if message.len() > 4096 {
            message.truncate(message.floor_char_boundary(4096));
            message.push_str("\nFurther diagnostics omitted");
        }

        if let Some(module) = self.modules.borrow_mut().get_mut(&ticket.key) {
            let module = Rc::make_mut(module);
            module.phase = None;
            module.outcome = Some(outcome);
            module.message.clone_from(&message);
        }

        self.wake_batch();

        if changed {
            self.revision.set(self.revision.get().wrapping_add(1));
            self.emit(SymbolEvent::Changed(ticket.key.clone()));
        }

        if self.generation.get() == ticket.generation {
            self.emit(SymbolEvent::Finished(ticket.key.clone(), outcome, message));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_status_distinguishes_loading_from_debug_information_verification() {
        for (info, expected) in [
            (DebugInfo::Unverified, SymbolStatus::SymbolsLoaded),
            (DebugInfo::Missing, SymbolStatus::SymbolsOnly),
            (DebugInfo::Embedded, SymbolStatus::DebugInfo),
            (
                DebugInfo::Separate(PathBuf::from("/debug/libc.debug")),
                SymbolStatus::DebugInfo,
            ),
        ] {
            assert_eq!(SymbolStatus::new(true, &info), expected);
            assert_eq!(SymbolStatus::new(false, &info), SymbolStatus::NotLoaded);
        }
    }

    fn module() -> SharedLibrary {
        SharedLibrary {
            target_name: String::from("/lib/example.so"),
            host_name: None,
            from: Some(String::from("0x1000")),
            to: Some(String::from("0x2000")),
            symbols_loaded: false,
        }
    }

    #[test]
    fn cancellation_and_new_sessions_cannot_publish_stale_success() {
        let state = Rc::new(SymbolState::default());
        let module = module();
        let key = ModuleKey::new("i1", &module);
        state.observe("i1", std::slice::from_ref(&module));
        let first = state.begin(key.clone()).unwrap().unwrap();
        assert!(state.begin(key.clone()).unwrap().is_none());
        state.cancel(&key);
        assert!(!state.current(&first));
        assert_eq!(
            state.snapshot(&key).unwrap().outcome,
            Some(Outcome::Cancelled)
        );

        let second = state.begin(key.clone()).unwrap().unwrap();
        state.reset();
        state.observe("i1", &[module]);
        let third = state.begin(key.clone()).unwrap().unwrap();
        state.finish(&first, Outcome::Loaded, String::new(), true);
        state.finish(&second, Outcome::Loaded, String::new(), true);
        assert!(state.current(&third));
        assert_eq!(state.revision(), 0);

        let observed = Rc::new(Cell::new(0));
        let count = Rc::clone(&observed);
        let weak = Rc::downgrade(&state);

        let subscription = state.subscribe(move |event| {
            if let SymbolEvent::Changed(key) = event {
                count.set(count.get() + 1);
                assert!(weak.upgrade().unwrap().snapshot(key).is_some());
            }
        });

        state.finish(&third, Outcome::Loaded, String::from("loaded"), true);
        assert_eq!(observed.get(), 1);
        assert_eq!(state.revision(), 1);
        drop(subscription);
        assert!(state.listeners.callbacks.borrow().is_empty());

        let fourth = state.begin(key).unwrap().unwrap();
        let weak = Rc::downgrade(&state);
        let subscription = state.subscribe(move |event| match event {
            SymbolEvent::Changed(_) => weak.upgrade().unwrap().reset(),
            SymbolEvent::Finished(..) => {
                panic!("published completion after a reentrant session reset")
            }
            _ => {}
        });

        state.finish(&fourth, Outcome::Loaded, String::new(), true);
        drop(subscription);
    }

    #[test]
    fn remapping_and_selection_changes_cancel_module_leases() {
        let state = SymbolState::default();
        let mut module = module();
        state.observe("i1", std::slice::from_ref(&module));
        let first = state.begin(ModuleKey::new("i1", &module)).unwrap().unwrap();
        module.from = Some(String::from("0x3000"));
        state.observe("i1", std::slice::from_ref(&module));
        assert!(!state.current(&first));
        let second = state.begin(ModuleKey::new("i1", &module)).unwrap().unwrap();
        state.observe("i2", &[module]);
        assert!(!state.current(&second));
        assert_eq!(state.modules.borrow().len(), 1);
    }
}
