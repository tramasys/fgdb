use std::{
    collections::VecDeque,
    os::unix::fs::MetadataExt,
    sync::mpsc::{self, TryRecvError},
    time::{Duration, Instant},
};

use gtk::{gio, glib};

use super::*;
use crate::{
    debug_info::{self, ModuleDebugMetadata},
    debugger::{MiClient, MiRecord},
    model::DebuggerModel,
};

/// One serialized resolver for all consumers. Requests for an existing module
/// coalesce, and later requests reuse the debuginfod client's validated cache.
pub(crate) struct SymbolResolver {
    model: Weak<DebuggerModel>,
    client: Weak<MiClient>,
    queue: RefCell<VecDeque<Rc<Job>>>,
    running: Cell<bool>,
    configuration_gate: crate::model::RefreshGate,
    batch_running: Cell<bool>,
}

struct Job {
    model: Weak<DebuggerModel>,
    client: Weak<MiClient>,
    state: Rc<SymbolState>,
    epoch: u64,
    ticket: Ticket,
    mode: ResolveMode,
    directories: Vec<PathBuf>,
    changed: Cell<bool>,
}

impl SymbolResolver {
    pub(crate) fn new(model: &Rc<DebuggerModel>, client: &Rc<MiClient>) -> Rc<Self> {
        Rc::new(Self {
            model: Rc::downgrade(model),
            client: Rc::downgrade(client),
            queue: RefCell::new(VecDeque::new()),
            running: Cell::new(false),
            configuration_gate: crate::model::RefreshGate::default(),
            batch_running: Cell::new(false),
        })
    }

    pub(crate) fn resolve(
        self: &Rc<Self>,
        key: ModuleKey,
        mode: ResolveMode,
    ) -> Result<(), String> {
        let model = self
            .model
            .upgrade()
            .ok_or("The debugger session was closed")?;

        let client = self.client.upgrade().ok_or("GDB is unavailable")?;

        if !client.is_ready()
            || !model.debugger_synchronization_available()
            || model.selected_inferior_id().as_deref() != Some(&key.inferior)
        {
            return Err(String::from(
                "Pause the selected inferior before resolving symbols",
            ));
        }

        backend::exact_regex(&key.target)?;
        self.queue
            .borrow_mut()
            .retain(|job| job.state.current(&job.ticket));

        let Some(ticket) = model.symbols.begin(key)? else {
            return Ok(());
        };

        let job = Rc::new(Job {
            model: self.model.clone(),
            client: self.client.clone(),
            state: Rc::clone(&model.symbols),
            epoch: client.transport_epoch(),
            ticket,
            mode,
            directories: model.symbols.extra_directories.borrow().clone(),
            changed: Cell::new(false),
        });

        self.queue.borrow_mut().push_back(job);

        if !self.running.replace(true) {
            let resolver = Rc::clone(self);

            glib::spawn_future_local(async move {
                loop {
                    let job = resolver.queue.borrow_mut().pop_front();
                    let Some(job) = job else { break };
                    let result = Rc::clone(&job).run().await;

                    let (outcome, message) = if !job.current() {
                        (
                            Outcome::Cancelled,
                            String::from(if job.ticket.active.load(Ordering::Acquire) {
                                "Symbol resolution cancelled because the selected target changed or became unavailable"
                            } else {
                                "Symbol resolution cancelled"
                            }),
                        )
                    } else {
                        result.unwrap_or_else(|error| (Outcome::Failed, error))
                    };

                    job.state
                        .finish(&job.ticket, outcome, message, job.changed.get());
                }

                resolver.running.set(false);
            });
        }

        Ok(())
    }

    pub(crate) fn refresh_configuration(self: &Rc<Self>) {
        if !self.configuration_gate.begin() {
            return;
        }

        let resolver = Rc::clone(self);

        glib::spawn_future_local(async move {
            loop {
                resolver.read_configuration().await;

                if !resolver.configuration_gate.finish() || !resolver.configuration_gate.begin() {
                    break;
                }
            }
        });
    }

    pub(crate) fn resolve_all(self: &Rc<Self>, keys: Vec<ModuleKey>) -> Result<(), String> {
        self.resolve_all_then(keys, |_| {})
    }

