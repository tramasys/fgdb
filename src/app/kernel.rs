use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use super::*;

const MAX_KERNEL_WORKERS: usize = 2;
static ACTIVE_KERNEL_WORKERS: AtomicUsize = AtomicUsize::new(0);
struct KernelWorkerGuard;

impl Drop for KernelWorkerGuard {
    fn drop(&mut self) {
        ACTIVE_KERNEL_WORKERS.fetch_sub(1, Ordering::Relaxed);
    }
}

fn spawn_kernel_worker<T: Send + 'static>(
    name: &str,
    read: impl FnOnce() -> T + Send + 'static,
) -> Result<futures_channel::oneshot::Receiver<T>, String> {
    ACTIVE_KERNEL_WORKERS
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |active| {
            (active < MAX_KERNEL_WORKERS).then_some(active + 1)
        })
        .map_err(|_| "Previous procfs readers are still finishing. Try again shortly".to_owned())?;

    let guard = KernelWorkerGuard;
    let (sender, receiver) = futures_channel::oneshot::channel();

    // Diagnostics can change network namespaces. Never reuse these threads.
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let result = {
                let _guard = guard;
                read()
            };

            let _ = sender.send(result);
        })
        .map_err(|error| format!("Cannot start kernel inspection worker: {error}"))?;

    Ok(receiver)
}

pub(super) fn request_kernel_refresh(ui: Weak<Ui>, client: Rc<MiClient>) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    let Some(generation) = current_ui.begin_kernel_refresh() else {
        return;
    };

    let cached_pid = current_ui.model.inferior_pid();
    let selected_inferior = current_ui.model.selected_inferior_id();
    let debugger_pid = current_ui.model.debugger_pid();
    let include_tls_metadata = current_ui.kernel_tls_requested();
    drop(current_ui);

    if let (Some(pid), Some(debugger_pid)) = (cached_pid, debugger_pid) {
        read_kernel_snapshot(ui, generation, pid, debugger_pid, include_tls_metadata);
        return;
    }

    let ui_for_response = ui.clone();

    if let Err(error) = client.request("-list-thread-groups", move |_, record| {
        let Some(current_ui) = ui_for_response.upgrade() else {
            return;
        };

        if !current_ui.kernel_refresh_is_current(generation) {
            current_ui.finish_stale_kernel_refresh();
            return;
        }

        let debugger_pid = current_ui.model.debugger_pid();
        let include_tls_metadata = current_ui.kernel_tls_requested();
        drop(current_ui);

        let Some(pid) = selected_inferior
            .as_deref()
            .and_then(|id| crate::debugger::inferior_pid_for_group(&record, id))
        else {
            show_kernel_error(
                &ui_for_response,
                generation,
                record
                    .error_message()
                    .unwrap_or("GDB did not report a live inferior process ID"),
            );

            return;
        };

        if let Some(current_ui) = ui_for_response.upgrade() {
            current_ui.model.set_inferior_pid(Some(pid));
        }

        let Some(debugger_pid) = debugger_pid else {
            show_kernel_error(
                &ui_for_response,
                generation,
                "The local GDB process identity is unavailable",
            );

            return;
        };

        read_kernel_snapshot(
            ui_for_response,
            generation,
            pid,
            debugger_pid,
            include_tls_metadata,
        );
    }) {
        show_kernel_error(&ui, generation, &error.to_string());
    }
}

