use super::*;

struct ForkPolicyRefresh {
    ui: Weak<Ui>,
    generation: u64,
    remaining: u8,
    follow: Option<ForkFollowMode>,
    detach: Option<bool>,
}

pub(super) fn refresh_inferiors(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    let generation = current_ui.start_inferior_refresh();
    drop(current_ui);
    let weak_ui = ui.clone();
    let weak_ui_for_error = ui.clone();
    if client
        .request("-list-thread-groups --recurse 1", move |client, record| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };
            if !ui.is_inferior_refresh_current(generation) {
                return;
            }
            if !record.is_done() {
                ui.set_status(
                    "Inferior refresh failed",
                    record
                        .error_message()
                        .unwrap_or("GDB did not return its thread groups"),
                    Some("status-error"),
                );
                return;
            }
            let previous = ui.selected_inferior_id();
            let current_thread_id = ui.current_thread_id();
            ui.show_inferiors(crate::debugger::inferiors(
                &record,
                current_thread_id.as_deref(),
            ));
            let selection_changed = previous != ui.selected_inferior_id();
            let refresh_selected = selection_changed && ui.selected_inferior_context_stopped();
            drop(ui);
            if selection_changed {
                refresh_modules(&weak_ui, client);
            }
            if refresh_selected {
                refresh_threads(&weak_ui, client);
            }
        })
        .is_err()
        && let Some(ui) = weak_ui_for_error.upgrade()
    {
        ui.set_status(
            "Inferior refresh failed",
            "Could not queue the GDB thread-group query",
            Some("status-error"),
        );
    }
}

pub(super) fn refresh_fork_policy(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    let generation = current_ui.start_fork_policy_refresh();
    drop(current_ui);
    let refresh = Rc::new(RefCell::new(ForkPolicyRefresh {
        ui: ui.clone(),
        generation,
        remaining: 2,
        follow: None,
        detach: None,
    }));
    let refresh_for_response = Rc::clone(&refresh);
    if client
        .request("-gdb-show follow-fork-mode", move |_, record| {
            let mode = record
                .is_done()
                .then(|| record.field("value"))
                .flatten()
                .and_then(|value| value.as_const())
                .and_then(|value| match value {
                    "parent" => Some(ForkFollowMode::Parent),
                    "child" => Some(ForkFollowMode::Child),
                    _ => None,
                });
            complete_fork_policy_refresh(&refresh_for_response, Some(mode), None);
        })
        .is_err()
    {
        complete_fork_policy_refresh(&refresh, Some(None), None);
    }
    let refresh_for_response = Rc::clone(&refresh);
    if client
        .request("-gdb-show detach-on-fork", move |_, record| {
            let detach = record
                .is_done()
                .then(|| record.field("value"))
                .flatten()
                .and_then(|value| value.as_const())
                .and_then(|value| match value {
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => None,
                });
            complete_fork_policy_refresh(&refresh_for_response, None, Some(detach));
        })
        .is_err()
    {
        complete_fork_policy_refresh(&refresh, None, Some(None));
    }
}

fn complete_fork_policy_refresh(
    refresh: &Rc<RefCell<ForkPolicyRefresh>>,
    follow: Option<Option<ForkFollowMode>>,
    detach: Option<Option<bool>>,
) {
    let finished = {
        let mut refresh = refresh.borrow_mut();
        if let Some(follow) = follow {
            refresh.follow = follow;
        }
        if let Some(detach) = detach {
            refresh.detach = detach;
        }
        refresh.remaining = refresh.remaining.saturating_sub(1);
        (refresh.remaining == 0).then(|| {
            (
                refresh.ui.clone(),
                refresh.generation,
                refresh.follow,
                refresh.detach,
            )
        })
    };
    let Some((ui, generation, follow, detach)) = finished else {
        return;
    };
    if let Some(ui) = ui
        .upgrade()
        .filter(|ui| ui.is_fork_policy_refresh_current(generation))
    {
        ui.set_fork_policy(follow, detach);
    }
}