    /// Completion is delivered once, including cancellation. Consumers must keep
    /// their own session lease before doing further work in the callback.
    pub(crate) fn resolve_all_then(
        self: &Rc<Self>,
        mut keys: Vec<ModuleKey>,
        completed: impl FnOnce(BatchSummary) + 'static,
    ) -> Result<(), String> {
        let model = self
            .model
            .upgrade()
            .ok_or("The debugger session was closed")?;
        let client = self.client.upgrade().ok_or("GDB is unavailable")?;
        let inferior = model
            .selected_inferior_id()
            .ok_or("No inferior is selected")?;

        if !client.is_ready() || !model.debugger_synchronization_available() {
            return Err(String::from(
                "Pause the selected inferior before resolving symbols",
            ));
        }

        if keys.is_empty() || keys.len() > 4096 {
            return Err(String::from(
                "Select between 1 and 4096 modules for bulk symbol loading",
            ));
        }

        if keys.iter().any(|key| key.inferior != inferior) {
            return Err(String::from(
                "A symbol batch must belong to the selected inferior",
            ));
        }

        let mut seen = std::collections::HashSet::new();
        keys.retain(|key| seen.insert(key.clone()));

        if self.batch_running.replace(true) {
            return Err(String::from(
                "A bulk symbol request is already in progress. Cancel its current module or wait for it to finish",
            ));
        }

        let generation = model.symbols.lifetime();
        let epoch = client.transport_epoch();
        let revision = model.symbols.revision();
        model.symbols.batch_active.set(true);
        model.symbols.emit(SymbolEvent::BatchStarted);
        let resolver = Rc::clone(self);

        glib::spawn_future_local(async move {
            let mut summary = BatchSummary::default();
            let current = || {
                model.symbols.lifetime() == generation
                    && client.is_ready()
                    && client.transport_epoch() == epoch
                    && model.selected_inferior_id().as_deref() == Some(&inferior)
                    && model.debugger_synchronization_available()
            };

            for (index, key) in keys.iter().enumerate() {
                resolver
                    .wait_for_batch(&model.symbols, || {
                        !current()
                            || model.symbols.pending.borrow().len() < MAX_PENDING
                            || model.symbols.pending.borrow().contains_key(key)
                            || model.symbols.snapshot(key).is_none()
                    })
                    .await;

                if !current() {
                    summary.cancelled = keys.len() - index;
                    break;
                }

                if model.symbols.snapshot(key).is_none() {
                    summary.skipped += 1;
                    continue;
                }

                if let Err(error) = resolver.resolve(key.clone(), ResolveMode::Configured) {
                    summary.failed += 1;

                    if summary.errors.len() < 4 {
                        summary.errors.push(format!("{}: {error}", key.target));
                    }

                    continue;
                }

                resolver
                    .wait_for_batch(&model.symbols, || {
                        !current() || !model.symbols.pending.borrow().contains_key(key)
                    })
                    .await;

                if !current() {
                    summary.cancelled = keys.len() - index;
                    break;
                }

                match model.symbols.snapshot(key).and_then(|state| state.outcome) {
                    Some(Outcome::Loaded) => summary.loaded += 1,
                    Some(Outcome::Failed) => summary.failed += 1,
                    Some(Outcome::Cancelled) => {
                        summary.cancelled = keys.len() - index;
                        break;
                    }
                    Some(_) => summary.unresolved += 1,
                    None => summary.skipped += 1,
                }
            }

            resolver.batch_running.set(false);
            summary.changed = model.symbols.revision() != revision;

            if model.symbols.lifetime() == generation {
                model.symbols.batch_active.set(false);
                model
                    .symbols
                    .emit(SymbolEvent::BatchFinished(summary.clone()));
            }

            completed(summary);
        });

        Ok(())
    }

    async fn wait_for_batch(&self, state: &SymbolState, ready: impl Fn() -> bool) {
        std::future::poll_fn(|context| {
            if ready() {
                state.batch_waiter.borrow_mut().take();
                std::task::Poll::Ready(())
            } else {
                state
                    .batch_waiter
                    .borrow_mut()
                    .replace(context.waker().clone());
                std::task::Poll::Pending
            }
        })
        .await;
    }

