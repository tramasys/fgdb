//! Signal enrichment is explicit and pinned to the originating stopped thread.

use super::*;
use crate::model::stop_info::StopEntry;

const SIGNAL_SCRIPT: &str = r#"import gdb
with gdb.with_parameter('language', 'c'):
 s=gdb.parse_and_eval('$_siginfo')
 code=int(s['si_code'])
 address='-'
 if code > 0 and fault_signal:
  try: address=hex(int(s['si_addr']))
  except (gdb.error, RuntimeError):
   try: address=hex(int(s['_sifields']['_sigfault']['si_addr']))
   except (gdb.error, RuntimeError): pass
 gdb.write('FGDB_SIGNAL '+str(code)+' '+address+'\n')
"#;

pub(super) fn connect(ui: &Rc<Ui>, client: &Rc<MiClient>) {
    let weak = Rc::downgrade(ui);
    let client = Rc::clone(client);

    ui.connect_stop_info(move |entry, target| {
        inspect_signal(weak.clone(), &client, entry, target);
    });
}

fn inspect_signal(
    ui: Weak<Ui>,
    client: &MiClient,
    entry: Rc<StopEntry>,
    target: gtk::glib::WeakRef<gtk::Box>,
) {
    let Some(current_ui) = ui.upgrade().filter(|ui| ui.stop_entry_is_current(&entry)) else {
        return;
    };

    let generation = current_ui.model.current_stop_refresh_generation();
    let Some(requests) = stop_requests(&ui, client, generation) else {
        return;
    };

    let fault_signal = matches!(
        entry.signal.as_deref(),
        Some("SIGSEGV" | "SIGBUS" | "SIGILL" | "SIGFPE" | "SIGTRAP")
    );

    let script = format!(
        "fault_signal={}\n{SIGNAL_SCRIPT}",
        if fault_signal { "True" } else { "False" }
    );

    let command = crate::debugger::console_command(&format!(
        "python exec({}, {{}})",
        crate::debugger::quote(&script)
    ));

    let guard_ui = ui.clone();
    let guard_entry = Rc::clone(&entry);
    let response_ui = ui.clone();
    let response_entry = Rc::clone(&entry);
    let response_target = target.clone();

    let result = requests
        .frame(&command)
        .when(move || {
            guard_ui
                .upgrade()
                .is_some_and(|ui| ui.stop_entry_is_current(&guard_entry))
        })
        .capture(move |_, record, output| {
            if let (Some(ui), Some(target)) = (response_ui.upgrade(), response_target.upgrade()) {
                let result = if record.is_done() && record.field("fgdb-output-truncated").is_none()
                {
                    parse_signal(&output)
                        .ok_or_else(|| "Signal information is unavailable on this target".into())
                } else {
                    Err(record
                        .error_message()
                        .unwrap_or("Signal inspection failed")
                        .to_owned())
                };

                ui.show_stop_signal(response_entry, &target, result);
            }
        });

    if let Err(error) = result
        && let (Some(ui), Some(target)) = (ui.upgrade(), target.upgrade())
    {
        ui.show_stop_signal(entry, &target, Err(error.to_string()));
    }
}

fn parse_signal(output: &str) -> Option<(i64, Option<u64>)> {
    let mut records = output
        .lines()
        .filter_map(|line| line.strip_prefix("FGDB_SIGNAL "));
    let line = records.next()?;

    if records.next().is_some() {
        return None;
    }

    let (code, address) = line.split_once(' ')?;
    let address = if address == "-" {
        None
    } else {
        Some(u64::from_str_radix(address.strip_prefix("0x")?, 16).ok()?)
    };

    Some((code.parse().ok()?, address))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_parser_preserves_null_and_rejects_malformed_evidence() {
        assert_eq!(parse_signal("FGDB_SIGNAL 1 0x0\n"), Some((1, Some(0))));
        assert_eq!(parse_signal("FGDB_SIGNAL -6 -\n"), Some((-6, None)));
        assert_eq!(parse_signal("FGDB_SIGNAL 1 bogus\n"), None);
        assert_eq!(parse_signal("FGDB_SIGNAL 1 0x1\nFGDB_SIGNAL 2 -\n"), None);
    }

    #[test]
    #[ignore = "requires Python-enabled GDB and the C variable-location and core fixtures"]
    fn live_signal_reads_null_fault_without_inventing_an_address_for_raise() {
        use std::{process::Command, time::Duration};

        for (fixture, commands, fault) in [
            (
                "c-variable-location-target",
                vec![
                    "break location_checkpoint",
                    "run",
                    "set var pointer=0",
                    "continue",
                ],
                true,
            ),
            ("c-misc-core-target", vec!["run"], false),
        ] {
            let executable = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/debug-fixtures")
                .join(fixture);
            let mut command = Command::new("gdb");
            command
                .args(["--nx", "--batch", "-ex", "set debuginfod enabled off"])
                .arg(executable);

            for action in commands {
                command.arg("-ex").arg(action);
            }

            command.arg("-ex").arg(format!(
                "python exec({}, {{}})",
                crate::debugger::quote(&format!("fault_signal=True\n{SIGNAL_SCRIPT}"))
            ));
            let output =
                crate::bounded::process::output(&mut command, Duration::from_secs(15)).unwrap();
            let output = String::from_utf8(output).unwrap();
            let (code, address) = parse_signal(&output).unwrap_or_else(|| panic!("{output}"));

            if fault {
                assert!(code > 0);
                assert_eq!(address, Some(0));
            } else {
                assert!(code <= 0);
                assert_eq!(address, None);
            }
        }
    }
}
