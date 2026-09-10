use super::*;
use crate::{
    debugger::{InferiorInfo, InferiorState},
    model::{DebuggerModel, DebuggerStateDelta, TargetConnection},
};

const SNAPSHOT: u64 = 17;

fn thread(id: &str, group: &str, tid: u32) -> ThreadInfo {
    ThreadInfo {
        id: id.into(),
        group_id: Some(group.into()),
        target_id: format!("Thread 0x123 (LWP {tid})"),
        name: Some(format!("worker-{tid}")),
        state: "stopped".into(),
        core: None,
        frame: None,
        pc_symbol: None,
        current: id == "1",
    }
}

fn model() -> Rc<DebuggerModel> {
    let model = Rc::new(DebuggerModel::new(None));
    model.set_controls_ready(true);

    model.apply_debugger_state_delta(DebuggerStateDelta::establish_stopped_target(
        TargetConnection::Local,
    ));

    model.show_inferiors(vec![
        InferiorInfo {
            id: "i1".into(),
            pid: Some(42),
            executable: None,
            exit_code: None,
            state: InferiorState::Stopped,
            threads: vec![thread("1", "i1", 10), thread("2", "i1", 20)],
        },
        InferiorInfo {
            id: "i2".into(),
            pid: Some(43),
            executable: None,
            exit_code: None,
            state: InferiorState::Stopped,
            threads: vec![thread("3", "i2", 30)],
        },
    ]);

    model.select_thread("1");
    model.set_debug_state_stale(false);
    model.start_stop_refresh();
    model.bind_stop_context(1).unwrap();
    model
}

fn view() -> Rc<LocksView> {
    let view = build_locks_page(&ColumnLayouts::default());

    view.context.replace(Some(LockContext {
        generation: SNAPSHOT,
        inferior: "i1".into(),
        pid: 42,
    }));

    let waits = [(10, 20), (20, 10)]
        .into_iter()
        .map(|(tid, owner)| {
            let mut wait = LockWait {
                tid,
                thread: format!("worker-{tid}"),
                address: Some(u64::from(tid) * 16),
                ..Default::default()
            };

            wait.observation.ownership = crate::misc::LockOwnership::RobustCandidate(owner);
            wait
        })
        .collect();

    view.show(LockSnapshot {
        waits,
        dependencies: vec![edge(10, 20), edge(20, 10)],
        ..Default::default()
    });

    view.selection.set_selected(0);
    view
}

#[track_caller]
fn assert_buttons(view: &LocksView, model: &DebuggerModel, enabled: [bool; 6]) {
    view.update_controls(model, SNAPSHOT);

    for ((action, button), expected) in view.actions.iter().zip(enabled) {
        assert_eq!(button.is_sensitive(), expected, "{action:?}");
    }
}

