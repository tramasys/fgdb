use gtk::{gio, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    ffi::OsStr,
    path::Path,
    rc::Rc,
    time::Duration,
};

const OUTPUT_LIMIT: usize = 256 * 1024;
const DIAGNOSTIC_LIMIT: usize = 16 * 1024;
type Ready = Box<dyn FnOnce(Result<(String, String), String>)>;

struct RrScript(gio::File);

impl Drop for RrScript {
    fn drop(&mut self) {
        let file = self.0.clone();
        gio::spawn_blocking(move || {
            let _ = file.delete(None::<&gio::Cancellable>);
        });
    }
}

/// Owns the replay server, its bounded diagnostics and the generated GDB setup.
/// Dropping or stopping a session never leaves its rr server deliberately alive.
pub(super) struct RrServer {
    process: gio::Subprocess,
    helper: RefCell<Option<gio::Subprocess>>,
    script: RefCell<Option<RrScript>>,
    ready: RefCell<Option<Ready>>,
    endpoint: RefCell<Option<String>>,
    stopped: Cell<bool>,
    exited: Rc<Cell<bool>>,
    diagnostics: RefCell<VecDeque<u8>>,
    unexpected_exit: Box<dyn Fn(String)>,
    output_closed: Cell<bool>,
    io_cancel: gio::Cancellable,
    startup_timeout: RefCell<Option<glib::SourceId>>,
}

