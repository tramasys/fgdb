//! UI consumers of the general module symbol-resolution service.

use super::*;
use crate::symbols::{BatchSummary, ModuleKey, Outcome, SymbolEvent};

#[derive(Default)]
struct PendingUpdates {
    lifetime: u64,
    scheduled: bool,
    configuration: bool,
    consumers: bool,
    modules: bool,
    keys: HashSet<ModuleKey>,
    notices: Vec<Notice>,
}

enum Notice {
    Progress(String),
    Finished(ModuleKey, Outcome, String),
    Batch(BatchSummary),
}

pub(super) fn connect(
    ui: &Rc<Ui>,
    mi_client: &Rc<MiClient>,
    model: &Rc<crate::model::DebuggerModel>,
) -> Rc<crate::symbols::SymbolResolver> {
    let resolver = crate::symbols::SymbolResolver::new(model, mi_client);
    let observer_ui = Rc::downgrade(ui);
    let observer_client = Rc::downgrade(mi_client);
    let pending = Rc::new(RefCell::new(PendingUpdates::default()));

    let subscription = model.symbols.subscribe(move |event| {
        let Some(ui) = observer_ui.upgrade() else {
            return;
        };

        let lifetime = ui.model.symbols.lifetime();
        let mut updates = pending.borrow_mut();

        if updates.lifetime != lifetime {
            let scheduled = updates.scheduled;
            *updates = PendingUpdates {
                lifetime,
                scheduled,
                ..PendingUpdates::default()
            };
        }

        match event {
            SymbolEvent::Configuration => updates.configuration = true,
            SymbolEvent::Changed(key) => {
                updates.keys.insert(key.clone());

                if ui.model.selected_inferior_id().as_deref() == Some(&key.inferior) {
                    updates.consumers = true;
                }
            }
            SymbolEvent::Updated(key, phase) => {
                updates.keys.insert(key.clone());

                if *phase == crate::symbols::Phase::Checking && updates.notices.len() < 128 {
                    updates.notices.push(Notice::Progress(format!(
                        "Resolving symbols for {}",
                        key.target
                    )));
                }
            }
            SymbolEvent::Finished(key, outcome, message) => {
                updates.keys.insert(key.clone());
                updates.modules = true;

                if updates.notices.len() < 128 {
                    updates
                        .notices
                        .push(Notice::Finished(key.clone(), *outcome, message.clone()));
                }
            }
            SymbolEvent::BatchStarted => {}
            SymbolEvent::BatchFinished(summary) => {
                updates.consumers |= summary.changed;
                updates.modules = true;
                updates.notices.push(Notice::Batch(summary.clone()));
            }
        }

        if updates.scheduled {
            return;
        }

        updates.scheduled = true;
        drop(updates);
        let pending = Rc::clone(&pending);
        let weak = observer_ui.clone();
        let client = observer_client.clone();

        gtk::glib::idle_add_local_once(move || {
            let updates = pending.take();
            let (Some(ui), Some(client)) = (weak.upgrade(), client.upgrade()) else {
                return;
            };

            if ui.model.symbols.lifetime() != updates.lifetime {
                return;
            }

            if updates.configuration {
                ui.refresh_module_debug_metadata(true);
                ui.render_symbol_configuration();
            }

            // Revisions invalidate old variable objects immediately. Rebuilding
            // their consumers can wait until the batch reaches a terminal state.
            if ui.model.symbols.batch_in_progress() {
                let mut deferred = pending.borrow_mut();
                deferred.lifetime = updates.lifetime;
                deferred.consumers |= updates.consumers;
                deferred.modules |= updates.modules;
            } else {
                if updates.modules {
                    refresh_modules(&weak, &client);
                }

                if updates.consumers {
                    ui.invalidate_source_discovery();
                    ui.invalidate_allocator_probe_cache();
                    refresh_breakpoints(&weak, &client);
                    client.refresh_pretty_printer_capabilities();

                    if ui.model.stopped_inspection_available() {
                        refresh_stopped_state(&weak, &client);
                        ui.refresh_misc_after_stop();
                    }
                }
            }

            for notice in updates.notices {
                match notice {
                    Notice::Progress(message) => ui.add_debug_data_progress(message),
                    Notice::Finished(key, outcome, message) => {
                        let message = format!("{}: {message}", key.target);

                        match outcome {
                            Outcome::Loaded => ui.add_debug_data_success(message),
                            Outcome::Failed => ui.add_debug_data_error(message),
                            _ => ui.add_debug_data_warning(message),
                        }
                    }
                    Notice::Batch(summary) => {
                        if summary.failed > 0 || summary.cancelled > 0 || summary.unresolved > 0 {
                            ui.add_debug_data_warning(summary.message());
                        } else {
                            ui.add_debug_data_success(summary.message());
                        }
                    }
                }
            }

            ui.update_symbol_rows(&updates.keys);
        });
    });

    let subscription = RefCell::new(Some(subscription));

    ui.window.connect_destroy(move |_| {
        subscription.borrow_mut().take();
    });

    resolver
}
