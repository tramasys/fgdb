use super::*;

pub(super) fn handle_mi_event(weak_ui: &Weak<Ui>, client: &MiClient, event: MiEvent) {
    let Some(ui) = weak_ui.upgrade() else {
        return;
    };

    match event {
        MiEvent::Ready(capabilities) => {
            ui.set_command_pending(false);
            ui.set_debug_state_stale(false);
            ui.set_gdb_recovery_available(false);
            ui.set_gdb_capabilities(capabilities.clone());
            ui.clear_gef_capabilities();
            ui.invalidate_allocator_probe_cache();
            ui.reset_target_abi();
            let detail = if !capabilities.mi_async {
                "GDB is ready in compatibility mode. It did not accept asynchronous MI mode."
            } else if !capabilities.pretty_printing {
                "GDB is ready. Dynamic C++ and Rust pretty printing is unavailable in this build."
            } else if !capabilities.features_known {
                "GDB is ready. It did not expose an MI feature list, so optional commands use compatibility defaults."
            } else {
                "The native controls and terminal share one GDB process."
            };
            ui.set_status("Ready", detail, Some("status-ready"));
            ui.set_controls_ready(true);
            detect_target_abi(weak_ui, client);
            detect_gef(weak_ui, client);
            request_initial_source(weak_ui, client);
            refresh_breakpoints(weak_ui, client);
            ui.take_modules_dirty();
            refresh_modules(weak_ui, client);
        }
        MiEvent::InferiorStarted => {
            // A terminal user can load and run a different executable in the
            // same GDB process. Register-number caches are target-specific and
            // must not leak across that boundary; the stopped-state refresh
            // will establish the new ABI from GDB and the traced ELF.
            ui.reset_target_abi();
            ui.invalidate_allocator_probe_cache();
            ui.set_inferior_started(true);
        }
        MiEvent::Running => {
            // Preserve the disabled state across the pending -> running
            // transition. Clearing `pending` first briefly made every stop-only
            // control sensitive before `running` disabled it again.
            ui.set_controls_running(true);
            ui.set_command_pending(false);
            if ui.native_until_active() {
                return;
            }
            ui.set_debug_state_stale(true);
            ui.set_inferior_started(true);
            ui.set_thread_stop_reason(None);
            // Any queued stop-state responses now describe the previous stop.
            // Invalidating them also prevents recursive pointer enrichment from
            // issuing more MI work while the inferior is running.
            ui.start_stop_refresh();
            ui.start_thread_refresh();
            ui.invalidate_kernel_refresh();
            ui.invalidate_misc_refresh();
            // Keep the last source tab identity stable while a short execution
            // command is in flight. The stale line marker is removed, while a
            // subsequent stop atomically moves the active-source decoration.
            ui.suspend_execution_location();
            ui.set_execution_status(
                "Running",
                "The inferior is running. Pause it to inspect state.",
            );
        }
        MiEvent::Stopped {
            reason,
            signal_name,
            signal_meaning,
            address,
            thread_id,
        } => {
            ui.set_command_pending(false);
            ui.set_current_thread_id(thread_id.as_deref());
            // The preceding *running event marks the inferior as running, but
            // stopped-state queries intentionally refuse to run in that state.
            // Clear it before populating context, source marks, registers and
            // stack data.
            ui.set_controls_running(false);
            if ui.handle_native_until_stop(
                reason.as_deref(),
                address.as_deref(),
                thread_id.as_deref(),
            ) {
                return;
            }
            drop(ui);
            finish_stopped_state(weak_ui, client, reason, signal_name, signal_meaning, None);
        }
        MiEvent::BreakpointsChanged => refresh_breakpoints(weak_ui, client),
        MiEvent::ThreadsChanged => {
            if !ui.inferior_is_running() && !ui.native_until_active() {
                refresh_threads(weak_ui, client);
            }
        }
        MiEvent::LibrariesChanged => {
            ui.invalidate_allocator_probe_cache();
            ui.mark_modules_dirty();
            if !ui.inferior_is_running() && !ui.native_until_active() && ui.take_modules_dirty() {
                refresh_modules(weak_ui, client);
            }
        }
        MiEvent::SelectionChanged => {
            if !ui.inferior_is_running() && !ui.native_until_active() {
                refresh_stopped_state(weak_ui, client);
            }
        }
        MiEvent::Error(message) => {
            if ui.native_until_active() {
                ui.cancel_native_until();
            }
            ui.set_command_pending(false);
            ui.set_status("Command failed", &message, Some("status-error"));
        }
        MiEvent::Disconnected => {
            ui.set_command_pending(false);
            ui.finish_full_resynchronization();
            ui.set_debug_state_stale(true);
            ui.clear_gef_capabilities();
            ui.clear_gdb_capabilities();
            ui.set_inferior_started(false);
            ui.reset_target_abi();
            ui.clear_debugger_state();
            ui.set_status(
                "Disconnected",
                "The GDB/MI channel was closed.",
                Some("status-error"),
            );
            ui.set_controls_ready(false);
        }
    }
}