    async fn read_configuration(&self) {
        let (Some(model), Some(client)) = (self.model.upgrade(), self.client.upgrade()) else {
            return;
        };

        let epoch = client.transport_epoch();
        let generation = model.symbols.generation.get();
        let model_guard = self.model.clone();
        let client_guard = self.client.clone();

        let guard: Rc<dyn Fn() -> bool> = Rc::new(move || {
            client_guard
                .upgrade()
                .is_some_and(|client| client.is_ready() && client.transport_epoch() == epoch)
                && model_guard
                    .upgrade()
                    .is_some_and(|model| model.symbols.generation.get() == generation)
        });

        let command = backend::command(
            None,
            backend::Action::Configuration,
            None,
            &model.symbols.extra_directories.borrow(),
        );

        let Ok(command) = command else { return };
        let response = console(&client, command, Rc::clone(&guard)).await;

        if guard()
            && let Ok((record, output)) = response
            && record.is_done()
            && !record.output_was_truncated()
            && let Ok(configuration) = backend::parse_configuration(&output)
        {
            model.symbols.configuration.replace(Some(configuration));
            model.symbols.emit(SymbolEvent::Configuration);
        }
    }
}

impl Job {
    fn current(&self) -> bool {
        self.state.current(&self.ticket)
            && self
                .client
                .upgrade()
                .is_some_and(|client| client.is_ready() && client.transport_epoch() == self.epoch)
            && self.model.upgrade().is_some_and(|model| {
                model.debugger_synchronization_available()
                    && model.selected_inferior_id().as_deref() == Some(&self.ticket.key.inferior)
            })
    }

    fn guard(self: &Rc<Self>) -> Rc<dyn Fn() -> bool> {
        let job = Rc::clone(self);
        Rc::new(move || job.current())
    }

    async fn probe(
        self: &Rc<Self>,
        action: backend::Action<'_>,
        expected: Option<&backend::Snapshot>,
    ) -> Result<backend::Snapshot, String> {
        if !self.current() {
            return Err(String::from("Symbol request is no longer current"));
        }

        let command =
            backend::command(Some(&self.ticket.key), action, expected, &self.directories)?;
        let client = self.client.upgrade().ok_or("GDB is unavailable")?;
        let (record, output) = console(&client, command, self.guard()).await?;

        if !self.current() {
            return Err(String::from("Symbol request is no longer current"));
        }

        if !record.is_done() {
            return Err(record
                .error_message()
                .unwrap_or("GDB rejected symbol resolution")
                .to_owned());
        }

        if record.output_was_truncated() {
            return Err(String::from(
                "GDB's symbol response was truncated. Success could not be verified",
            ));
        }

        let snapshot = backend::parse(&output)?;
        let changed = self.state.configuration.borrow().as_ref() != Some(&snapshot.configuration);

        if changed {
            self.state
                .configuration
                .replace(Some(snapshot.configuration.clone()));

            self.state.emit(SymbolEvent::Configuration);
        }

        Ok(snapshot)
    }

    async fn run(self: Rc<Self>) -> Result<(Outcome, String), String> {
        self.state.phase(&self.ticket, Phase::Checking);
        let capabilities = self
            .client
            .upgrade()
            .ok_or("GDB is unavailable")?
            .capabilities();

        if capabilities.features_known && !capabilities.supports("python") {
            return self.local_compatibility().await;
        }

        let mut snapshot = match self.probe(backend::Action::Inspect, None).await {
            Ok(snapshot) => snapshot,
            Err(error)
                if error
                    .contains("Verified module resolution requires GDB's Python MI interface") =>
            {
                return self.local_compatibility().await;
            }

            Err(error) => return Err(error),
        };

        if !snapshot.loaded {
            self.state.phase(&self.ticket, Phase::Loading);
            self.changed.set(true);
            snapshot = self.probe(backend::Action::Load, Some(&snapshot)).await?;
        }

        if !snapshot.loaded || snapshot.object == 0 {
            return Err(String::from(
                "GDB completed the request but did not load a symbol object for this module",
            ));
        }

        let (metadata, debug_info) = self.inspect(&snapshot).await?;
        self.publish(&snapshot, debug_info.clone());

        if debug_info.is_present() {
            return Ok((
                Outcome::Loaded,
                String::from(
                    "Matching DWARF data is present in GDB. Types and split-DWARF dependencies are checked when inspected",
                ),
            ));
        }

        if let Some(path) = metadata.separate_debug_file.as_ref() {
            return self.attach(&snapshot, &metadata, path.clone()).await;
        }

        let policy = effective_policy(self.state.policy.get(), &snapshot.configuration.debuginfod);

        if self.mode == ResolveMode::LocalOnly || policy == SymbolDownloads::Off {
            return Ok((
                Outcome::Disabled,
                String::from(
                    "Symbols loaded, debug information not found locally. Downloads are disabled for this request",
                ),
            ));
        }

        if policy != SymbolDownloads::On && self.mode != ResolveMode::DownloadOnce {
            return Ok((
                Outcome::NeedsConsent,
                String::from(
                    "Symbols loaded, debug information is missing. Choose Fetch debug info to allow a download for this module",
                ),
            ));
        }

        let build_id = snapshot.build_id.as_deref().or(metadata.build_id.as_deref())
            .ok_or("No build ID is available. Install a matching debug package or configure its debug-file directory")?;

        self.state.phase(&self.ticket, Phase::Downloading);
        let path = download::fetch(build_id, &snapshot.configuration, self.guard()).await?;
        self.attach(&snapshot, &metadata, path).await
    }

