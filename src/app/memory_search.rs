//! One background read at a time, guarded by both stop identity and job ownership.

use super::*;
use crate::memory_search::{Action, Query, Scan};
use gtk::glib;
use std::time::{Duration, Instant};

const SEARCH_DEADLINE: Duration = Duration::from_secs(120);
const UPDATE_INTERVAL: Duration = Duration::from_millis(100);

struct Controller {
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    active: RefCell<Option<Rc<Search>>>,
}

struct Search {
    owner: Weak<Controller>,
    requests: StopRequests,
    scan: RefCell<Scan>,
    started: Instant,
    published: Cell<(u64, u64, usize)>,
    finished: Cell<bool>,
}

pub(super) fn connect(ui: &Rc<Ui>, client: &Rc<MiClient>) {
    let controller = Rc::new(Controller {
        ui: Rc::downgrade(ui),
        client: Rc::clone(client),
        active: RefCell::new(None),
    });

    ui.connect_memory_search_actions(move |action| {
        let Some(ui) = controller.ui.upgrade() else {
            return;
        };

        match action {
            Action::Start(query) => {
                if let Err(error) = controller.start(&ui, query) {
                    ui.memory_search_error(&error);
                }
            }
            Action::Cancel => {
                let active = controller.active.borrow().clone();

                if let Some(search) = active {
                    search.finish("Cancelled - partial results");
                }
            }
            Action::Inspect(address) => {
                if ui.model.debugger_synchronization_available() {
                    ui.inspect_memory_search_hit(address);
                }
            }
        }
    });
}

impl Controller {
    fn start(self: &Rc<Self>, ui: &Ui, query: Query) -> Result<(), String> {
        if self.active.borrow().is_some() || !ui.model.debugger_synchronization_available() {
            return Err(String::from(
                "Pause the target and wait for the current operation",
            ));
        }

        if !self
            .client
            .capabilities()
            .supports("data-read-memory-bytes")
        {
            return Err(String::from(
                "This GDB does not support byte-range memory reads",
            ));
        }

        let generation = ui.model.current_stop_refresh_generation();
        let requests = stop_requests(&self.ui, &self.client, generation)
            .filter(StopRequests::is_current)
            .ok_or("A current stopped target is required")?;

        let scan = Scan::new(
            query,
            ui.model.known_target_pointer_bits(),
            ui.model.target_endian(),
        )?;
        let search = Rc::new(Search {
            owner: Rc::downgrade(self),
            requests,
            scan: RefCell::new(scan),
            started: Instant::now(),
            published: Cell::new((0, 0, 0)),
            finished: Cell::new(false),
        });

        self.active.replace(Some(Rc::clone(&search)));
        let origin = match ui.model.current_session() {
            Some(DebugSession::CoreDump { .. }) => {
                "Core-session memory through GDB, not the inspector cache"
            }
            Some(DebugSession::RrReplay { .. }) => {
                "Replay memory through GDB, not the inspector cache"
            }
            _ => "Target memory through GDB, not the inspector cache",
        };

        ui.begin_memory_search(generation, origin);
        let weak = Rc::downgrade(&search);

        // Also retire stalled or dropped callbacks. This timer exists only while
        // a search is active and never starts another read in parallel.
        glib::timeout_add_local(UPDATE_INTERVAL, move || {
            let Some(search) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };

            if !search.check() {
                return glib::ControlFlow::Break;
            }

            search.publish("Searching…", false);
            glib::ControlFlow::Continue
        });

        search.schedule();
        Ok(())
    }
}

impl Search {
    fn check(&self) -> bool {
        if self.finished.get() {
            return false;
        }

        if !self.requests.is_current() {
            self.finish("Stopped context changed - partial results");
            return false;
        }

        if self.started.elapsed() >= SEARCH_DEADLINE {
            self.finish("Search time limit reached - partial results");
            return false;
        }

        true
    }

    fn schedule(self: &Rc<Self>) {
        let search = Rc::clone(self);
        glib::idle_add_local_once(move || search.next());
    }