pub(super) fn handle_inferior_action(ui: Weak<Ui>, client: Rc<MiClient>, action: InferiorAction) {
    if !ui
        .upgrade()
        .is_some_and(|ui| ui.inferior_action_is_current(&action))
    {
        return;
    }
    match action {
        InferiorAction::Select(id) => select_inferior(ui, client, id),
        InferiorAction::Resume(id) => execute_inferior(ui, client, id, true),
        InferiorAction::Interrupt(id) => execute_inferior(ui, client, id, false),
        InferiorAction::SetFollowFork(mode) => {
            set_fork_setting(
                ui,
                client,
                "follow-fork-mode",
                mode.gdb_value(),
                move |ui| {
                    ui.set_fork_follow_mode(Some(mode));
                    ui.set_status(
                        "Fork policy updated",
                        &format!(
                            "GDB will follow the {} process after a fork",
                            mode.gdb_value()
                        ),
                        Some("status-ready"),
                    );
                },
            );
        }
        InferiorAction::SetDetachOnFork(detach) => {
            let value = if detach { "on" } else { "off" };
            set_fork_setting(ui, client, "detach-on-fork", value, move |ui| {
                ui.set_detach_on_fork(Some(detach));
                ui.set_status(
                    "Fork policy updated",
                    if detach {
                        "GDB will detach the process it does not follow"
                    } else {
                        "GDB will retain both parent and child as inferiors"
                    },
                    Some("status-ready"),
                );
            });
        }
        InferiorAction::Refresh => {
            refresh_inferiors(&ui, &client);
            refresh_fork_policy(&ui, &client);
            if ui.upgrade().is_some_and(|ui| !ui.inferior_is_running()) {
                refresh_threads(&ui, &client);
                refresh_modules(&ui, &client);
            }
        }
    }
}

fn select_inferior(ui: Weak<Ui>, client: Rc<MiClient>, id: String) {
    let Some(number) = gdb_inferior_number(&id) else {
        if let Some(ui) = ui.upgrade() {
            ui.set_status(
                "Inferior selection unavailable",
                &format!("GDB reported an unsupported inferior identifier: {id}"),
                Some("status-error"),
            );
        }
        return;
    };
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    if current_ui.selected_inferior_id().as_deref() == Some(id.as_str()) {
        return;
    }
    current_ui.set_inferior_action_pending(Some(InferiorActionPending::Selection));
    current_ui.set_status("Switching inferior", &format!("Selecting {id}"), None);
    drop(current_ui);
    let command = format!(
        "-interpreter-exec console {}",
        crate::debugger::quote(&format!("inferior {number}"))
    );
    let weak_ui = ui.clone();
    let weak_ui_for_error = ui.clone();
    if client
        .request(&command, move |client, record| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };
            if !record.is_success() {
                ui.clear_inferior_action_pending();
                ui.set_status(
                    "Inferior selection failed",
                    record
                        .error_message()
                        .unwrap_or("GDB rejected the selected inferior"),
                    Some("status-error"),
                );
                return;
            }
            ui.set_selected_inferior(&id);
            ui.clear_inferior_action_pending();
            ui.set_status(
                "Inferior selected",
                &format!("Threads, modules, and stopped state now follow {id}"),
                Some("status-ready"),
            );
            let stopped = ui.selected_inferior_context_stopped();
            drop(ui);
            refresh_inferiors(&weak_ui, client);
            refresh_modules(&weak_ui, client);
            detect_target_abi(&weak_ui, client);
            if stopped {
                refresh_stopped_state(&weak_ui, client);
            }
        })
        .is_err()
        && let Some(ui) = weak_ui_for_error.upgrade()
    {
        ui.clear_inferior_action_pending();
        ui.set_status(
            "Inferior selection failed",
            "Could not queue the GDB inferior command",
            Some("status-error"),
        );
    }
}