pub(super) fn finish_stopped_state(
    weak_ui: &Weak<Ui>,
    client: &MiClient,
    reason: Option<String>,
    signal_name: Option<String>,
    signal_meaning: Option<String>,
    status_detail: Option<String>,
) {
    let Some(ui) = weak_ui.upgrade() else {
        return;
    };
    ui.set_command_pending(false);
    ui.set_debug_state_stale(false);
    ui.set_controls_running(false);
    let reason = reason.unwrap_or_else(|| String::from("stopped"));
    ui.set_thread_stop_reason(Some(&reason));
    if reason.starts_with("exited") {
        ui.set_inferior_started(false);
        ui.clear_debugger_state();
        refresh_breakpoints(weak_ui, client);
    } else {
        ui.set_inferior_started(true);
        refresh_stopped_state(weak_ui, client);
    }
    if ui.take_modules_dirty() {
        refresh_modules(weak_ui, client);
    }
    ui.show_signal(signal_name.as_deref(), signal_meaning.as_deref());
    let detail =
        status_detail.unwrap_or_else(|| format!("GDB reported: {}", reason.replace('-', " ")));
    ui.set_status("Paused", &detail, Some("status-ready"));
}

pub(super) fn detect_target_abi(ui: &Weak<Ui>, client: &MiClient) {
    let weak_ui = ui.clone();
    if client
        .request("-gdb-show architecture", move |client, record| {
            let description = record
                .is_done()
                .then(|| record.field("value"))
                .flatten()
                .and_then(|value| value.as_const());
            let architecture = description
                .map(crate::debugger::TargetArchitecture::from_gdb_description)
                .unwrap_or_default();
            if let Some(ui) = weak_ui.upgrade() {
                ui.set_target_architecture(architecture);
                if let Some(bits) = description.and_then(
                    crate::debugger::TargetArchitecture::pointer_bits_from_gdb_description,
                ) {
                    ui.set_target_pointer_bits(bits);
                }
                if let Some(endian) = description
                    .and_then(crate::debugger::TargetEndian::from_architecture_description)
                {
                    ui.set_target_endian(Some(endian));
                }
            }
            detect_target_pointer_width(&weak_ui, client);
        })
        .is_err()
    {
        if let Some(ui) = ui.upgrade() {
            ui.set_target_architecture(TargetArchitecture::Unknown);
        }
        detect_target_pointer_width(ui, client);
    }
}

fn detect_target_pointer_width(ui: &Weak<Ui>, client: &MiClient) {
    let weak_ui = ui.clone();
    if client
        .request(
            "-data-evaluate-expression sizeof(void*)",
            move |client, record| {
                let bytes = record
                    .is_done()
                    .then(|| crate::debugger::evaluated_value(&record))
                    .flatten()
                    .and_then(|value| parse_pointer_size(&value));
                if let (Some(ui), Some(bytes)) = (weak_ui.upgrade(), bytes) {
                    ui.set_target_pointer_bits(bytes.saturating_mul(8));
                }
                detect_target_endian(&weak_ui, client);
            },
        )
        .is_err()
    {
        detect_target_endian(ui, client);
    }
}

