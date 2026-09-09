//! Real-GDB test harness shared by inspector regression tests.

use crate::debugger::{MiClient, MiEvent, MiRecord};
use gtk::glib;
use std::{
    cell::{Cell, RefCell},
    process::{Child, Command, Stdio},
    rc::Rc,
    time::{Duration, Instant},
};

pub(super) struct Debugger(Child);

impl Debugger {
    pub(super) fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Debugger {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(super) fn wait_until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    let main = glib::MainContext::default();

    while !done() {
        assert!(Instant::now() < deadline, "Timed out waiting for GDB");
        main.block_on(glib::timeout_future(Duration::from_millis(5)));
    }
}

pub(super) fn request(client: &MiClient, command: &str) -> MiRecord {
    let result = Rc::new(RefCell::new(None));
    let response = Rc::clone(&result);
    client
        .request(command, move |_, record| {
            response.replace(Some(record));
        })
        .unwrap();

    wait_until(|| result.borrow().is_some());
    result.take().unwrap()
}

pub(super) fn open_debugger(fixture: &str, checkpoint: &str) -> (Debugger, Rc<MiClient>) {
    open_debugger_with_printers(fixture, checkpoint, false)
}

pub(super) fn open_debugger_with_printers(
    fixture: &str,
    checkpoint: &str,
    rust_printers: bool,
) -> (Debugger, Rc<MiClient>) {
    let stopped = Rc::new(Cell::new(false));
    let stopped_event = Rc::clone(&stopped);
    let client = MiClient::open(move |_, event| {
        if matches!(event, MiEvent::Stopped { .. }) {
            stopped_event.set(true);
        }
    })
    .unwrap();

    let mut command = Command::new("gdb");

    if rust_printers {
        let toolchain = crate::language::toolchain::RustToolchain::discover(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
            Duration::from_secs(5),
        )
        .unwrap();

        let arguments = toolchain.gdb_printer_arguments();
        assert!(!arguments.is_empty(), "{toolchain:?}");
        command.args(arguments);
    }

    let debugger = Debugger(
        command
            .args([
                "--nx",
                "--quiet",
                "-ex",
                "set pagination off",
                "-ex",
                "set debuginfod enabled off",
                "-ex",
            ])
            .arg(format!("new-ui mi2 {}", client.slave_path().display()))
            .arg(format!(
                "{}/target/debug-fixtures/{fixture}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    wait_until(|| client.is_ready());
    assert!(request(&client, &crate::language::python::install_command()).is_done());
    if rust_printers {
        let verification = request(
            &client,
            &crate::debugger::console_command(
                "python import gdb; assert any(getattr(printer, 'name', '') == 'rust' for objfile in gdb.objfiles() for printer in objfile.pretty_printers)",
            ),
        );

        assert!(verification.is_done(), "{verification:?}");
    }

    assert!(request(&client, "-enable-pretty-printing").is_done());
    assert!(request(&client, &format!("-break-insert {checkpoint}")).is_done());
    assert_eq!(request(&client, "-exec-run").class, "running");
    wait_until(|| stopped.get());
    (debugger, client)
}