    async fn local_compatibility(self: &Rc<Self>) -> Result<(Outcome, String), String> {
        let client = self.client.upgrade().ok_or("GDB is unavailable")?;
        let key = &self.ticket.key;
        let pattern = backend::exact_regex(&key.target)?;
        let command = crate::debugger::MiCommandBuilder::new("-file-list-shared-libraries")
            .keyword("--thread-group")
            .argument(&key.inferior)
            .argument(&pattern)
            .finish();

        let before = mi(&client, command.clone(), self.guard()).await?;

        let matches = |record: &MiRecord| {
            let libraries = crate::debugger::shared_libraries(record);
            let mut matches = libraries
                .into_iter()
                .filter(|module| ModuleKey::new(&key.inferior, module) == *key);

            let first = matches.next();
            first.filter(|_| matches.next().is_none())
        };

        let module = matches(&before).ok_or("The selected module is no longer uniquely mapped")?;

        if !module.symbols_loaded {
            self.state.phase(&self.ticket, Phase::Loading);
            self.changed.set(true);
            let command = format!("with debuginfod enabled off -- sharedlibrary {pattern}");
            let (record, _) = console(&client, command, self.guard()).await?;

            if !record.is_done() {
                return Err(format!(
                    "Local symbol loading failed: {}",
                    record
                        .error_message()
                        .unwrap_or("GDB rejected the scoped command")
                ));
            }
        }

        let after = mi(&client, command, self.guard()).await?;
        let module = matches(&after)
            .filter(|module| module.symbols_loaded)
            .ok_or(
                "GDB completed the request but the selected module's symbols are still not loaded",
            )?;

        if let Some(state) = self.state.modules.borrow_mut().get_mut(key) {
            let state = Rc::make_mut(state);
            state.symbols_loaded = module.symbols_loaded;
            state.debug_info = DebugInfo::Unverified;
        }

        Ok((
            Outcome::SymbolsOnly,
            String::from(
                "Library symbols are loaded. Verifying and attaching debug files requires GDB with Python MI support. No download was attempted",
            ),
        ))
    }

    async fn inspect(
        self: &Rc<Self>,
        snapshot: &backend::Snapshot,
    ) -> Result<(ModuleDebugMetadata, DebugInfo), String> {
        let snapshot = snapshot.clone();

        self.work(move |current| {
            let metadata = debug_info::inspect_module_for_build_id(&snapshot.filename, &snapshot.configuration.search, snapshot.build_id.as_deref(), &current);

            if let (Some(expected), Some(actual)) = (snapshot.build_id.as_deref(), metadata.build_id.as_deref())
                && expected != actual
            {
                return Err(String::from("The host module does not match GDB's build ID. Check the sysroot and library paths"));
            }

            let mut info = if metadata.embedded_debug_info {
                DebugInfo::Embedded
            } else if metadata.error.is_some() {
                DebugInfo::Unverified
            } else {
                DebugInfo::Missing
            };

            for file in &snapshot.debug_files {
                if debug_info::validate_debug_file(file, snapshot.build_id.as_deref().or(metadata.build_id.as_deref()), metadata.debuglink_crc, &current).is_ok() {
                    info = DebugInfo::Separate(file.clone());
                    break;
                }
            }

            Ok((metadata, info))
        }).await
    }

    fn publish(&self, snapshot: &backend::Snapshot, debug_info: DebugInfo) {
        if !self.state.owns(&self.ticket) {
            return;
        }

        if let Some(module) = self.state.modules.borrow_mut().get_mut(&self.ticket.key) {
            let module = Rc::make_mut(module);
            module.symbols_loaded = snapshot.loaded;
            module.debug_info = debug_info;
            module.build_id.clone_from(&snapshot.build_id);
        }
    }

