use super::*;

fn thread(id: &str, inferior: &str) -> ThreadInfo {
    ThreadInfo {
        id: id.into(),
        group_id: Some(inferior.into()),
        target_id: format!("Thread {id}"),
        name: None,
        state: "stopped".into(),
        core: None,
        frame: None,
        pc_symbol: None,
        current: id == "1",
    }
}

fn model() -> DebuggerModel {
    let model = DebuggerModel::new(None);
    model.set_controls_ready(true);

    model.apply_debugger_state_delta(DebuggerStateDelta::establish_stopped_target(
        TargetConnection::Local,
    ));

    model.show_inferiors(vec![
        InferiorInfo {
            id: "i1".into(),
            pid: Some(10),
            executable: None,
            exit_code: None,
            state: InferiorState::Stopped,
            threads: vec![thread("1", "i1"), thread("2", "i1")],
        },
        InferiorInfo {
            id: "i2".into(),
            pid: Some(20),
            executable: None,
            exit_code: None,
            state: InferiorState::Stopped,
            threads: vec![thread("3", "i2")],
        },
    ]);

    model.select_thread("1");
    model.set_debug_state_stale(false);
    model.start_stop_refresh();
    model.bind_stop_context(1).unwrap();
    record(&model);
    model
}

fn record(model: &DebuggerModel) {
    model.record_return_value(
        Some(ReturnValue {
            value: "42".into(),
            history_variable: Some("$1".into()),
        }),
        Some("1"),
        Some("i1"),
    );
}

#[test]
fn verified_capture_replaces_only_its_own_native_result() {
    let model = model();

    let verified = ReturnValue {
        value: "{x = 7, y = 11}".into(),
        history_variable: Some("$2".into()),
    };

    model.record_verified_return_value(verified.clone(), "2", "i1", Some("$1"));
    assert_eq!(model.return_values()[0].value.value, "42");
    model.record_verified_return_value(verified, "1", "i1", Some("$1"));
    let values = model.return_values();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].value.history_variable.as_deref(), Some("$2"));
    assert_eq!(values[0].value.value, "{x = 7, y = 11}");
}

#[test]
fn history_survives_execution_and_frame_refresh_but_is_thread_scoped() {
    let model = model();
    let original = model.return_values().remove(0);
    model.start_stop_refresh();
    model.bind_stop_context(1).unwrap();
    model.select_frame(2);
    assert!(Rc::ptr_eq(&original, &model.return_values()[0]));
    model.set_controls_running(true);
    assert_eq!(model.return_values()[0].value.value, "42");
    model.mark_inferior_stopped(Some("1"), true);
    model.set_controls_running(false);
    model.record_return_value(None, Some("1"), Some("i1"));
    assert_eq!(model.return_values().len(), 1);
    model.set_current_thread_id(Some("2"));
    assert!(model.return_values().is_empty());
    model.set_current_thread_id(Some("1"));
    assert_eq!(model.return_values().len(), 1);
    model.set_selected_inferior("i2");
    assert!(model.return_values().is_empty());
}

#[test]
fn history_is_bounded_and_deduplicates_absolute_history_references() {
    let model = model();

    for index in 2..=100 {
        model.record_return_value(
            Some(ReturnValue {
                value: index.to_string(),
                history_variable: Some(format!("${index}")),
            }),
            Some("1"),
            Some("i1"),
        );
    }

    let values = model.return_values();
    assert_eq!(values.len(), MAX_RETURN_VALUES);
    assert_eq!(values[0].value.value, "100");
    assert_eq!(values.last().unwrap().value.value, "69");
    let previous_id = values[0].id;
    model.record_return_value(Some(values[0].value.clone()), Some("1"), Some("i1"));
    assert_eq!(model.return_values().len(), MAX_RETURN_VALUES);
    assert!(model.return_values()[0].id > previous_id);
    assert!(model.return_values()[0].variable().return_value.is_some());
    assert!(model.return_values()[0].variable().local_index.is_none());
}

#[test]
fn history_retires_at_owner_and_backend_lifetime_boundaries() {
    let invalidations: &[fn(&DebuggerModel)] = &[
        |model| {
            model.set_controls_ready(false);
        },
        |model| {
            model.set_resynchronizing(true);
        },
        |model| {
            model.set_inferior_started(false);
        },
        |model| {
            model.apply_debugger_state_delta(DebuggerStateDelta::clear_inferior());
        },
        |model| model.clear_inferiors(),
        |model| model.forget_thread_group("1"),
        |model| model.record_inferior_exited("i1"),
        |model| model.record_inferior_started("i1", Some(11)),
    ];

    for invalidate in invalidations {
        let model = model();
        invalidate(&model);
        assert!(model.stopped.return_value.borrow().values.is_empty());
    }
}

#[test]
fn missing_returns_do_not_invent_entries_or_remove_saved_results() {
    let model = model();
    model.record_return_value(None, Some("1"), Some("i1"));

    for thread in [None, Some(""), Some("all")] {
        let value = model.return_values()[0].value.clone();
        model.record_return_value(Some(value), thread, Some("i1"));
    }

    assert_eq!(model.return_values().len(), 1);
    model.record_inferior_exited("i2");
    assert_eq!(model.return_values().len(), 1);
    model.forget_thread_group("2");
    assert_eq!(model.return_values().len(), 1);
    model.clear_return_value();
    assert!(model.return_values().is_empty());
}

#[test]
fn clearing_history_invalidates_in_flight_capture_revisions() {
    let model = model();
    let previous = model.return_value_revision();
    model.clear_return_value();
    assert_ne!(model.return_value_revision(), previous);
}

#[test]
fn large_results_evict_older_history_without_truncating_the_latest_value() {
    let model = model();

    for index in 2..=3 {
        model.record_return_value(
            Some(ReturnValue {
                value: "x".repeat(MAX_RETURN_HISTORY_BYTES),
                history_variable: Some(format!("${index}")),
            }),
            Some("1"),
            Some("i1"),
        );
    }

    let values = model.return_values();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].value.history_variable.as_deref(), Some("$3"));
    assert_eq!(values[0].value.value.len(), MAX_RETURN_HISTORY_BYTES);
}