impl RrServer {
    pub(super) fn start(
        executable: &str,
        trace: &Path,
        ready: impl FnOnce(Result<(String, String), String>) + 'static,
        unexpected_exit: impl Fn(String) + 'static,
    ) -> Result<Rc<Self>, String> {
        if executable.is_empty()
            || executable.contains(['\0', '\r', '\n'])
            || trace.as_os_str().as_encoded_bytes().contains(&0)
        {
            return Err(String::from("Invalid rr executable or trace path"));
        }

        let trace =
            std::path::absolute(trace).map_err(|error| format!("Cannot open rr trace: {error}"))?;
        let flags = gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE;
        let process = gio::Subprocess::newv(
            &[
                OsStr::new(executable),
                OsStr::new("replay"),
                OsStr::new("-s"),
                OsStr::new("0"),
                OsStr::new("-h"),
                OsStr::new("127.0.0.1"),
                OsStr::new("-q"),
                trace.as_os_str(),
            ],
            flags,
        )
        .map_err(|error| {
            format!("Cannot start rr replay: {error}. Configure 'rr' with the rr executable path")
        })?;

        let server = Rc::new(Self {
            process,
            helper: RefCell::new(None),
            script: RefCell::new(None),
            ready: RefCell::new(Some(Box::new(ready))),
            endpoint: RefCell::new(None),
            stopped: Cell::new(false),
            exited: Rc::new(Cell::new(false)),
            diagnostics: RefCell::new(VecDeque::new()),
            unexpected_exit: Box::new(unexpected_exit),
            output_closed: Cell::new(false),
            io_cancel: gio::Cancellable::new(),
            startup_timeout: RefCell::new(None),
        });

        let weak = Rc::downgrade(&server);
        let exited = Rc::clone(&server.exited);

        server
            .process
            .wait_async(None::<&gio::Cancellable>, move |_| {
                exited.set(true);

                if let Some(server) = weak.upgrade() {
                    if server.output_closed.get() {
                        server.report_exit();
                    } else {
                        // Drain pending diagnostics before reporting an exit. Do not
                        // wait indefinitely for a descendant holding the pipe open.
                        glib::timeout_add_local_once(Duration::from_millis(100), move || {
                            if let Some(server) = weak.upgrade() {
                                server.report_exit();
                            }
                        });
                    }
                }
            });

        let weak = Rc::downgrade(&server);
        let stream = server.process.stdout_pipe().expect("rr stdout pipe");
        let output_task = async move {
            let mut pending = Vec::new();
            let mut launch_announced = false;

            loop {
                let bytes = match stream
                    .read_bytes_future(4096, glib::Priority::DEFAULT)
                    .await
                {
                    Ok(bytes) if !bytes.is_empty() => bytes,
                    Ok(_) => break,
                    Err(error) => {
                        if let Some(server) = weak.upgrade() {
                            server.fail(&format!("Cannot read rr diagnostics: {error}"));
                        }

                        return;
                    }
                };

                let Some(server) = weak.upgrade() else { break };

                if server.stopped.get() {
                    break;
                }

                if pending.len().saturating_add(bytes.len()) > OUTPUT_LIMIT {
                    server.fail("rr output exceeded the diagnostic line budget");
                    break;
                }

                pending.extend_from_slice(&bytes);
                let mut consumed = 0;

                for end in pending
                    .iter()
                    .enumerate()
                    .filter_map(|(index, byte)| (*byte == b'\n').then_some(index))
                {
                    // Decode complete lines, so a split UTF-8 character survives
                    // pipe chunk boundaries. Compact the buffer once per read.
                    let line = String::from_utf8_lossy(&pending[consumed..end]);
                    consumed = end + 1;

                    let needs_endpoint = server.endpoint.borrow().is_none();

                    let endpoint = (needs_endpoint && launch_announced)
                        .then(|| replay_endpoint(&line))
                        .flatten();
                    let handshake = endpoint.is_some() || line.trim() == "Launch debugger with";

                    if let Some(endpoint) = endpoint {
                        server.endpoint.replace(Some(endpoint));
                        server.maybe_ready();
                    }

                    launch_announced = line.trim() == "Launch debugger with";

                    if !handshake {
                        append_diagnostic(&mut server.diagnostics.borrow_mut(), &line);
                    }
                }

                pending.drain(..consumed);
            }

            if let Some(server) = weak.upgrade() {
                if !pending.is_empty() {
                    append_diagnostic(
                        &mut server.diagnostics.borrow_mut(),
                        &String::from_utf8_lossy(&pending),
                    );
                }

                server.output_closed.set(true);
                server.report_exit();
            }
        };

        glib::spawn_future_local(gio::CancellableFuture::new(
            output_task,
            server.io_cancel.clone(),
        ));

        let helper = match gio::Subprocess::newv(
            &[OsStr::new(executable), OsStr::new("gdbinit")],
            gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_PIPE,
        ) {
            Ok(helper) => helper,
            Err(error) => {
                server.stop();
                return Err(format!("Cannot load rr's GDB integration: {error}"));
            }
        };

        server.helper.replace(Some(helper.clone()));
        let weak = Rc::downgrade(&server);
        let cancel = server.io_cancel.clone();

        let helper_task = async move {
            let result = read_helper(&helper, &cancel).await;
            let result = match result {
                Ok(bytes) => gio::spawn_blocking(move || write_script(&bytes, &cancel))
                    .await
                    .unwrap_or_else(|_| Err(String::from("Cannot prepare rr's GDB integration"))),
                Err(error) => Err(error),
            };

            let Some(server) = weak.upgrade() else { return };

            if server.stopped.get() {
                return;
            }

            match result {
                Ok(file) => {
                    server.helper.borrow_mut().take();
                    server.script.replace(Some(file));
                    server.maybe_ready();
                }
                Err(error) => server.fail(&error),
            }
        };

        glib::spawn_future_local(gio::CancellableFuture::new(
            helper_task,
            server.io_cancel.clone(),
        ));

        let weak = Rc::downgrade(&server);
        let timeout = glib::timeout_add_local_once(Duration::from_secs(30), move || {
            let Some(server) = weak.upgrade() else { return };
            server.startup_timeout.borrow_mut().take();
            let pending = server.ready.borrow().is_some();

            if pending {
                server.fail("rr did not become ready within 30 seconds. Check the trace, CPU support, and perf_event permissions");
            }
        });

        server.startup_timeout.replace(Some(timeout));
        Ok(server)
    }

    fn maybe_ready(&self) {
        if self.stopped.get() || self.exited.get() {
            return;
        }

        let endpoint = self.endpoint.borrow().clone();
        let script = self
            .script
            .borrow()
            .as_ref()
            .and_then(|script| script.0.path());

        if let (Some(endpoint), Some(script)) = (endpoint, script) {
            let Some(script) = script.to_str() else {
                self.fail("The temporary rr integration path is not valid UTF-8 for GDB");
                return;
            };

            let callback = self.ready.borrow_mut().take();

            if let Some(callback) = callback {
                self.cancel_startup_timeout();
                callback(Ok((endpoint, script.to_owned())));
            }
        }
    }