#[test]
fn running_lock_process_detection_is_inferior_scoped() {
    let model = model();
    assert!(!super::super::controls::process_running(&model));
    let mut threads = model.thread_snapshot();
    let mut other = thread("3", "i2", 30);
    other.state = "running".into();
    threads.push(other);
    model.stage_threads_for_execution(&threads);
    assert!(!super::super::controls::process_running(&model));
    threads[1].state = "running".into();
    model.stage_threads_for_execution(&threads);
    assert!(!model.inferior_is_running());
    assert!(super::super::controls::process_running(&model));
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn lock_buttons_recover_after_thread_frame_and_pending_state_changes() {
    gtk::init().unwrap();
    let model = model();
    let view = view();
    let available = [true, true, true, true, true, false];
    assert_buttons(&view, &model, available);

    for pending in [
        ThreadActionPending::Selection,
        ThreadActionPending::Setting,
        ThreadActionPending::Analysis,
        ThreadActionPending::Execution,
    ] {
        model.set_thread_action_pending(Some(pending));
        assert_buttons(&view, &model, [false; 6]);
        model.set_thread_action_pending(None);
        assert_buttons(&view, &model, available);
    }

    for pending in [
        InferiorActionPending::Selection,
        InferiorActionPending::Execution,
        InferiorActionPending::Setting,
    ] {
        model.set_inferior_action_pending(Some(pending));
        assert_buttons(&view, &model, [false; 6]);
        model.set_inferior_action_pending(None);
        assert_buttons(&view, &model, available);
    }

    for set in [
        DebuggerModel::set_command_pending,
        DebuggerModel::set_session_pending,
        DebuggerModel::set_resynchronizing,
        DebuggerModel::set_debug_state_stale,
    ] {
        set(&model, true);
        assert_buttons(&view, &model, [false; 6]);
        set(&model, false);
        model.set_debug_state_stale(false);
        assert_buttons(&view, &model, available);
    }

    model.begin_execution_transition();
    assert_buttons(&view, &model, [false; 6]);
    model.finish_execution_transition();
    assert_buttons(&view, &model, available);

    let first_request = view
        .begin_symbol(model.current_stop_refresh_generation())
        .unwrap();

    model.select_thread("2");
    assert!(view.snapshot_current(&model, SNAPSHOT));
    assert_buttons(&view, &model, [false; 6]);
    assert!(!model.is_stop_refresh_current(first_request.generation));
    view.cancel_symbol(first_request);
    model.start_stop_refresh();
    assert_buttons(&view, &model, [false; 6]);
    model.bind_stop_context(1).unwrap();
    assert_buttons(&view, &model, available);
    assert_eq!(view.selected_wait().unwrap().tid, 10);
    model.select_frame(1);
    assert_buttons(&view, &model, [false; 6]);
    model.start_stop_refresh();
    model.bind_stop_context(1).unwrap();
    assert_buttons(&view, &model, available);
    assert_eq!(view.context.borrow().as_ref().unwrap().generation, SNAPSHOT);

    let request = view
        .begin_symbol(model.current_stop_refresh_generation())
        .unwrap();

    view.cancel_symbol(first_request);
    view.complete_symbol(first_request, "obsolete symbol".into());
    assert_eq!(view.symbol_pending.get(), Some(request));
    assert!(!view.symbol.text().contains("obsolete symbol"));
    view.complete_symbol(request, "current symbol".into());
    assert!(view.symbol.text().contains("current symbol"));
    view.navigate(LockAction::Follow);
    assert_buttons(&view, &model, [true; 6]);
    view.navigate(LockAction::Back);
    assert_buttons(&view, &model, available);
    let threads = model.thread_snapshot();
    model.publish_threads(&threads[1..]);
    assert_buttons(&view, &model, [false, true, true, true, true, false]);
    model.publish_threads(&threads);
    assert_buttons(&view, &model, available);
    let mut wrong_owner = threads.clone();
    wrong_owner[1].group_id = Some("i2".into());
    model.stage_threads_for_execution(&wrong_owner);
    assert_buttons(&view, &model, [true, false, true, true, true, false]);
    model.stage_threads_for_execution(&threads);
    assert_buttons(&view, &model, available);
    model.set_selected_inferior("i2");
    model.select_thread("3");
    model.start_stop_refresh();
    model.bind_stop_context(1).unwrap();
    assert_buttons(&view, &model, [false; 6]);
    model.set_selected_inferior("i1");
    model.select_thread("2");
    model.start_stop_refresh();
    model.bind_stop_context(1).unwrap();
    assert_buttons(&view, &model, available);
    model.set_inferior_pid(Some(999));
    assert_buttons(&view, &model, [false; 6]);
    model.set_inferior_pid(Some(42));
    assert_buttons(&view, &model, available);
    view.update_controls(&model, SNAPSHOT + 1);

    assert!(
        view.actions
            .iter()
            .all(|(_, button)| !button.is_sensitive())
    );

    assert_buttons(&view, &model, available);
    let mut running = model.thread_snapshot();
    running[0].state = "running".into();
    model.stage_threads_for_execution(&running);
    assert!(view.process_running(&model));
    assert_buttons(&view, &model, [false; 6]);
    view.invalidate();
    model.stage_threads_for_execution(&threads);
    assert_buttons(&view, &model, [false; 6]);
    view.clear();
    assert_buttons(&view, &model, [false; 6]);
    model.set_controls_ready(false);
    assert_buttons(&view, &model, [false; 6]);
    model.set_controls_ready(true);
    assert_buttons(&view, &model, [false; 6]);
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn lock_symbols_resume_once_after_context_rebinding() {
    gtk::init().unwrap();
    let model = model();
    let view = view();
    let requests = Rc::new(RefCell::new(Vec::new()));
    let pending = Rc::clone(&requests);
    let current = Rc::clone(&model);
    let weak = Rc::downgrade(&view);

    view.handler.replace(Some(Rc::new(move |_| {
        let view = weak.upgrade().unwrap();

        if view.navigation_current(&current, SNAPSHOT)
            && let Some(request) = view.begin_symbol(current.current_stop_refresh_generation())
        {
            pending.borrow_mut().push(request);
        }
    })));

    let settle = || {
        glib::MainContext::default()
            .block_on(glib::timeout_future(std::time::Duration::from_millis(20)));
    };

    for _ in 0..8 {
        view.schedule_symbol();
    }

    assert!(requests.borrow().is_empty());
    model.set_thread_action_pending(Some(ThreadActionPending::Selection));
    settle();
    assert!(requests.borrow().is_empty());
    model.select_thread("2");
    model.set_thread_action_pending(None);
    model.start_stop_refresh();
    model.bind_stop_context(1).unwrap();
    view.schedule_symbol();
    settle();
    assert_eq!(requests.borrow().len(), 1);
    let request = requests.borrow()[0];
    assert_eq!(request.generation, model.current_stop_refresh_generation());
    view.schedule_symbol();
    settle();
    assert_eq!(requests.borrow().len(), 1);
    view.cancel_symbol(request);
    view.schedule_symbol();
    settle();
    assert_eq!(requests.borrow().len(), 2);
    let replacement = requests.borrow()[1];
    assert_ne!(replacement, request);
    view.complete_symbol(request, "cancelled response".into());
    assert_eq!(view.symbol_pending.get(), Some(replacement));
    view.cancel_symbol(replacement);
    view.schedule_symbol();
    view.clear();
    settle();
    assert_eq!(requests.borrow().len(), 2);
}
