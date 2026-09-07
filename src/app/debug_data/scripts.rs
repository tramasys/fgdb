//! Backend-scoped loading of trusted user scripts. Filesystem validation runs
//! outside GTK, while GDB execution uses the shared guarded console queue.

use super::*;

pub(super) fn load(ui: Weak<Ui>, client: Rc<MiClient>, path: PathBuf) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    if !client.is_ready() {
        current_ui.add_debug_data_warning("GDB is not ready to load a pretty-printer script");
        return;
    }

    let path = if path.is_absolute() || path.as_os_str().is_empty() {
        path
    } else {
        current_ui
            .model
            .current_session()
            .as_ref()
            .and_then(DebugSession::working_directory)
            .map_or(path.clone(), |directory| directory.join(path))
    };

    let request = match current_ui.begin_pretty_printer_script_load() {
        Ok(request) => request,
        Err(error) => {
            current_ui.add_debug_data_warning(error);
            return;
        }
    };

    current_ui.add_debug_data_progress(format!("Loading pretty-printer script {}", path.display()));

    Rc::new(PrinterScriptLoad {
        ui,
        client: Rc::downgrade(&client),
        epoch: client.transport_epoch(),
        request,
    })
    .validate(path);
}

struct PrinterScriptLoad {
    ui: Weak<Ui>,
    client: Weak<MiClient>,
    epoch: u64,
    request: crate::model::printers::PrinterLoadId,
}

impl PrinterScriptLoad {
    fn backend(&self) -> Option<Rc<MiClient>> {
        let client = self.client.upgrade()?;

        (client.is_ready()
            && client.transport_epoch() == self.epoch
            && self
                .ui
                .upgrade()
                .is_some_and(|ui| ui.model.printer_scripts.borrow().is_current(self.request)))
        .then_some(client)
    }

    fn cancel(&self) {
        if let Some(ui) = self.ui.upgrade() {
            ui.finish_pretty_printer_script_load(self.request, None);
        }
    }

    fn fail(&self, message: String) {
        if let Some(ui) = self.ui.upgrade()
            && ui.finish_pretty_printer_script_load(self.request, None)
        {
            ui.add_debug_data_error(message);
        }
    }

    fn validate(self: Rc<Self>, path: PathBuf) {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc::{self, TryRecvError},
        };

        let (sender, receiver) = mpsc::sync_channel(1);
        let active = Arc::new(AtomicBool::new(true));
        let active_for_worker = Arc::clone(&active);
        let candidate = path.clone();

        if let Err(error) = crate::background::submit_cancellable_with_priority(
            crate::background::Priority::Interactive,
            move || active_for_worker.load(Ordering::Acquire),
            move || {
                let result = crate::language::scripts::PrinterScript::resolve(&candidate);
                let _ = sender.send(result);
            },
        ) {
            self.fail(format!("Could not validate pretty-printer script: {error}"));
            return;
        }

        let started = std::time::Instant::now();

        gtk::glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
            if self.backend().is_none() {
                active.store(false, Ordering::Release);
                self.cancel();
                return gtk::glib::ControlFlow::Break;
            }

            let result = match receiver.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty)
                    if started.elapsed() < std::time::Duration::from_secs(10) =>
                {
                    return gtk::glib::ControlFlow::Continue;
                }
                Err(TryRecvError::Empty) => Err(String::from("filesystem validation timed out")),
                Err(TryRecvError::Disconnected) => {
                    Err(String::from("filesystem validation stopped"))
                }
            };

            active.store(false, Ordering::Release);

            match result {
                Ok(script) => Rc::clone(&self).source(script),
                Err(error) => self.fail(format!(
                    "Could not open pretty-printer script '{}': {error}",
                    path.display(),
                )),
            }

            gtk::glib::ControlFlow::Break
        });
    }

    fn source(self: Rc<Self>, script: crate::language::scripts::PrinterScript) {
        let Some(client) = self.backend() else {
            self.cancel();
            return;
        };

        if let Some(ui) = self.ui.upgrade() {
            let loaded = ui.model.printer_scripts.borrow().contains(script.path());

            if loaded {
                ui.finish_pretty_printer_script_load(self.request, None);
                ui.add_debug_data_warning(
                    "This pretty-printer script is already loaded for the current GDB session",
                );
                return;
            }
        }

        let guard = Rc::clone(&self);
        let response = Rc::clone(&self);
        let path = script.path().to_path_buf();

        if let Err(error) = client.request_console_when(
            script.command(),
            move || guard.backend().is_some(),
            move |_, record, output| {
                let Some(client) = response.backend() else {
                    response.cancel();
                    return;
                };

                let Some(ui) = response.ui.upgrade() else {
                    return;
                };

                note_console_truncation(&ui, &record, "Pretty-printer load output");

                if record.is_done() {
                    ui.finish_pretty_printer_script_load(response.request, Some(path.clone()));
                    let output = output.trim();
                    let message = if output.is_empty() {
                        format!("Loaded pretty-printer script {}", path.display())
                    } else {
                        format!("Loaded pretty-printer script {}\n{output}", path.display())
                    };

                    ui.add_debug_data_success(message);
                    client.refresh_pretty_printer_capabilities();
                    request_pretty_printers(Rc::downgrade(&ui), client);
                } else {
                    response.fail(format!(
                        "Could not load pretty-printer script {}: {}",
                        path.display(),
                        console_error(&record, &output),
                    ));
                }
            },
        ) {
            self.fail(format!("Could not queue pretty-printer script: {error}"));
        }
    }
}