    async fn attach(
        self: &Rc<Self>,
        snapshot: &backend::Snapshot,
        metadata: &ModuleDebugMetadata,
        path: PathBuf,
    ) -> Result<(Outcome, String), String> {
        self.state.phase(&self.ticket, Phase::Verifying);
        let candidate = path.clone();
        let build_id = snapshot
            .build_id
            .clone()
            .or_else(|| metadata.build_id.clone());

        let crc = metadata.debuglink_crc;

        let stamp = self
            .work(move |current| {
                let before = file_stamp(&candidate)?;
                debug_info::validate_debug_file(&candidate, build_id.as_deref(), crc, &current)?;

                if before != file_stamp(&candidate)? {
                    return Err(String::from("The debug file changed during validation"));
                }

                Ok(before)
            })
            .await?;

        self.state.phase(&self.ticket, Phase::Loading);
        self.changed.set(true);
        let snapshot = self
            .probe(
                backend::Action::Attach {
                    path: &path,
                    stamp: &stamp,
                },
                Some(snapshot),
            )
            .await?;

        let (_, info) = self.inspect(&snapshot).await?;
        let available = info.is_present();
        self.publish(&snapshot, info);

        if !available {
            return Err(String::from(
                "GDB completed attachment but matching DWARF data could not be confirmed",
            ));
        }

        Ok((
            Outcome::Loaded,
            format!("Loaded matching debug information from {}", path.display()),
        ))
    }

    async fn work<T: Send + 'static>(
        self: &Rc<Self>,
        work: impl FnOnce(Box<dyn Fn() -> bool + Send>) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let active = Arc::clone(&self.ticket.active);
        let queued = Arc::clone(&active);
        let (sender, receiver) = mpsc::sync_channel(1);
        let deadline = Instant::now() + Duration::from_secs(15);

        crate::background::submit_cancellable_with_priority(
            crate::background::Priority::Interactive,
            move || queued.load(Ordering::Acquire),
            move || {
                let current =
                    Box::new(move || active.load(Ordering::Acquire) && Instant::now() < deadline);
                let _ = sender.send(work(current));
            },
        )
        .map_err(|error| error.to_string())?;

        loop {
            if !self.current() {
                return Err(String::from("Symbol request cancelled"));
            }

            match receiver.try_recv() {
                Ok(result) => return result,
                Err(TryRecvError::Disconnected) => {
                    return Err(String::from("Debug-file inspection stopped"));
                }

                Err(TryRecvError::Empty) if Instant::now() >= deadline => {
                    return Err(String::from("Debug-file inspection timed out"));
                }

                Err(TryRecvError::Empty) => glib::timeout_future(Duration::from_millis(20)).await,
            }
        }
    }
}

fn effective_policy(policy: SymbolDownloads, gdb: &str) -> SymbolDownloads {
    if policy == SymbolDownloads::Gdb {
        match gdb {
            "on" => SymbolDownloads::On,
            "off" => SymbolDownloads::Off,
            _ => SymbolDownloads::Ask,
        }
    } else {
        policy
    }
}

fn file_stamp(path: &std::path::Path) -> Result<String, String> {
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    let modified = i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec());
    Ok(format!(
        "{}:{}:{}:{modified}",
        metadata.dev(),
        metadata.ino(),
        metadata.len()
    ))
}

async fn console(
    client: &Rc<MiClient>,
    command: String,
    guard: Rc<dyn Fn() -> bool>,
) -> Result<(MiRecord, String), String> {
    gio::GioFuture::new(client, move |client, _, send| {
        let sender = Rc::new(RefCell::new(Some(send)));
        let response = Rc::clone(&sender);

        if let Err(error) = client.request_console_when(
            &command,
            move || guard(),
            move |_, record, output| {
                if let Some(sender) = response.borrow_mut().take() {
                    sender.resolve(Ok((record, output)));
                }
            },
        ) && let Some(sender) = sender.borrow_mut().take()
        {
            sender.resolve(Err(error.to_string()));
        }
    })
    .await
}

