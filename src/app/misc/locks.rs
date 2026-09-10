//! On-demand, stop-scoped lock-address symbol lookup.

use super::*;
use crate::ui::LockSymbolRequest;

pub(in crate::app) fn connect(ui: &Rc<Ui>, client: &Rc<MiClient>) {
    let weak_ui = Rc::downgrade(ui);
    let weak_client = Rc::downgrade(client);

    ui.connect_lock_actions(move |request| {
        if let Some(client) = weak_client.upgrade() {
            resolve_symbol(weak_ui.clone(), &client, request);
        }
    });
}

fn resolve_symbol(ui: Weak<Ui>, client: &MiClient, request: LockSymbolRequest) {
    let Some(requests) = stop_requests(&ui, client, request.generation) else {
        if let Some(ui) = ui.upgrade() {
            ui.show_lock_symbol(request, "Stopped debugger context unavailable".into());
        }

        return;
    };

    let command = symbol_command(request.address);
    let guard = ui.clone();
    let response = ui.clone();

    let result = requests
        .frame(&command)
        .when(move || {
            guard
                .upgrade()
                .is_some_and(|ui| ui.lock_symbol_request_is_current(request))
        })
        .capture(move |_, record, output| {
            if let Some(ui) = response.upgrade() {
                let symbol = if record.is_done() && record.field("fgdb-output-truncated").is_none()
                {
                    output.trim().chars().take(4096).collect::<String>()
                } else {
                    record
                        .error_message()
                        .unwrap_or("Symbol lookup unavailable")
                        .chars()
                        .take(512)
                        .collect()
                };

                ui.show_lock_symbol(
                    request,
                    if symbol.is_empty() {
                        "No symbol returned by GDB".into()
                    } else {
                        symbol
                    },
                );
            }
        });

    if result.is_err()
        && let Some(ui) = ui.upgrade()
    {
        ui.show_lock_symbol(request, "Symbol lookup could not be queued".into());
    }
}

fn symbol_command(address: u64) -> String {
    format!(
        "-interpreter-exec --language c console {}",
        crate::debugger::quote(&format!("info symbol 0x{address:x}"))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::{open_debugger, request as mi, wait_until};
    use crate::debugger::{MiRecord, StopContext};

    #[test]
    #[ignore = "requires Python-enabled GDB and the C lock fixture"]
    fn live_lock_snapshot_reads_words_mappings_and_scoped_symbols() {
        let (debugger, client) = open_debugger("c-misc-locks-target", "c_misc_locks_checkpoint");

        let pid =
            crate::debugger::inferior_pid_for_group(&mi(&client, "-list-thread-groups"), "i1")
                .unwrap();

        let snapshot = crate::misc::read_live_misc(pid, debugger.pid(), true, Default::default())
            .unwrap()
            .locks
            .unwrap();

        assert_eq!(snapshot.waits.len(), 3, "{snapshot:?}");
        assert!(snapshot.warnings.is_empty(), "{snapshot:?}");

        let addresses = snapshot
            .waits
            .iter()
            .filter_map(|wait| wait.address)
            .collect::<HashSet<_>>();

        assert_eq!(addresses.len(), 2);

        for wait in &snapshot.waits {
            assert_eq!(wait.observation.word, Some(0));

            assert_eq!(
                wait.observation.ownership,
                crate::misc::LockOwnership::Unencoded
            );

            assert_eq!(wait.operation_flags, Some(0x80));
            let mapping = wait.observation.mapping.as_ref().unwrap();
            assert!(mapping.start <= wait.address.unwrap() && wait.address.unwrap() < mapping.end);
            assert!(mapping.permissions.contains('w'));
            assert!(mapping.path.contains("c-misc-locks-target"));
        }

        assert!(mi(&client, "-gdb-set language rust").is_done());
        let before = mi(&client, "-gdb-show language").field("value").cloned();
        let valid = Rc::new(Cell::new(true));
        let guard = Rc::clone(&valid);

        let requests = client.bind_stop_requests(
            StopContext::new(
                client.transport_epoch(),
                1,
                Some("i1".into()),
                "1".into(),
                0,
            )
            .unwrap(),
            move |_| guard.get(),
        );

        for address in addresses {
            let response = Rc::new(RefCell::new(None));
            let result = Rc::clone(&response);

            requests
                .frame(&symbol_command(address))
                .capture(move |_, record, output| {
                    result.replace(Some((record, output)));
                })
                .unwrap();

            wait_until(|| response.borrow().is_some());
            let (record, output): (MiRecord, String) = response.take().unwrap();
            assert!(record.is_done(), "{record:?}");
            assert!(output.contains("fixture"), "{output}");
        }

        assert_eq!(
            mi(&client, "-gdb-show language").field("value").cloned(),
            before
        );

        valid.set(false);
        let response = Rc::new(RefCell::new(None));
        let result = Rc::clone(&response);

        requests
            .frame(&symbol_command(snapshot.waits[0].address.unwrap()))
            .capture(move |_, record, _| {
                result.replace(Some(record));
            })
            .unwrap();

        wait_until(|| response.borrow().is_some());
        assert_eq!(response.take().unwrap().class, "superseded");
    }
}
