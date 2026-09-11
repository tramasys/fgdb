use super::super::tests::{breakpoint, metadata};
use super::*;

fn location(number: &str, enabled: bool) -> Breakpoint {
    let mut breakpoint = breakpoint(number);
    breakpoint.parent_number = Some(breakpoint.command_number().to_owned());
    breakpoint.enabled = enabled;
    breakpoint
}

#[test]
fn group_targets_include_hidden_members_and_respect_parent_membership() {
    let mut watchpoint = breakpoint("3");
    watchpoint.kind = "hw watchpoint".into();
    let mut catchpoint = breakpoint("4");
    catchpoint.kind = "catchpoint".into();

    let breakpoints = vec![
        breakpoint("1"),
        location("1.1", true),
        location("1.2", false),
        breakpoint("2"),
        watchpoint,
        catchpoint,
        breakpoint("5"),
        breakpoint("6"),
        location("9.1", true),
    ];

    let metadata = HashMap::from([
        ("1".into(), metadata("standard", "visible")),
        ("1.2".into(), metadata("child-only", "ignored")),
        ("2".into(), metadata("standard", "hidden")),
        ("3".into(), metadata("standard", "")),
        ("4".into(), metadata("standard", "")),
        ("5".into(), metadata("Standard", "")),
        ("9".into(), metadata("standard", "stale")),
    ]);

    assert_eq!(
        group_targets(
            &breakpoints,
            &metadata,
            "standard",
            StopPointBulkAction::Delete
        ),
        ["1", "2", "3", "4"],
    );

    assert_eq!(
        group_targets(
            &breakpoints,
            &metadata,
            "standard",
            StopPointBulkAction::Enable
        ),
        ["1.2"],
    );

    assert_eq!(
        group_targets(
            &breakpoints,
            &metadata,
            "standard",
            StopPointBulkAction::Disable
        ),
        ["1", "1.1", "2", "3", "4"],
    );

    assert!(
        group_targets(
            &breakpoints,
            &metadata,
            "child-only",
            StopPointBulkAction::Delete
        )
        .is_empty()
    );

    let states = group_states(&breakpoints, &metadata);

    assert_eq!(
        states["standard"],
        GroupState {
            parents: 4,
            enabled: true,
            disabled: true,
        }
    );

    assert!(!states.contains_key("child-only"));
}

#[test]
fn group_targets_reconcile_deleted_and_reassigned_members() {
    let original = vec![breakpoint("1"), breakpoint("2")];
    let latest = vec![breakpoint("2"), breakpoint("3")];

    let mut metadata = HashMap::from([
        ("1".into(), metadata("group with spaces", "")),
        ("2".into(), metadata("group with spaces", "")),
        ("3".into(), metadata("other", "")),
    ]);

    assert_eq!(
        group_targets(
            &original,
            &metadata,
            "group with spaces",
            StopPointBulkAction::Delete
        ),
        ["1", "2"],
    );

    assert_eq!(
        group_targets(
            &latest,
            &metadata,
            "group with spaces",
            StopPointBulkAction::Delete
        ),
        ["2"],
    );

    metadata.get_mut("2").unwrap().group = Some("other".into());

    assert!(
        group_targets(
            &latest,
            &metadata,
            "group with spaces",
            StopPointBulkAction::Delete
        )
        .is_empty()
    );

    assert!(!group_states(&latest, &metadata).contains_key("group with spaces"));
}

#[test]
fn group_state_tracks_parent_and_location_flags_independently() {
    let mut parent = breakpoint("1");
    parent.enabled = false;
    let mut breakpoints = vec![parent, location("1.1", true), location("1.2", false)];
    let metadata = HashMap::from([("1".into(), metadata("test", ""))]);

    assert_eq!(
        group_targets(&breakpoints, &metadata, "test", StopPointBulkAction::Enable),
        ["1", "1.2"],
    );

    assert_eq!(
        group_targets(
            &breakpoints,
            &metadata,
            "test",
            StopPointBulkAction::Disable
        ),
        ["1.1"],
    );

    for enabled in [true, false] {
        for breakpoint in &mut breakpoints {
            breakpoint.enabled = enabled;
        }

        let action = if enabled {
            StopPointBulkAction::Enable
        } else {
            StopPointBulkAction::Disable
        };

        assert!(group_targets(&breakpoints, &metadata, "test", action).is_empty());
        let state = group_states(&breakpoints, &metadata)["test"];
        assert_eq!(state.parents, 1);
        assert_eq!(state.enabled, enabled);
        assert_eq!(state.disabled, !enabled);
    }
}

#[test]
fn group_commands_are_interlocked_during_selection_and_execution_changes() {
    let model = crate::model::DebuggerModel::new(None);
    assert!(!stop_point_actions_available(&model));
    model.set_controls_ready(true);
    assert!(stop_point_actions_available(&model));
    model.set_command_pending(true);
    assert!(!stop_point_actions_available(&model));
    model.set_command_pending(false);
    model.set_thread_action_pending(Some(ThreadActionPending::Selection));
    assert!(!stop_point_actions_available(&model));
    model.set_thread_action_pending(None);
    model.set_inferior_action_pending(Some(InferiorActionPending::Selection));
    assert!(!stop_point_actions_available(&model));
    model.set_inferior_action_pending(None);
    assert!(stop_point_actions_available(&model));

    model.apply_debugger_state_delta(crate::model::DebuggerStateDelta::establish_stopped_target(
        crate::model::TargetConnection::Local,
    ));

    model.set_debug_state_stale(true);
    assert!(!stop_point_actions_available(&model));
    model.set_debug_state_stale(false);
    model.set_controls_running(true);
    assert!(!stop_point_actions_available(&model));
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn group_controls_update_for_same_model_state_and_retire_old_menus() {
    gtk::init().unwrap();
    Theme::graphite().install();
    let events = Rc::new(RefCell::new(Vec::new()));
    let emitted = Rc::clone(&events);
    let controls = GroupControls::new(move |action| emitted.borrow_mut().push(action));

    let mut state = GroupState {
        parents: 2,
        enabled: true,
        disabled: true,
    };

    controls.update(state, true);
    assert!(controls.enable.is_sensitive());
    assert!(controls.disable.is_sensitive());
    assert!(controls.delete.is_sensitive());
    controls.enable.emit_clicked();
    assert_eq!(*events.borrow(), [StopPointBulkAction::Enable]);
    state.disabled = false;
    controls.update(state, true);
    assert!(!controls.enable.is_sensitive());
    controls.enable.emit_clicked();
    assert_eq!(events.borrow().len(), 1);
    controls.update(state, false);
    assert!(!controls.button.is_sensitive());
    assert!(!controls.disable.is_sensitive());
    assert!(!controls.delete.is_sensitive());
    controls.delete.emit_clicked();
    assert_eq!(events.borrow().len(), 1);
    controls.update(state, true);
    controls.disable.emit_clicked();
    assert_eq!(events.borrow().last(), Some(&StopPointBulkAction::Disable));
    controls.retire();
    controls.delete.emit_clicked();
    assert_eq!(events.borrow().len(), 2);
    let rebuilt = GroupControls::new(|_| {});
    rebuilt.update(state, true);
    assert!(rebuilt.disable.is_sensitive());
    assert!(!rebuilt.enable.is_sensitive());
    rebuilt.update(GroupState::default(), true);
    assert!(!rebuilt.button.is_sensitive());
    assert!(!rebuilt.delete.is_sensitive());
}