async fn mi(
    client: &Rc<MiClient>,
    command: String,
    guard: Rc<dyn Fn() -> bool>,
) -> Result<MiRecord, String> {
    gio::GioFuture::new(client, move |client, _, send| {
        let sender = Rc::new(RefCell::new(Some(send)));
        let response = Rc::clone(&sender);

        if let Err(error) = client.request_when(
            &command,
            move || guard(),
            move |_, record| {
                if let Some(sender) = response.borrow_mut().take() {
                    sender.resolve(Ok(record));
                }
            },
        ) && let Some(sender) = sender.borrow_mut().take()
        {
            sender.resolve(Err(error.to_string()));
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_policy_never_treats_unknown_settings_as_consent() {
        assert_eq!(
            effective_policy(SymbolDownloads::Gdb, "ask"),
            SymbolDownloads::Ask
        );

        assert_eq!(
            effective_policy(SymbolDownloads::Gdb, "unavailable"),
            SymbolDownloads::Ask
        );

        assert_eq!(
            effective_policy(SymbolDownloads::Gdb, "off"),
            SymbolDownloads::Off
        );

        assert_eq!(
            effective_policy(SymbolDownloads::Gdb, "on"),
            SymbolDownloads::On
        );

        assert_eq!(
            effective_policy(SymbolDownloads::Off, "on"),
            SymbolDownloads::Off
        );
    }

    #[test]
    #[ignore = "requires GDB with Python MI, a C compiler, objcopy, strip and debuginfod-find"]
    fn live_symbol_resolution_loads_fetches_attaches_and_cancels_without_a_display() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            process::{Child, Command, Stdio},
            sync::atomic::AtomicUsize,
        };

        struct Fixture {
            directory: PathBuf,
            debugger: Option<Child>,
            server_active: Arc<AtomicBool>,
            server: Option<std::thread::JoinHandle<()>>,
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                self.server_active.store(false, Ordering::Release);

                if let Some(server) = self.server.take() {
                    let _ = server.join();
                }

                if let Some(mut debugger) = self.debugger.take() {
                    let _ = debugger.kill();
                    let _ = debugger.wait();
                }

                let _ = std::fs::remove_dir_all(&self.directory);
            }
        }

        fn run(command: &mut Command) {
            let result = command.output().unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }

        async fn wait_for(mut ready: impl FnMut() -> bool) {
            let started = Instant::now();

            while !ready() {
                assert!(
                    started.elapsed() < Duration::from_secs(15),
                    "live symbol request timed out"
                );

                glib::timeout_future(Duration::from_millis(10)).await;
            }
        }

        let directory = Command::new("mktemp")
            .args(["-d", "/tmp/fgdb-symbol-test.XXXXXX"])
            .output()
            .unwrap();

        assert!(directory.status.success());
        let directory = PathBuf::from(String::from_utf8(directory.stdout).unwrap().trim());

        let mut fixture = Fixture {
            directory: directory.clone(),
            debugger: None,
            server_active: Arc::new(AtomicBool::new(true)),
            server: None,
        };

        let library = directory.join("libsymbol [x]+(a)?{b}|$^\\\"é.so");
        let debug = directory.join("separate.debug");
        let executable = directory.join("main");

        std::fs::write(directory.join("library.c"), "typedef struct Payload { long number; } Payload; static Payload payload = {42}; int fixture_value(void) { return (int)payload.number; }\n").unwrap();
        std::fs::write(directory.join("main.c"), "#include <dlfcn.h>\n__attribute__((noinline)) void checkpoint(void) { __asm__ volatile(\"\" ::: \"memory\"); }\nint main(int argc, char **argv) { if (argc != 2) return 2; void *module = dlopen(argv[1], RTLD_NOW); if (!module) return 3; checkpoint(); return 0; }\n").unwrap();
        run(Command::new("cc")
            .args(["-g", "-O0", "-fPIC", "-shared", "-Wl,--build-id"])
            .arg(directory.join("library.c"))
            .arg("-o")
            .arg(&library));

        run(Command::new("objcopy")
            .arg("--only-keep-debug")
            .arg(&library)
            .arg(&debug));

        run(Command::new("strip").arg("--strip-debug").arg(&library));
        run(Command::new("cc")
            .args(["-g", "-O0"])
            .arg(directory.join("main.c"))
            .args(["-ldl", "-o"])
            .arg(&executable));

        let bytes = std::fs::read(&debug).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let active = Arc::clone(&fixture.server_active);
        let requests = Arc::new(AtomicUsize::new(0));
        let request_count = Arc::clone(&requests);

        fixture.server = Some(std::thread::spawn(move || {
            while active.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                };

                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();

                let mut request = [0_u8; 4096];
                let count = stream.read(&mut request).unwrap_or(0);
                request_count.fetch_add(1, Ordering::AcqRel);

                if String::from_utf8_lossy(&request[..count]).contains("/ffff/") {
                    while active.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                } else {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    );

                    let _ = stream.write_all(&bytes);
                }
            }
        }));

        let context = glib::MainContext::default();
        let _owner = context.acquire().unwrap();
        let model = Rc::new(DebuggerModel::new(None));
        let observer = Rc::clone(&model);

        let client = MiClient::open(move |_, event| {
            if let crate::debugger::MiEvent::Ready(capabilities) = event {
                observer.set_gdb_capabilities(capabilities);
                observer.set_controls_ready(true);
            }
        })
        .unwrap();

        fixture.debugger = Some(
            Command::new("gdb")
                .args([
                    "-nx",
                    "-q",
                    "-ex",
                    "set confirm off",
                    "-ex",
                    "set debuginfod enabled off",
                    "-ex",
                    "set auto-solib-add off",
                    "-ex",
                    "break checkpoint",
                    "-ex",
                    "run",
                    "-ex",
                    "set debuginfod enabled ask",
                ])
                .arg("-ex")
                .arg(format!("set debuginfod urls {url}"))
                .arg("-ex")
                .arg(format!("new-ui mi {}", client.slave_path().display()))
                .arg("--args")
                .arg(&executable)
                .arg(&library)
                .env("DEBUGINFOD_CACHE_PATH", directory.join("cache"))
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );

        context.block_on(async {
            wait_for(|| client.is_ready()).await;
            let groups = mi(
                &client,
                String::from("-list-thread-groups --recurse 1"),
                Rc::new(|| true),
            )
            .await
            .unwrap();

            model.show_inferiors(crate::debugger::inferiors(&groups, Some("1")));
            model.set_selected_inferior("i1");
            model.set_controls_running(false);
            assert!(model.debugger_synchronization_available());

            let libraries = mi(
                &client,
                String::from("-file-list-shared-libraries libsymbol"),
                Rc::new(|| true),
            )
            .await
            .unwrap();

            let modules = crate::debugger::shared_libraries(&libraries);
            assert_eq!(modules.len(), 1);
            let key = ModuleKey::new("i1", &modules[0]);
            model.symbols.observe("i1", &modules);
            let resolver = SymbolResolver::new(&model, &client);

            for _ in 0..100 {
                resolver
                    .resolve(key.clone(), ResolveMode::LocalOnly)
                    .unwrap();

                assert_eq!(resolver.queue.borrow().len(), 1);
                model.symbols.cancel(&key);
            }

            resolver
                .resolve(key.clone(), ResolveMode::LocalOnly)
                .unwrap();

            resolver
                .resolve(key.clone(), ResolveMode::LocalOnly)
                .unwrap();

            assert_eq!(model.symbols.pending.borrow().len(), 1);
            wait_for(|| !model.symbols.pending.borrow().contains_key(&key)).await;
            let state = model.symbols.snapshot(&key).unwrap();
            assert_eq!(state.outcome, Some(Outcome::Disabled), "{}", state.message);
            assert!(state.symbols_loaded);
            assert_eq!(state.debug_info, DebugInfo::Missing);
            assert_eq!(requests.load(Ordering::Acquire), 0);
            model.symbols.policy.set(SymbolDownloads::Off);
            resolver
                .resolve(key.clone(), ResolveMode::DownloadOnce)
                .unwrap();

            wait_for(|| !model.symbols.pending.borrow().contains_key(&key)).await;
            assert_eq!(
                model.symbols.snapshot(&key).unwrap().outcome,
                Some(Outcome::Disabled)
            );

            assert_eq!(requests.load(Ordering::Acquire), 0);
            model.symbols.policy.set(SymbolDownloads::Gdb);

            resolver
                .resolve(key.clone(), ResolveMode::Configured)
                .unwrap();

            wait_for(|| !model.symbols.pending.borrow().contains_key(&key)).await;
            assert_eq!(
                model.symbols.snapshot(&key).unwrap().outcome,
                Some(Outcome::NeedsConsent)
            );

            assert_eq!(requests.load(Ordering::Acquire), 0);
            resolver
                .resolve(key.clone(), ResolveMode::DownloadOnce)
                .unwrap();

            wait_for(|| !model.symbols.pending.borrow().contains_key(&key)).await;
            let state = model.symbols.snapshot(&key).unwrap();
            assert_eq!(state.outcome, Some(Outcome::Loaded), "{}", state.message);
            assert!(matches!(state.debug_info, DebugInfo::Separate(_)));
            assert_eq!(requests.load(Ordering::Acquire), 1);
            assert!(debug_info::validate_debug_file(&debug, Some("0011"), None, &|| true).is_err());
            let revision = model.symbols.revision();

            resolver
                .resolve(key.clone(), ResolveMode::Configured)
                .unwrap();

            wait_for(|| !model.symbols.pending.borrow().contains_key(&key)).await;
            assert_eq!(model.symbols.revision(), revision);
            assert_eq!(requests.load(Ordering::Acquire), 1);
            assert!(resolver.resolve_all(Vec::new()).is_err());
            model.set_controls_running(true);
            assert!(resolver.resolve_all(vec![key.clone()]).is_err());
            model.set_controls_running(false);
            resolver
                .resolve_all(vec![key.clone(), key.clone()])
                .unwrap();
            assert!(resolver.resolve_all(vec![key.clone()]).is_err());
            wait_for(|| !resolver.batch_running.get()).await;
            assert_eq!(model.symbols.revision(), revision);
            assert_eq!(requests.load(Ordering::Acquire), 1);
            let configuration = model.symbols.configuration.borrow().clone().unwrap();
            assert_eq!(configuration.debuginfod, "ask");
            assert!(!configuration.auto_solib);

            let mut inventory = modules.clone();

            for index in 0..MAX_PENDING {
                let mut module = modules[0].clone();
                module.target_name = format!("/queued-{index}.so");
                inventory.push(module);
            }

            let mut invalid = modules[0].clone();
            invalid.target_name = String::from("/invalid\nmodule.so");
            let invalid_key = ModuleKey::new("i1", &invalid);
            inventory.push(invalid);
            model.symbols.observe("i1", &inventory);
            let queued = inventory
                .iter()
                .skip(modules.len())
                .take(MAX_PENDING)
                .map(|module| {
                    model
                        .symbols
                        .begin(ModuleKey::new("i1", module))
                        .unwrap()
                        .unwrap()
                })
                .collect::<Vec<_>>();
            let mut missing = key.clone();
            missing.target = String::from("/unloaded.so");
            let summary = Rc::new(RefCell::new(None));
            let result = Rc::clone(&summary);

            resolver
                .resolve_all_then(vec![key.clone(), missing, invalid_key], move |report| {
                    result.replace(Some(report));
                })
                .unwrap();

            glib::timeout_future(Duration::from_millis(10)).await;
            assert!(
                summary.borrow().is_none(),
                "a full queue must wait, not abandon the batch"
            );

            for ticket in queued {
                model.symbols.cancel(&ticket.key);
            }

            wait_for(|| summary.borrow().is_some()).await;
            let report = summary.borrow_mut().take().unwrap();
            assert_eq!(
                (
                    report.loaded,
                    report.skipped,
                    report.failed,
                    report.cancelled
                ),
                (1, 1, 1, 0)
            );
            assert_eq!(report.errors.len(), 1);
            model.symbols.observe("i1", &modules);

            let current = Rc::new(Cell::new(true));
            let current_for_download = Rc::clone(&current);
            let transfer = glib::spawn_future_local(async move {
                download::fetch(
                    "ffff",
                    &configuration,
                    Rc::new(move || current_for_download.get()),
                )
                .await
            });

            wait_for(|| requests.load(Ordering::Acquire) == 2).await;
            current.set(false);
            assert!(transfer.await.unwrap().unwrap_err().contains("cancelled"));
            let ready = mi(
                &client,
                String::from("-gdb-show auto-solib-add"),
                Rc::new(|| true),
            )
            .await
            .unwrap();

            assert!(ready.is_done());
            resolver.resolve_all(vec![key.clone()]).unwrap();
            model.symbols.reset();
            model.symbols.observe("i1", &modules);
            wait_for(|| !resolver.batch_running.get()).await;
            assert!(model.symbols.snapshot(&key).unwrap().outcome.is_none());
            let inspect =
                backend::command(Some(&key), backend::Action::Inspect, None, &[]).unwrap();

            let (record, output) = console(&client, inspect, Rc::new(|| true)).await.unwrap();
            assert!(record.is_done(), "{output}");
            let previous = backend::parse(&output).unwrap();
            let (cleared, output) = console(&client, String::from("symbol-file"), Rc::new(|| true))
                .await
                .unwrap();

            assert!(cleared.is_done(), "{output}");
            let stale =
                backend::command(Some(&key), backend::Action::Inspect, Some(&previous), &[])
                    .unwrap();

            let (record, output) = console(&client, stale, Rc::new(|| true)).await.unwrap();
            assert!(!record.is_done(), "{output}");
            assert!(
                record
                    .error_message()
                    .is_some_and(|message| message.contains("different program instance")),
                "{record:?} {output}"
            );

            let _ = mi(&client, String::from("-gdb-exit"), Rc::new(|| true)).await;
        });
    }
}