fn read_kernel_snapshot(
    ui: Weak<Ui>,
    generation: u64,
    pid: u32,
    debugger_pid: u32,
    include_tls_metadata: bool,
) {
    const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(15);

    let work = crate::kernel::WorkDeadline::new(SNAPSHOT_TIMEOUT);
    let worker_work = work.clone();

    let receiver = match spawn_kernel_worker("fgdb-procfs", move || {
        crate::kernel::read_snapshot(pid, debugger_pid, include_tls_metadata, &worker_work)
    }) {
        Ok(receiver) => receiver,
        Err(error) => {
            show_kernel_error(&ui, generation, &error);
            return;
        }
    };

    gtk::glib::MainContext::default().spawn_local(async move {
        let result = crate::background::receive_current(receiver, SNAPSHOT_TIMEOUT, || {
            ui.upgrade()
                .is_some_and(|ui| ui.kernel_refresh_is_current(generation))
        })
        .await;

        work.cancel();

        match result {
            Ok(Ok(snapshot)) => {
                if let Some(ui) = ui.upgrade() {
                    ui.show_kernel_snapshot(generation, snapshot);
                }
            }
            Ok(Err(error)) => show_kernel_error(&ui, generation, &error),
            Err(crate::background::CompletionError::Superseded) => {
                if let Some(ui) = ui.upgrade() {
                    ui.finish_stale_kernel_refresh();
                }
            }
            Err(crate::background::CompletionError::TimedOut) => show_kernel_error(
                &ui,
                generation,
                "The procfs snapshot exceeded the 15-second collection limit",
            ),
            Err(crate::background::CompletionError::Disconnected) => show_kernel_error(
                &ui,
                generation,
                "The background procfs reader stopped before returning a snapshot",
            ),
        }
    });
}

fn show_kernel_error(ui: &Weak<Ui>, generation: u64, error: &str) {
    if let Some(ui) = ui.upgrade() {
        ui.show_kernel_error(generation, error);
    }
}