    fn report_exit(&self) {
        if self.exited.get() && !self.stopped.get() {
            let status = if self.process.has_signaled() {
                format!("signal {}", self.process.term_sig())
            } else {
                format!("status {}", self.process.exit_status())
            };

            let mut message =
                format!("rr replay exited with {status}. Restart GDB to reopen the trace.");
            let mut diagnostics = self.diagnostics.borrow_mut();
            let text = String::from_utf8_lossy(diagnostics.make_contiguous());

            if !text.trim().is_empty() {
                message.push('\n');
                message.push_str(text.trim());
            }

            drop(diagnostics);
            self.fail(&message);
        }
    }

    fn fail(&self, message: &str) {
        if self.stopped.get() {
            return;
        }

        let callback = self.ready.borrow_mut().take();
        self.stop();

        if let Some(callback) = callback {
            callback(Err(message.to_owned()));
        } else {
            (self.unexpected_exit)(message.to_owned());
        }
    }

    pub(super) fn stop(&self) {
        if self.stopped.replace(true) {
            return;
        }

        self.ready.borrow_mut().take();
        self.cancel_startup_timeout();

        // Cancel pipe reads as well as the processes. A descendant can keep an
        // inherited pipe open after rr itself has exited.
        self.io_cancel.cancel();

        if let Some(helper) = self.helper.borrow_mut().take() {
            helper.force_exit();
            helper.wait_async(None::<&gio::Cancellable>, |_| {});
        }

        if !self.exited.get() {
            self.process.send_signal(15);
            let process = self.process.clone();
            let exited = Rc::clone(&self.exited);
            glib::timeout_add_local_once(Duration::from_secs(1), move || {
                if !exited.get() {
                    process.force_exit();
                }
            });
        }
    }

    fn cancel_startup_timeout(&self) {
        if let Some(source) = self.startup_timeout.borrow_mut().take() {
            source.remove();
        }
    }
}