    fn next(self: &Rc<Self>) {
        if !self.check() {
            return;
        }

        let range = self.scan.borrow_mut().next_read();
        let Some(range) = range else {
            let phase = {
                let scan = self.scan.borrow();
                scan.limit_reached()
                    .unwrap_or(if scan.progress.skipped == 0 {
                        "Complete"
                    } else {
                        "Finished with skipped bytes"
                    })
            };

            self.finish(phase);
            return;
        };

        let command = format!(
            "-data-read-memory-bytes 0x{:x} {}",
            range.start,
            range.end - range.start
        );

        let weak = Rc::downgrade(self);
        let search = Rc::clone(self);
        let result = self
            .requests
            .thread(&command)
            .when(move || weak.upgrade().is_some_and(|search| !search.finished.get()))
            .background(move |_, record| {
                if !search.check() {
                    return;
                }

                if record.is_done() {
                    match crate::debugger::memory_blocks(&record, range.clone()) {
                        Ok(blocks) if !blocks.is_empty() => {
                            search.scan.borrow_mut().accept(range, &blocks)
                        }
                        Ok(_) => search
                            .scan
                            .borrow_mut()
                            .failed(range, "GDB returned no readable bytes"),
                        Err(error) => {
                            search.finish(error);
                            return;
                        }
                    }
                } else {
                    let message = record
                        .error_message()
                        .unwrap_or("Memory read did not complete");

                    if record.class == "error"
                        && [
                            "Unable to read memory",
                            "Cannot access memory at address",
                            "Cannot read memory at address",
                        ]
                        .iter()
                        .any(|prefix| message.starts_with(prefix))
                    {
                        search.scan.borrow_mut().failed(range, message);
                    } else {
                        search.finish(message);
                        return;
                    }
                }

                search.schedule();
            });

        if let Err(error) = result {
            self.finish(&format!("Memory search stopped: {error}"));
        }
    }

    fn publish(&self, phase: &str, finished: bool) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };

        let Some(ui) = owner.ui.upgrade() else { return };

        let scan = self.scan.borrow();
        let progress = &scan.progress;
        let signature = (progress.searched, progress.skipped, progress.hits.len());

        if finished || self.published.get() != signature {
            self.published.set(signature);
            ui.show_memory_search(progress, phase, finished);
        }
    }

    fn finish(&self, phase: &str) {
        if self.finished.replace(true) {
            return;
        }

        self.publish(phase, true);

        if let Some(owner) = self.owner.upgrade() {
            owner.active.borrow_mut().take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires GDB and the C memory-search fixture"]
    fn live_transfer_sizes_preserve_search_results_and_byte_accounting() {
        use crate::app::test_support::open_debugger;
        use crate::memory_search::SearchKind;

        let (_debugger, client) = open_debugger("c-memory-search-target", "search_checkpoint");
        glib::MainContext::default().block_on(async {
            async fn request(client: &MiClient, command: &str) -> MiRecord {
                let (sender, receiver) = futures_channel::oneshot::channel();
                client.request(command, move |_, record| { let _ = sender.send(record); }).unwrap();
                glib::future_with_timeout(Duration::from_secs(2), receiver).await.unwrap().unwrap()
            }

            let address = request(&client, "-data-evaluate-expression \"(unsigned long long)&search_bytes\"").await;
            let address = crate::debugger::evaluated_value(&address).unwrap().parse::<u64>().unwrap();
            let length = 131_072_u64;
            for iteration in 0..6 {
                for read_bytes in [64 * 1024, crate::memory_search::READ_BYTES] {
                    let query = Query {
                        kind: SearchKind::Text, value: "fgdb-needle-42".into(),
                        ranges: std::iter::once(address..address + length).collect(),
                        aligned: false, max_results: 100, max_bytes: length,
                    };
                    let mut scan = Scan::new(query, None, None).unwrap();
                    let started = Instant::now();
                    let mut start = address;
                    let mut requests = 0;
                    while start < address + length {
                        let end = (start + read_bytes).min(address + length);
                        let record = request(&client, &format!("-data-read-memory-bytes 0x{start:x} {}", end - start)).await;
                        let blocks = crate::debugger::memory_blocks(&record, start..end).unwrap();
                        scan.accept(start..end, &blocks);
                        requests += 1;
                        start = end;
                    }
                    eprintln!("memory-transfer bytes={read_bytes} iteration={iteration} requests={requests} us={}", started.elapsed().as_micros());
                    assert_eq!(scan.progress.searched, length);
                    assert_eq!(scan.progress.skipped, 0);
                    assert_eq!(scan.progress.hits.iter().map(|hit| hit.address - address).collect::<Vec<_>>(), [65533, 65590]);
                }
            }
        });
    }
}