pub(super) fn request_socket_diagnostics(ui: Weak<Ui>) {
    let Some(current) = ui.upgrade() else {
        return;
    };
    let Some((generation, stamp, request)) = current.begin_socket_diagnostics() else {
        return;
    };

    let timeout = Duration::from_secs(3);
    let work = crate::kernel::WorkDeadline::new(timeout);
    let worker_work = work.clone();

    let receiver = match spawn_kernel_worker("fgdb-socket-diag", move || {
        crate::kernel::sockets::diagnostics::read_on_worker(&request, &worker_work)
    }) {
        Ok(receiver) => receiver,
        Err(error) => {
            current.show_socket_diagnostics(generation, stamp, Err(error));
            return;
        }
    };

    drop(current);

    gtk::glib::MainContext::default().spawn_local(async move {
        let result = crate::background::receive_current(receiver, timeout, || {
            ui.upgrade()
                .is_some_and(|ui| ui.socket_diagnostics_current(generation, stamp))
        })
        .await;

        work.cancel();

        let result = match result {
            Ok(result) => result,
            Err(crate::background::CompletionError::Superseded) => {
                if let Some(ui) = ui.upgrade() {
                    ui.cancel_socket_diagnostics(stamp);
                }
                return;
            }
            Err(crate::background::CompletionError::TimedOut) => {
                Err("The three-second diagnostic collection limit was reached".into())
            }
            Err(crate::background::CompletionError::Disconnected) => {
                Err("The diagnostic worker stopped before returning a result".into())
            }
        };

        if let Some(ui) = ui.upgrade() {
            ui.show_socket_diagnostics(generation, stamp, result);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::{open_debugger_observing, request, wait_until};
    use crate::debugger::{MiEvent, evaluated_value};

    #[test]
    fn kernel_worker_slots_are_bounded_and_released_on_completion_and_panic() {
        let context = gtk::glib::MainContext::new();

        context.block_on(async {
            let mut releases = Vec::new();
            let mut results = Vec::new();

            for _ in 0..MAX_KERNEL_WORKERS {
                let (release, wait) = std::sync::mpsc::channel::<()>();
                releases.push(release);

                results.push(
                    spawn_kernel_worker("fgdb-test", move || {
                        let _ = wait.recv();
                        42
                    })
                    .unwrap(),
                );
            }

            assert!(spawn_kernel_worker("fgdb-test", || ()).is_err());
            drop(releases);

            for result in results {
                assert_eq!(result.await.unwrap(), 42);
            }

            assert_eq!(ACTIVE_KERNEL_WORKERS.load(Ordering::Relaxed), 0);
            let panic = spawn_kernel_worker("fgdb-test", || panic!("test lost reply")).unwrap();
            assert!(panic.await.is_err());
            assert_eq!(ACTIVE_KERNEL_WORKERS.load(Ordering::Relaxed), 0);
        });
    }

    #[test]
    #[ignore = "requires local GDB, procfs and the C networking fixture"]
    fn live_network_snapshots_and_tcp_diagnostics_follow_fixture_phases() {
        let stops = Rc::new(Cell::new(0));
        let observed = Rc::clone(&stops);

        let (debugger, client) = open_debugger_observing(
            "c-network-target",
            "c_network_checkpoint",
            false,
            move |event| {
                if matches!(event, MiEvent::Stopped { .. }) {
                    observed.set(observed.get() + 1);
                }
            },
        );

        let pid =
            crate::debugger::inferior_pid_for_group(&request(&client, "-list-thread-groups"), "i1")
                .unwrap();

        for phase in 0..4 {
            let work = crate::kernel::WorkDeadline::new(Duration::from_secs(15));
            let snapshot = crate::kernel::read_snapshot(pid, debugger.pid(), false, &work).unwrap();
            assert!(
                snapshot.file_descriptors_complete,
                "{:?}",
                snapshot.warnings
            );
            let sockets = snapshot
                .file_descriptors
                .iter()
                .filter(|fd| fd.kind == "socket")
                .collect::<Vec<_>>();

            if phase == 3 {
                assert!(sockets.is_empty());
                assert!(
                    snapshot
                        .file_descriptors
                        .iter()
                        .all(|fd| fd.kind != "epoll")
                );
                break;
            }

            assert!(sockets.len() >= 8, "{sockets:?}");
            assert!(sockets.iter().all(|fd| fd.socket.is_some()), "{sockets:?}");
            let epoll = snapshot
                .file_descriptors
                .iter()
                .find(|fd| fd.kind == "epoll")
                .unwrap();
            assert!(epoll.watches.len() >= 3);

            for watch in &epoll.watches {
                assert!(snapshot.file_descriptors.iter().any(|fd| watch.matches(fd)));
            }

            for index in 0..5 {
                let value = evaluated_value(&request(
                    &client,
                    &format!("-data-evaluate-expression fixture->pairs[{index}].server"),
                ))
                .unwrap();
                let number: i32 = value.parse().unwrap();
                if number < 0 {
                    continue;
                }
                let fd = sockets
                    .iter()
                    .find(|fd| fd.number == number as u32)
                    .unwrap();
                let socket = fd.socket.as_ref().unwrap();

                if let Some(queue) = socket.receive {
                    if phase == 0 {
                        assert!(queue.value > 0, "{socket:?}");
                    }
                    if phase == 1 {
                        assert_eq!(queue.value, 0, "{socket:?}");
                    }
                }

                if phase == 2 && socket.protocol.is_tcp() {
                    assert_eq!(socket.state_name(), "CLOSE_WAIT");
                }
            }

            if phase == 0 {
                let shared_listener = evaluated_value(&request(
                    &client,
                    "-data-evaluate-expression fixture->shared_listener",
                ))
                .unwrap()
                .parse::<u32>()
                .unwrap();

                for fd in &sockets {
                    let socket = fd.socket.as_ref().unwrap();
                    if !socket.protocol.is_tcp() {
                        continue;
                    }

                    let query = crate::kernel::sockets::diagnostics::Request {
                        pid,
                        debugger_pid: debugger.pid(),
                        start_time: snapshot.process_identity().unwrap().1,
                        fd: fd.number,
                        inode: fd.inode.unwrap(),
                        socket: socket.clone(),
                    };

                    let result = std::thread::spawn(move || {
                        crate::kernel::sockets::diagnostics::read_on_worker(
                            &query,
                            &crate::kernel::WorkDeadline::new(Duration::from_secs(3)),
                        )
                    })
                    .join()
                    .unwrap()
                    .unwrap();
                    let wanted = if socket.listening {
                        "Maximum backlog"
                    } else {
                        "RTT"
                    };
                    assert!(result.iter().any(|fact| fact.label == wanted), "{result:?}");

                    if socket.listening {
                        let expected = if fd.number == shared_listener {
                            "7"
                        } else {
                            "1"
                        };
                        assert!(
                            result
                                .iter()
                                .any(|fact| fact.label == wanted && fact.value == expected),
                            "FD {}: {result:?}",
                            fd.number
                        );
                    }
                }
            }

            let before = stops.get();
            assert_eq!(request(&client, "-exec-continue").class, "running");
            wait_until(|| stops.get() > before);
        }
    }
}