impl Drop for RrServer {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn read_helper(
    process: &gio::Subprocess,
    cancel: &gio::Cancellable,
) -> Result<Vec<u8>, String> {
    let stderr = process
        .stderr_pipe()
        .ok_or("rr gdbinit did not provide a diagnostic stream")?;
    let diagnostic_process = process.clone();
    let diagnostics = glib::spawn_future_local(gio::CancellableFuture::new(
        async move { read_helper_pipe(&diagnostic_process, &stderr, DIAGNOSTIC_LIMIT).await },
        cancel.clone(),
    ));

    let stream = process
        .stdout_pipe()
        .ok_or("rr gdbinit did not provide an output stream")?;
    let output = read_helper_pipe(process, &stream, OUTPUT_LIMIT).await?;
    let diagnostics = diagnostics
        .await
        .map_err(|_| String::from("rr diagnostic reader failed"))?
        .map_err(|_| String::from("rr integration was canceled"))??;

    process.wait_check_future().await.map_err(|error| {
        format!(
            "rr gdbinit failed: {error}\n{}",
            String::from_utf8_lossy(&diagnostics).trim()
        )
    })?;

    if output.is_empty() {
        return Err(String::from("rr returned an empty GDB integration script"));
    }

    Ok(output)
}

async fn read_helper_pipe(
    process: &gio::Subprocess,
    stream: &gio::InputStream,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();

    loop {
        let bytes = stream
            .read_bytes_future(4096, glib::Priority::DEFAULT)
            .await
            .map_err(|error| error.to_string())?;

        if bytes.is_empty() {
            break;
        }

        if output.len().saturating_add(bytes.len()) > limit {
            process.force_exit();
            return Err(String::from(
                "rr's GDB integration output exceeded the size budget",
            ));
        }

        output.extend_from_slice(&bytes);
    }

    Ok(output)
}

fn write_script(bytes: &[u8], cancel: &gio::Cancellable) -> Result<RrScript, String> {
    if cancel.is_cancelled() {
        return Err(String::from("rr integration was canceled"));
    }

    let (file, stream) =
        gio::File::new_tmp(Some("fgdb-rr-XXXXXX")).map_err(|error| error.to_string())?;
    let file = RrScript(file);
    let result = stream.output_stream().write_all(bytes, Some(cancel));
    let close = stream.close(None::<&gio::Cancellable>);

    let result = result.and_then(|(written, error)| match error {
        Some(error) => Err(error),
        None if written == bytes.len() => Ok(()),
        None => Err(glib::Error::new(
            gio::IOErrorEnum::Failed,
            "Incomplete rr integration script write",
        )),
    });

    if let Err(error) = result.and(close) {
        return Err(format!("Cannot prepare rr's GDB integration: {error}"));
    }

    Ok(file)
}

fn append_diagnostic(diagnostics: &mut VecDeque<u8>, line: &str) {
    let line = &line[line.ceil_char_boundary(line.len().saturating_sub(DIAGNOSTIC_LIMIT - 1))..];
    let excess = diagnostics
        .len()
        .saturating_add(line.len() + 1)
        .saturating_sub(DIAGNOSTIC_LIMIT);
    diagnostics.drain(..excess);

    while diagnostics.front().is_some_and(|byte| byte & 0xc0 == 0x80) {
        diagnostics.pop_front();
    }

    diagnostics.extend(line.as_bytes());
    diagnostics.push_back(b'\n');
}

fn replay_endpoint(line: &str) -> Option<String> {
    let words = shell_words::split(line).ok()?;

    for word in &words {
        let Some(endpoint) = word
            .strip_prefix("target extended-remote ")
            .or_else(|| word.strip_prefix("target remote "))
        else {
            continue;
        };

        let port = endpoint.strip_prefix("127.0.0.1:")?.parse::<u16>().ok()?;

        if port != 0 {
            return Some(format!("127.0.0.1:{port}"));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_keep_recent_errors_with_bounded_utf8_storage() {
        let mut diagnostics = VecDeque::new();
        append_diagnostic(&mut diagnostics, &"界".repeat(DIAGNOSTIC_LIMIT));
        assert!(diagnostics.len() <= DIAGNOSTIC_LIMIT);
        append_diagnostic(&mut diagnostics, "fatal: replay failed");
        assert!(
            std::str::from_utf8(diagnostics.make_contiguous())
                .unwrap()
                .ends_with("fatal: replay failed\n")
        );
        assert!(diagnostics.len() <= DIAGNOSTIC_LIMIT);
    }

    #[test]
    fn helper_diagnostics_never_become_gdb_commands_and_survive_failure() {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    for (script, success) in [
                        (
                            "printf '# integration script\\n'; printf 'helper warning\\n' >&2",
                            true,
                        ),
                        ("printf 'helper failed\\n' >&2; exit 1", false),
                    ] {
                        let process = gio::Subprocess::newv(
                            &[OsStr::new("/bin/sh"), OsStr::new("-c"), OsStr::new(script)],
                            gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_PIPE,
                        )
                        .unwrap();

                        let result = read_helper(&process, &gio::Cancellable::new()).await;

                        if success {
                            assert_eq!(result.unwrap(), b"# integration script\n");
                        } else {
                            assert!(result.unwrap_err().contains("helper failed"));
                        }
                    }
                })
            })
            .unwrap();
    }

    #[test]
    fn accepts_only_rr_loopback_endpoints_not_announced_shell_commands() {
        assert_eq!(
            replay_endpoint(
                "  'gdb' '-ex' 'target extended-remote 127.0.0.1:2345' '/path with spaces/app'"
            ),
            Some(String::from("127.0.0.1:2345"))
        );

        for line in [
            "'target extended-remote 0.0.0.0:2345'",
            "'target extended-remote 127.0.0.1:0'",
            "'target extended-remote 127.0.0.1:65536'",
            "'target extended-remote 127.0.0.1:1234;quit'",
        ] {
            assert_eq!(replay_endpoint(line), None);
        }
    }
}