fn parse_pointer_size(value: &str) -> Option<u32> {
    let value = value.split_whitespace().next()?.trim();
    value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .map_or_else(
            || value.parse::<u32>().ok(),
            |digits| u32::from_str_radix(digits, 16).ok(),
        )
        .filter(|bytes| matches!(bytes, 4 | 8))
}

fn detect_target_endian(ui: &Weak<Ui>, client: &MiClient) {
    let weak_ui = ui.clone();
    if client
        .request("-gdb-show endian", move |client, record| {
            let endian = record
                .is_done()
                .then(|| record.field("value"))
                .flatten()
                .and_then(|value| value.as_const())
                .and_then(crate::debugger::TargetEndian::from_gdb_description);
            if let Some(ui) = weak_ui.upgrade()
                && (endian.is_some() || ui.target_endian().is_none())
            {
                ui.set_target_endian(endian);
            }
            refresh_after_target_abi_detection(&weak_ui, client);
        })
        .is_err()
    {
        refresh_after_target_abi_detection(ui, client);
    }
}

fn refresh_after_target_abi_detection(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    let started = current_ui.inferior_has_started();
    let resynchronized = current_ui.finish_full_resynchronization();
    drop(current_ui);
    if started {
        refresh_stopped_state(ui, client);
    } else {
        infer_initial_stop_reason(ui, client);
    }
    if resynchronized && let Some(ui) = ui.upgrade() {
        ui.set_status(
            "Paused",
            "Debugger state was re-read from GDB.",
            Some("status-ready"),
        );
    }
}

fn detect_gef(ui: &Weak<Ui>, client: &MiClient) {
    let weak_ui = ui.clone();
    if client
        .request("-complete gef", move |client, record| {
            if crate::debugger::has_exact_command_completion(&record, "gef") {
                detect_gef_capabilities(weak_ui, client);
            } else if let Some(ui) = weak_ui.upgrade() {
                ui.clear_gef_capabilities();
            }
        })
        .is_err()
        && let Some(ui) = ui.upgrade()
    {
        ui.clear_gef_capabilities();
    }
}

struct GefCapabilityDiscovery {
    ui: Weak<Ui>,
    next: usize,
    available: HashSet<&'static str>,
}

fn detect_gef_capabilities(ui: Weak<Ui>, client: &MiClient) {
    let capabilities = crate::ui::GEF_COMMAND_CAPABILITIES;
    if capabilities.is_empty() {
        if let Some(ui) = ui.upgrade() {
            ui.show_gef_capabilities(&HashSet::new());
        }
        return;
    }

    let discovery = Rc::new(RefCell::new(GefCapabilityDiscovery {
        ui,
        next: 0,
        available: HashSet::with_capacity(capabilities.len()),
    }));
    probe_next_gef_capability(client, discovery);
}

fn probe_next_gef_capability(client: &MiClient, discovery: Rc<RefCell<GefCapabilityDiscovery>>) {
    loop {
        let capability = {
            let mut discovery = discovery.borrow_mut();
            if discovery.ui.upgrade().is_none() {
                return;
            }
            let Some(&capability) = crate::ui::GEF_COMMAND_CAPABILITIES.get(discovery.next) else {
                let Some(ui) = discovery.ui.upgrade() else {
                    return;
                };
                ui.show_gef_capabilities(&discovery.available);
                configure_gef_context(&discovery.ui, client);
                return;
            };
            discovery.next += 1;
            capability
        };
        let command = format!("-complete {}", crate::debugger::quote(capability));
        let discovery_for_response = Rc::clone(&discovery);
        if client
            .request(&command, move |client, record| {
                if crate::debugger::has_exact_command_completion(&record, capability) {
                    discovery_for_response
                        .borrow_mut()
                        .available
                        .insert(capability);
                }
                probe_next_gef_capability(client, discovery_for_response);
            })
            .is_ok()
        {
            return;
        }
        // A saturated or disconnected MI client rejected this probe. Skip it
        // without recursively walking the remaining capability list.
    }
}