fn execute_inferior(ui: Weak<Ui>, client: Rc<MiClient>, id: String, resume: bool) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    let Some(group_id) = crate::debugger::thread_group_argument(&id) else {
        current_ui.set_status(
            "Inferior control unavailable",
            &format!("GDB reported an unsupported inferior identifier: {id}"),
            Some("status-error"),
        );
        return;
    };
    let execution_generation = current_ui.begin_inferior_execution_action(id.clone());
    current_ui.set_status(
        if resume {
            "Resuming inferior"
        } else {
            "Freezing inferior"
        },
        &id,
        Some("status-running"),
    );
    drop(current_ui);
    let command = format!(
        "{} --thread-group {}",
        if resume {
            "-exec-continue"
        } else {
            "-exec-interrupt"
        },
        group_id
    );
    let weak_ui = ui.clone();
    let weak_ui_for_error = ui.clone();
    let request = client.request(&command, move |client, record| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };
            if record.class == "timeout" {
                client.quarantine(
                    "GDB did not answer the process-level execution command within 30 seconds. The inferior state can no longer be determined safely.",
                );
            } else if !record.is_success() {
                ui.set_pending_execution_inferior(None);
                ui.clear_inferior_action_pending();
                ui.set_status(
                    if resume {
                        "Inferior resume failed"
                    } else {
                        "Inferior freeze failed"
                    },
                    record
                        .error_message()
                        .unwrap_or("GDB rejected the process-level execution command"),
                    Some("status-error"),
                );
            }
        });
    if request.is_err() {
        if let Some(ui) = weak_ui_for_error.upgrade() {
            ui.clear_inferior_action_pending();
            ui.set_pending_execution_inferior(None);
            ui.set_status(
                "Inferior control failed",
                "Could not queue the process-level execution command",
                Some("status-error"),
            );
        }
    } else {
        let weak_ui = ui;
        let weak_client = Rc::downgrade(&client);
        gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(15), move || {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };
            if ui.inferior_execution_action_pending_for(&id, execution_generation) {
                let message = "GDB accepted a process-level execution command but did not report a running or stopped transition within 15 seconds. Restart GDB from the Session menu.";
                if let Some(client) = weak_client.upgrade() {
                    client.quarantine(message);
                } else {
                    ui.require_gdb_recovery("GDB recovery required", message);
                }
            }
        });
    }
}

fn set_fork_setting(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    setting: &'static str,
    value: &'static str,
    applied: impl Fn(&Ui) + 'static,
) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    current_ui.start_fork_policy_refresh();
    current_ui.set_inferior_action_pending(Some(InferiorActionPending::Setting));
    drop(current_ui);
    let command = format!("-gdb-set {setting} {value}");
    let weak_ui = ui.clone();
    let weak_ui_for_error = ui.clone();
    if client
        .request(&command, move |_, record| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };
            if record.is_done() {
                applied(&ui);
                ui.clear_inferior_action_pending();
            } else {
                ui.clear_inferior_action_pending();
                ui.set_status(
                    "Fork policy update failed",
                    record
                        .error_message()
                        .unwrap_or("GDB rejected the fork setting"),
                    Some("status-error"),
                );
            }
        })
        .is_err()
        && let Some(ui) = weak_ui_for_error.upgrade()
    {
        ui.clear_inferior_action_pending();
        ui.set_status(
            "Fork policy update failed",
            "Could not queue the GDB setting command",
            Some("status-error"),
        );
    }
}

fn gdb_inferior_number(group_id: &str) -> Option<u64> {
    group_id.strip_prefix('i')?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::gdb_inferior_number;

    #[test]
    fn converts_native_gdb_thread_group_identifiers() {
        assert_eq!(gdb_inferior_number("i1"), Some(1));
        assert_eq!(gdb_inferior_number("i42"), Some(42));
        assert_eq!(gdb_inferior_number("process-1"), None);
    }
}