fn configure_gef_context(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    let visible = current_ui.gef_context_visible();
    let control = current_ui.detected_gef_context_control();
    current_ui.set_gef_context_hidden_by_fgdb(false);
    let Some(command) = gef_context_configuration_command(control, visible) else {
        return;
    };
    let command = format!(
        "-interpreter-exec console {}",
        crate::debugger::quote(command)
    );
    let weak_ui = ui.clone();
    if client
        .request(&command, move |_, record| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.set_gef_context_hidden_by_fgdb(!visible && record.is_success());
            }
        })
        .is_err()
        && let Some(ui) = ui.upgrade()
    {
        ui.set_gef_context_hidden_by_fgdb(false);
    }
}

fn gef_context_configuration_command(
    control: GefContextControl,
    visible: bool,
) -> Option<&'static str> {
    match (control, visible) {
        (GefContextControl::ContextCommand, false) => Some("context off"),
        (GefContextControl::ContextCommand, true) => Some("context on"),
        (GefContextControl::OriginalGef, false) => Some("gef config context.enable false"),
        (GefContextControl::OriginalGef, true) => Some("gef config context.enable true"),
        (GefContextControl::None, _) => None,
    }
}

pub(super) fn request_initial_source(ui: &Weak<Ui>, client: &MiClient) {
    let weak_ui = ui.clone();
    let _ = client.request("-file-list-exec-source-file", move |_, record| {
        if record.is_done()
            && let (Some(ui), Some(source_file)) =
                (weak_ui.upgrade(), crate::debugger::current_source(&record))
        {
            ui.show_initial_source(&source_file);
        }
    });
}

pub(super) fn resynchronize_debugger_state(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    if !current_ui.stopped_inspection_available() {
        current_ui.set_status(
            "Refresh unavailable",
            "Pause the target before refreshing debugger state.",
            Some("status-error"),
        );
        return;
    }
    current_ui.prepare_full_resynchronization();
    current_ui.set_status(
        "Refreshing debugger state",
        "Re-reading target ABI, frames, registers, variables, stop points, modules, memory, and inspectors…",
        None,
    );
    drop(current_ui);
    request_initial_source(ui, client);
    refresh_breakpoints(ui, client);
    refresh_modules(ui, client);
    detect_gef(ui, client);
    detect_target_abi(ui, client);
}

#[cfg(test)]
mod tests {
    use super::{GefContextControl, gef_context_configuration_command, parse_pointer_size};

    #[test]
    fn accepts_decimal_and_gdb_hex_pointer_sizes() {
        assert_eq!(parse_pointer_size("4"), Some(4));
        assert_eq!(parse_pointer_size("0x8"), Some(8));
        assert_eq!(parse_pointer_size("16"), None);
        assert_eq!(parse_pointer_size("not-a-size"), None);
    }

    #[test]
    fn configures_context_for_both_supported_gef_families() {
        assert_eq!(
            gef_context_configuration_command(GefContextControl::ContextCommand, false),
            Some("context off")
        );
        assert_eq!(
            gef_context_configuration_command(GefContextControl::ContextCommand, true),
            Some("context on")
        );
        assert_eq!(
            gef_context_configuration_command(GefContextControl::OriginalGef, false),
            Some("gef config context.enable false")
        );
        assert_eq!(
            gef_context_configuration_command(GefContextControl::OriginalGef, true),
            Some("gef config context.enable true")
        );
        assert_eq!(
            gef_context_configuration_command(GefContextControl::None, false),
            None
        );
    }
}
