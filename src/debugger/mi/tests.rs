use super::{
    GdbCapabilities, MiListItem, MiValue, OutgoingQueue, complete_input_end, drain_outgoing,
    gdb_version_from_banner, listed_features, parse_record, parse_stream_output, quote,
    result_field, scoped_mi_command, validate_mi_command,
};
use std::sync::Mutex;

pub(super) static MI_CLIENT_TEST_LOCK: Mutex<()> = Mutex::new(());
struct BackpressuredWriter;

impl std::io::Write for BackpressuredWriter {
    fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn parses_and_sorts_gdb_feature_negotiation() {
    let record =
        parse_record(r#"2^done,features=["thread-info","pending-breakpoints","thread-info"]"#)
            .unwrap();

    assert_eq!(
        listed_features(&record),
        ["pending-breakpoints", "thread-info"]
    );

    let capabilities = GdbCapabilities {
        version: Some(String::from("17.2")),
        features_known: true,
        features: listed_features(&record),
        mi_async: true,
        pretty_printing: true,
        rust_pretty_printing: true,
        language_printers: true,
    };

    assert!(capabilities.supports("thread-info"));
    assert!(!capabilities.supports("data-read-memory-bytes"));

    assert_eq!(
        capabilities.compatibility_summary(),
        "GDB 17.2  MI async  pretty printers  Rust printers  feature list"
    );

    assert!(GdbCapabilities::default().supports("future-mi-command"));
}

#[test]
fn extracts_versions_from_standard_and_packaged_gdb_banners() {
    assert_eq!(
        gdb_version_from_banner("GNU gdb (GDB) 17.2\nCopyright (C) 2025"),
        Some(String::from("17.2"))
    );

    assert_eq!(
        gdb_version_from_banner("GNU gdb (Ubuntu 15.0.50.20240403-0ubuntu1) 15.0.50.20240403-git"),
        Some(String::from("15.0.50.20240403"))
    );

    assert_eq!(gdb_version_from_banner("unrecognized debugger"), None);
}

#[test]
fn replaces_the_transport_without_replacing_the_client() {
    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let client = super::MiClient::open(|_, _| {}).unwrap();
            let first = client.slave_path();
            let second = client.reconnect().unwrap();
            assert_ne!(first, second);
            assert_eq!(client.slave_path(), second);
        })
        .unwrap();
}

#[test]
fn injected_transport_runs_a_deterministic_request_transcript() {
    use std::{
        cell::RefCell,
        io::{Read, Write},
        rc::Rc,
    };

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let result = Rc::new(RefCell::new(None));
            let result_for_request = Rc::clone(&result);

            let (client, mut peer) =
                super::MiClient::open_with_injected_transport(|_, _| {}).unwrap();

            let token = client
                .request("-thread-info", move |_, record| {
                    result_for_request.replace(Some(record.class));
                })
                .unwrap();

            super::MiClient::on_write_ready(&client.weak(), gtk::glib::IOCondition::OUT);
            let mut command = [0_u8; 128];
            let count = peer.read(&mut command).unwrap();

            assert_eq!(
                std::str::from_utf8(&command[..count]).unwrap(),
                format!("{token}-thread-info\n")
            );

            peer.write_all(format!("{token}^done\n").as_bytes())
                .unwrap();

            super::MiClient::on_io_ready(&client.weak(), gtk::glib::IOCondition::IN);
            assert_eq!(result.borrow().as_deref(), Some("done"));
        })
        .unwrap();
}

#[test]
fn one_readiness_callback_drains_records_across_chunk_boundaries() {
    use std::{cell::RefCell, io::Write, rc::Rc};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let result = Rc::new(RefCell::new(None));
            let result_for_request = Rc::clone(&result);
            let (client, mut peer) =
                super::MiClient::open_with_injected_transport(|_, _| {}).unwrap();

            let token = client
                .request("-thread-info", move |_, record| {
                    result_for_request.replace(Some(record.class));
                })
                .unwrap();

            let padding = "x".repeat(super::MI_READ_CHUNK_BYTES + 512);
            let transcript = format!("~\"{padding}\"\n{token}^done\n");
            peer.write_all(transcript.as_bytes()).unwrap();

            super::MiClient::on_io_ready(&client.weak(), gtk::glib::IOCondition::IN);

            assert_eq!(result.borrow().as_deref(), Some("done"));
            assert!(client.incoming.borrow().is_empty());
        })
        .unwrap();
}

#[test]
fn reconnect_clears_timeout_quarantine_before_accepting_new_work() {
    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let client = super::MiClient::open(|_, _| {}).unwrap();
            client.ready.set(true);
            client.quarantine("simulated timed-out command");
            assert!(!client.is_ready());
            assert!(client.request("-thread-info", |_, _| {}).is_err());
            client.reconnect().unwrap();
            assert!(!client.quarantined.get());
            assert!(client.connected.get());
            assert!(!client.is_ready());
            client.ready.set(true);
            assert!(client.request("-thread-info", |_, _| {}).is_ok());
        })
        .unwrap();
}

#[test]
fn reconnect_does_not_publish_ready_from_an_old_printer_probe() {
    use std::{cell::RefCell, rc::Rc};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_for_client = Rc::clone(&events);

            let client = super::MiClient::open(move |_, event| {
                events_for_client.borrow_mut().push(event);
            })
            .unwrap();
            client.process_line("(gdb)");
            client.process_line(r#"1^done,features=[]"#);
            client.process_line(r#"2^done,value="17""#);
            client.process_line(r#"3^done,value="2""#);
            client.process_line("4^done");
            client.process_line("5^done");
            assert!(events.borrow().is_empty());
            client.reconnect().unwrap();
            assert!(events.borrow().is_empty());
            assert!(!client.is_ready());
        })
        .unwrap();
}

#[test]
fn publishes_ready_only_after_capability_negotiation() {
    use std::{cell::RefCell, rc::Rc};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_for_client = Rc::clone(&events);

            let client = super::MiClient::open(move |_, event| {
                events_for_client.borrow_mut().push(event);
            })
            .unwrap();
            client.process_line("(gdb)");
            assert!(events.borrow().is_empty());

            client.process_line(
                r#"1^done,features=["pending-breakpoints","data-read-memory-bytes"]"#,
            );

            client.process_line(r#"2^done,value="17""#);
            client.process_line(r#"3^done,value="2""#);
            client.process_line("4^done");
            client.process_line("5^done");
            client.process_line("6^done");
            client.process_line("7^done");
            let events = events.borrow();

            let [super::MiEvent::Ready(capabilities)] = events.as_slice() else {
                panic!("expected one negotiated ready event, got {events:?}");
            };

            assert_eq!(capabilities.version.as_deref(), Some("17.2"));
            assert!(capabilities.mi_async);
            assert!(capabilities.pretty_printing);
            assert!(capabilities.rust_pretty_printing);
            assert!(capabilities.supports("pending-breakpoints"));
            assert!(!capabilities.supports("thread-info"));
        })
        .unwrap();
}

#[test]
fn remains_ready_when_rust_printer_probing_is_unavailable() {
    use std::{cell::RefCell, rc::Rc};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_for_client = Rc::clone(&events);

            let client = super::MiClient::open(move |_, event| {
                events_for_client.borrow_mut().push(event);
            })
            .unwrap();
            client.process_line("(gdb)");
            client.process_line(r#"1^done,features=[]"#);
            client.process_line(r#"2^done,value="17""#);
            client.process_line(r#"3^done,value="2""#);
            client.process_line("4^done");
            client.process_line("5^done");
            client.process_line(r#"6^error,msg="Python is unavailable""#);
            client.process_line(r#"7^error,msg="Python is unavailable""#);
            let events = events.borrow();

            let [super::MiEvent::Ready(capabilities)] = events.as_slice() else {
                panic!("expected a negotiated ready event, got {events:?}");
            };

            assert!(capabilities.pretty_printing);
            assert!(!capabilities.rust_pretty_printing);
        })
        .unwrap();
}

#[test]
fn publishes_when_rust_printer_availability_changes() {
    use std::{cell::RefCell, rc::Rc};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_for_client = Rc::clone(&events);

            let client = super::MiClient::open(move |_, event| {
                events_for_client.borrow_mut().push(event);
            })
            .unwrap();
            client.ready.set(true);
            client.capabilities.borrow_mut().pretty_printing = true;
            client.refresh_pretty_printer_capabilities();
            client.process_line("1^done");
            let events = events.borrow();

            let [super::MiEvent::CapabilitiesChanged(capabilities)] = events.as_slice() else {
                panic!("expected one capability update, got {events:?}");
            };

            assert!(capabilities.rust_pretty_printing);
        })
        .unwrap();
}

#[test]
fn publishes_process_scoped_async_events() {
    use std::{cell::RefCell, rc::Rc};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_for_client = Rc::clone(&events);

            let client = super::MiClient::open(move |_, event| {
                events_for_client.borrow_mut().push(event);
            })
            .unwrap();
            client.ready.set(true);
            client.process_line(r#"=thread-group-started,id="i2",pid="4312""#);
            client.process_line(r#"=thread-created,id="3",group-id="i2""#);
            client.process_line(r#"=thread-exited,id="4",group-id="i2""#);
            client.process_line(r#"=library-loaded,id="libc",thread-group="i2""#);

            client.process_line(
                r#"=thread-selected,id="3",frame={level="2",addr="0x401004"}"#,
            );

            client.process_line(r#"=thread-group-selected,id="i2""#);
            client.process_line(r#"*running,thread-id="all""#);

            client.process_line(
                r#"*stopped,reason="fork",newpid="4313",thread-id="3",thread-group="i2",stopped-threads="all",frame={level="0",addr="0x401000"}"#,
            );

            client.process_line(r#"=thread-group-exited,id="i2",exit-code="0""#);
            client.process_line("(gdb)");

            assert_eq!(
                events.borrow().as_slice(),
                [
                    super::MiEvent::InferiorStarted {
                        id: String::from("i2"),
                        pid: Some(4312),
                    },
                    super::MiEvent::ThreadsChanged {
                        id: Some(String::from("3")),
                        group_id: Some(String::from("i2")),
                    },
                    super::MiEvent::ThreadExited {
                        id: String::from("4"),
                        group_id: Some(String::from("i2")),
                    },
                    super::MiEvent::LibrariesChanged {
                        group_id: Some(String::from("i2")),
                    },
                    super::MiEvent::SelectionChanged {
                        thread_id: Some(String::from("3")),
                        group_id: None,
                        frame_level: Some(2),
                    },
                    super::MiEvent::SelectionChanged {
                        thread_id: None,
                        group_id: Some(String::from("i2")),
                        frame_level: None,
                    },
                    super::MiEvent::Running {
                        thread_id: Some(String::from("all")),
                    },
                    super::MiEvent::Stopped {
                        reason: Some(String::from("fork")),
                        signal_name: None,
                        signal_meaning: None,
                        address: Some(String::from("0x401000")),
                        thread_id: Some(String::from("3")),
                        group_id: Some(String::from("i2")),
                        frame_level: Some(0),
                        fork_pid: Some(4313),
                        all_stopped: true,
                    },
                    super::MiEvent::InferiorExited {
                        id: String::from("i2"),
                        exit_code: Some(String::from("0")),
                    },
                    super::MiEvent::ThreadExitPrompt,
                ]
            );
        })
        .unwrap();
}

#[test]
fn publishes_structured_command_parameter_changes() {
    use std::{cell::RefCell, rc::Rc};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_for_client = Rc::clone(&events);

            let client = super::MiClient::open(move |_, event| {
                events_for_client.borrow_mut().push(event);
            })
            .unwrap();
            client.ready.set(true);
            client.process_line(r#"=cmd-param-changed,param="scheduler-locking",value="step""#);

            assert_eq!(
                events.borrow().as_slice(),
                [super::MiEvent::CommandParameterChanged {
                    parameter: String::from("scheduler-locking"),
                    value: Some(String::from("step")),
                }]
            );
        })
        .unwrap();
}

#[test]
fn console_prose_does_not_drive_debugger_state() {
    use std::{cell::RefCell, rc::Rc};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_for_client = Rc::clone(&events);

            let client = super::MiClient::open(move |_, event| {
                events_for_client.borrow_mut().push(event);
            })
            .unwrap();
            client.ready.set(true);
            let warning = r#"~"Cannot remove breakpoints because program is no longer writable.\nFurther execution is probably impossible.\n""#;
            client.process_line(warning);
            client.process_line(warning);
            client.process_line("(gdb)");
            assert!(events.borrow().is_empty());
            assert!(client.is_ready());
            assert!(client.request("-thread-info", |_, _| {}).is_ok());
        })
        .unwrap();
}

#[test]
fn quarantine_fails_callbacks_before_publishing_the_terminal_event() {
    use std::{cell::RefCell, rc::Rc};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let order = Rc::new(RefCell::new(Vec::new()));
            let order_for_events = Rc::clone(&order);

            let client = super::MiClient::open(move |_, event| {
                if matches!(event, super::MiEvent::DebuggerUnusable(_)) {
                    order_for_events.borrow_mut().push("event");
                }
            })
            .unwrap();
            let order_for_request = Rc::clone(&order);

            client
                .request("-thread-info", move |_, record| {
                    assert_eq!(record.class, "unavailable");
                    order_for_request.borrow_mut().push("callback");
                })
                .unwrap();

            client.quarantine("test quarantine");
            assert_eq!(order.borrow().as_slice(), ["callback", "event"]);
            assert!(!client.is_ready());
            assert!(client.request("-thread-info", |_, _| {}).is_err());
        })
        .unwrap();
}

#[test]
fn an_unwritten_timed_out_request_is_cancelled_without_quarantining_gdb() {
    use std::{cell::RefCell, rc::Rc, time::Instant};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_for_client = Rc::clone(&events);

            let client = super::MiClient::open(move |_, event| {
                events_for_client.borrow_mut().push(event);
            })
            .unwrap();
            client.ready.set(true);
            let response_class = Rc::new(RefCell::new(None));
            let response_class_for_request = Rc::clone(&response_class);

            let token = client
                .request("-thread-info", move |_, record| {
                    response_class_for_request.replace(Some(record.class));
                })
                .unwrap();

            client
                .pending
                .borrow_mut()
                .get_mut(&token)
                .unwrap()
                .deadline = Instant::now();

            client.expire_requests();
            assert_eq!(response_class.borrow().as_deref(), Some("timeout"));
            assert!(client.is_ready());
            assert!(events.borrow().is_empty());
        })
        .unwrap();
}

#[test]
fn a_sent_timed_out_request_quarantines_the_command_stream() {
    use std::{cell::RefCell, rc::Rc, time::Instant};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_for_client = Rc::clone(&events);

            let client = super::MiClient::open(move |_, event| {
                events_for_client.borrow_mut().push(event);
            })
            .unwrap();
            client.ready.set(true);
            let token = client.request("-thread-info", |_, _| {}).unwrap();
            client.outgoing.borrow_mut().clear();

            client
                .pending
                .borrow_mut()
                .get_mut(&token)
                .unwrap()
                .deadline = Instant::now();

            client.expire_requests();
            assert!(!client.is_ready());

            assert!(matches!(
                events.borrow().as_slice(),
                [super::MiEvent::DebuggerUnusable(_)]
            ));
        })
        .unwrap();
}

#[test]
fn unrelated_response_progress_does_not_extend_an_expired_request() {
    use std::{cell::RefCell, rc::Rc, time::Instant};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let client = super::MiClient::open(|_, _| {}).unwrap();
            client.ready.set(true);
            let first = client.request("-thread-info", |_, _| {}).unwrap();
            let second_result = Rc::new(RefCell::new(None));
            let second_result_for_request = Rc::clone(&second_result);

            let second = client
                .request("-stack-list-frames", move |_, record| {
                    second_result_for_request.replace(Some(record.class));
                })
                .unwrap();

            client.process_line(&format!("{first}^done"));

            client
                .pending
                .borrow_mut()
                .get_mut(&second)
                .unwrap()
                .deadline = Instant::now();

            client.expire_requests();
            assert_eq!(second_result.borrow().as_deref(), Some("timeout"));
            assert!(!client.pending.borrow().contains_key(&second));
            assert!(client.is_ready());
        })
        .unwrap();
}

#[test]
fn newer_stop_cancels_owned_inspection_without_touching_current_work() {
    use std::{cell::RefCell, rc::Rc};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let client = super::MiClient::open(|_, _| {}).unwrap();
            client.ready.set(true);
            let results = Rc::new(RefCell::new(Vec::new()));
            let results_for_old = Rc::clone(&results);

            let old = client
                .request_for_stop(
                    "-stack-list-frames --thread 1",
                    1,
                    || true,
                    move |_, record| results_for_old.borrow_mut().push(record.class),
                )
                .unwrap();

            let results_for_scoped = Rc::clone(&results);

            let old_scoped = client
                .request_with_print_limit_for_stop(
                    "-stack-list-variables --thread 1 --frame 0 --simple-values",
                    32,
                    1,
                    || true,
                    move |_, record| results_for_scoped.borrow_mut().push(record.class),
                )
                .unwrap();

            let current = client
                .request_for_stop("-thread-info", 2, || true, |_, _| {})
                .unwrap();

            client.cancel_stale_stop_requests(2);
            assert_eq!(results.borrow().as_slice(), ["superseded", "superseded"]);
            assert!(!client.pending.borrow().contains_key(&old));
            assert!(client.pending.borrow().contains_key(&current));

            assert_ne!(
                client
                    .scoped_request
                    .borrow()
                    .as_ref()
                    .map(|request| request.token),
                Some(old_scoped)
            );

            assert!(client.is_ready());
        })
        .unwrap();
}

#[test]
fn a_sent_superseded_scoped_request_is_drained_before_the_next_one_starts() {
    use std::{cell::Cell, cell::RefCell, rc::Rc};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let client = super::MiClient::open(|_, _| {}).unwrap();
            client.ready.set(true);
            let first_is_current = Rc::new(Cell::new(true));
            let first_result = Rc::new(RefCell::new(None));
            let first_is_current_for_request = Rc::clone(&first_is_current);
            let first_result_for_request = Rc::clone(&first_result);

            let first = client
                .request_with_print_limit_for_owner(
                    "-data-evaluate-expression value",
                    32,
                    None,
                    move || first_is_current_for_request.get(),
                    move |_, record| {
                        first_result_for_request.replace(Some(record.class));
                    },
                )
                .unwrap();

            // Model a command that has left fgdb without needing a live
            // GDB process in this transport-level regression test.
            client.outgoing.borrow_mut().clear();
            first_is_current.set(false);

            let second = client
                .request_with_print_limit_for_owner(
                    "-data-evaluate-expression other",
                    32,
                    None,
                    || true,
                    |_, _| {},
                )
                .unwrap();

            client.expire_requests();
            assert!(first_result.borrow().is_none());

            assert_eq!(
                client
                    .scoped_request
                    .borrow()
                    .as_ref()
                    .map(|request| request.token),
                Some(first)
            );

            assert_eq!(client.scoped_queue.borrow().len(), 1);
            client.process_line(&format!("{first}^done"));
            assert_eq!(first_result.borrow().as_deref(), Some("superseded"));

            assert_eq!(
                client
                    .scoped_request
                    .borrow()
                    .as_ref()
                    .map(|request| request.token),
                Some(second)
            );

            assert!(client.scoped_queue.borrow().is_empty());
        })
        .unwrap();
}

#[test]
fn serializes_console_commands_and_returns_their_stream_output() {
    use std::{cell::RefCell, rc::Rc, time::Instant};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let client = super::MiClient::open(|_, _| {}).unwrap();
            client.ready.set(true);
            let result = Rc::new(RefCell::new(None));
            let result_for_request = Rc::clone(&result);

            let token = client
                .request_console("show directories", move |_, record, output| {
                    result_for_request.replace(Some((record.class, output)));
                })
                .unwrap();

            let expired = Instant::now();

            client
                .scoped_request
                .borrow_mut()
                .as_mut()
                .unwrap()
                .deadline = expired;

            client.process_line(r#"~"Source directories: /src:$cwd\n""#);
            client.process_line(r#"&"warning from GDB\n""#);
            client.process_line(r#"@"inferior output\n""#);
            assert!(client.scoped_request.borrow().as_ref().unwrap().deadline > expired);
            assert!(result.borrow().is_none());
            client.process_line(&format!("{token}^done"));

            assert_eq!(
                result.borrow().as_ref(),
                Some(&(
                    String::from("done"),
                    String::from("Source directories: /src:$cwd\nwarning from GDB\n")
                ))
            );
        })
        .unwrap();
}

#[test]
fn scoped_inspection_overtakes_queued_background_work() {
    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let client = super::MiClient::open(|_, _| {}).unwrap();
            client.ready.set(true);

            client
                .request_console("show version", |_, _, _| {})
                .unwrap();

            client
                .request_console("show directories", |_, _, _| {})
                .unwrap();

            client
                .request_with_print_limit_for_owner(
                    "-stack-list-variables --simple-values",
                    32,
                    None,
                    || true,
                    |_, _| {},
                )
                .unwrap();

            let queue = client.scoped_queue.borrow();
            assert_eq!(queue.len(), 2);
            assert_eq!(queue[0].class, super::CommandClass::Inspection);
            assert_eq!(queue[1].class, super::CommandClass::Background);
        })
        .unwrap();
}

#[test]
fn request_admission_preserves_control_and_execution_capacity() {
    use std::{cell::RefCell, rc::Rc};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_for_client = Rc::clone(&events);

            let client = super::MiClient::open(move |_, event| {
                events_for_client.borrow_mut().push(event);
            })
            .unwrap();
            client.ready.set(true);

            for _ in 0..super::MAX_INSPECTION_REQUESTS {
                client
                    .request_when("-thread-info", || true, |_, _| {})
                    .unwrap();
            }

            let rejected = client.request_when("-thread-info", || true, |_, _| {});
            assert_eq!(rejected.unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
            assert!(client.request("-break-insert main", |_, _| {}).is_ok());

            let additional_controls =
                super::MAX_NON_EXECUTION_REQUESTS.saturating_sub(client.pending.borrow().len());

            for _ in 0..additional_controls {
                client.request("-gdb-show language", |_, _| {}).unwrap();
            }

            assert_eq!(
                client
                    .request("-gdb-show language", |_, _| {})
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::WouldBlock
            );

            assert!(client.send("-exec-next").is_ok());

            assert!(events.borrow().iter().any(|event| matches!(
                event,
                super::MiEvent::Performance(notice)
                    if notice.outcome == crate::performance::BudgetOutcome::Rejected
            )));
        })
        .unwrap();
}

#[test]
fn inspection_window_reserves_a_priority_lane_for_execution() {
    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let client = super::MiClient::open(|_, _| {}).unwrap();
            client.ready.set(true);

            let tokens = (0..=super::MAX_ACTIVE_INSPECTION_REQUESTS)
                .map(|_| {
                    client
                        .request_when("-thread-info", || true, |_, _| {})
                        .unwrap()
                })
                .collect::<Vec<_>>();

            assert_eq!(
                client
                    .pending
                    .borrow()
                    .values()
                    .filter(|request| request.started_at.is_some())
                    .count(),
                super::MAX_ACTIVE_INSPECTION_REQUESTS
            );

            assert!(
                client
                    .pending
                    .borrow()
                    .get(tokens.last().unwrap())
                    .unwrap()
                    .started_at
                    .is_none()
            );

            let execution = client.send("-exec-next").unwrap();

            assert_eq!(
                client
                    .outgoing
                    .borrow()
                    .commands
                    .front()
                    .map(|command| command.token),
                Some(execution)
            );

            client.process_line(&format!("{}^done", tokens[0]));

            assert!(
                client
                    .pending
                    .borrow()
                    .get(tokens.last().unwrap())
                    .unwrap()
                    .started_at
                    .is_some()
            );
        })
        .unwrap();
}

#[test]
fn queued_request_timeout_is_safe_and_does_not_quarantine_gdb() {
    use std::{cell::RefCell, rc::Rc, time::Instant};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let client = super::MiClient::open(|_, _| {}).unwrap();
            client.ready.set(true);

            for _ in 0..super::MAX_ACTIVE_INSPECTION_REQUESTS {
                client
                    .request_when("-thread-info", || true, |_, _| {})
                    .unwrap();
            }

            let result = Rc::new(RefCell::new(None));
            let result_for_handler = Rc::clone(&result);

            let queued = client
                .request_when(
                    "-stack-list-frames",
                    || true,
                    move |_, record| {
                        result_for_handler.replace(Some(record.class));
                    },
                )
                .unwrap();

            {
                let mut pending = client.pending.borrow_mut();
                let request = pending.get_mut(&queued).unwrap();
                assert!(request.started_at.is_none());
                request.deadline = Instant::now();
            }

            client.expire_requests();
            assert_eq!(result.borrow().as_deref(), Some("timeout"));
            assert!(client.is_ready());
        })
        .unwrap();
}

#[test]
fn oversized_tokenized_result_fails_only_its_request() {
    use std::{cell::RefCell, rc::Rc};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_for_client = Rc::clone(&events);

            let client = super::MiClient::open(move |_, event| {
                events_for_client.borrow_mut().push(event);
            })
            .unwrap();
            client.ready.set(true);
            let result = Rc::new(RefCell::new(None));
            let result_for_request = Rc::clone(&result);

            let token = client
                .request("-thread-info", move |_, record| {
                    result_for_request.replace(Some(record.class));
                })
                .unwrap();

            let mut oversized = format!("{token}^done,value=\"").into_bytes();
            oversized.resize(super::MAX_MI_RECORD_BYTES + 1, b'x');
            client.consume(&oversized);
            assert!(result.borrow().is_none());
            assert!(client.discarding_oversized_line.get());
            client.consume(b"discarded tail\n*running,thread-id=\"all\"\n");
            assert_eq!(result.borrow().as_deref(), Some("resource-limit"));
            assert!(!client.pending.borrow().contains_key(&token));
            assert!(!client.discarding_oversized_line.get());
            assert!(client.is_ready());

            assert!(
                !events
                    .borrow()
                    .iter()
                    .any(|event| matches!(event, super::MiEvent::DebuggerUnusable(_)))
            );

            assert!(events.borrow().iter().any(|event| matches!(
                event,
                super::MiEvent::Running { thread_id }
                    if thread_id.as_deref() == Some("all")
            )));
        })
        .unwrap();
}

#[test]
fn oversized_console_capture_returns_an_explicit_partial_result() {
    use std::{cell::RefCell, rc::Rc};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let client = super::MiClient::open(|_, _| {}).unwrap();
            client.ready.set(true);
            let result = Rc::new(RefCell::new(None));
            let result_for_request = Rc::clone(&result);

            let token = client
                .request_console("show directories", move |_, record, output| {
                    result_for_request.replace(Some((record.output_was_truncated(), output.len())));
                })
                .unwrap();

            let output = "x".repeat(super::MAX_CAPTURED_CONSOLE_BYTES + 257);
            client.process_line(&format!("~\"{output}\""));
            client.process_line(&format!("{token}^done"));

            assert_eq!(
                *result.borrow(),
                Some((true, super::MAX_CAPTURED_CONSOLE_BYTES))
            );

            assert!(client.is_ready());
        })
        .unwrap();
}

#[test]
fn oversized_asynchronous_state_requires_recovery() {
    use std::{cell::RefCell, rc::Rc};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_for_client = Rc::clone(&events);

            let client = super::MiClient::open(move |_, event| {
                events_for_client.borrow_mut().push(event);
            })
            .unwrap();
            client.ready.set(true);

            client.handle_oversized_record(super::MiRecordHeader {
                token: None,
                kind: Some(b'*'),
            });

            assert!(!client.is_ready());

            assert!(matches!(
                events.borrow().as_slice(),
                [super::MiEvent::DebuggerUnusable(_)]
            ));
        })
        .unwrap();
}

#[test]
fn malformed_state_records_require_recovery_but_late_errors_do_not() {
    use std::{cell::RefCell, rc::Rc};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_for_client = Rc::clone(&events);

            let client = super::MiClient::open(move |_, event| {
                events_for_client.borrow_mut().push(event);
            })
            .unwrap();
            client.ready.set(true);
            client.process_line(r#"91^error,msg="late response""#);
            assert!(events.borrow().is_empty());
            client.process_line("*stopped,reason");

            assert!(matches!(
                events.borrow().as_slice(),
                [super::MiEvent::DebuggerUnusable(_)]
            ));
        })
        .unwrap();
}

#[test]
fn transport_can_be_replaced_from_inside_an_input_callback() {
    use std::{cell::Cell, rc::Rc};

    let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
    let context = gtk::glib::MainContext::new();

    context
        .with_thread_default(|| {
            let replaced = Rc::new(Cell::new(false));
            let replaced_for_client = Rc::clone(&replaced);
            let stale_running_seen = Rc::new(Cell::new(false));
            let stale_running_seen_for_client = Rc::clone(&stale_running_seen);

            let client = super::MiClient::open(move |client, event| {
                if event == super::MiEvent::InferiorsChanged {
                    client.reconnect().unwrap();
                    replaced_for_client.set(true);
                } else if matches!(event, super::MiEvent::Running { .. }) {
                    stale_running_seen_for_client.set(true);
                }
            })
            .unwrap();
            client.ready.set(true);
            client.consume(b"=thread-group-added,id=\"i2\"\n*running,thread-id=\"all\"\n");
            assert!(replaced.get());
            assert!(!stale_running_seen.get());
        })
        .unwrap();
}

#[test]
fn accepts_all_successful_mi_result_classes_for_session_commands() {
    for class in ["done", "connected", "running"] {
        assert!(parse_record(&format!("1^{class}")).unwrap().is_success());
    }

    assert!(
        !parse_record(r#"1^error,msg="failed""#)
            .unwrap()
            .is_success()
    );

    assert!(!super::synthetic_error_record("timeout", "timed out").is_success());
}

#[test]
fn drains_outgoing_commands_in_fifo_order_and_bounded_batches() {
    let mut outgoing = OutgoingQueue::default();
    outgoing.enqueue(7, 0, "-exec-next").unwrap();
    outgoing.enqueue(8, 0, "-exec-step").unwrap();
    let total = outgoing.remaining_bytes;
    let mut written = Vec::new();
    assert!(!drain_outgoing(&mut written, &mut outgoing, 5).unwrap());
    assert_eq!(written, b"7-exe");
    assert_eq!(outgoing.remaining_bytes, total - 5);
    assert!(drain_outgoing(&mut written, &mut outgoing, 1024).unwrap());
    assert_eq!(written, b"7-exec-next\n8-exec-step\n");
    assert_eq!(outgoing.remaining_bytes, 0);
}

#[test]
fn preserves_queued_output_when_the_pty_applies_backpressure() {
    let mut outgoing = OutgoingQueue::default();
    outgoing.enqueue(11, 0, "-exec-continue").unwrap();
    let remaining = outgoing.remaining_bytes;
    assert!(!drain_outgoing(&mut BackpressuredWriter, &mut outgoing, 1024).unwrap());
    assert_eq!(outgoing.remaining_bytes, remaining);
    assert_eq!(outgoing.commands.front().unwrap().written, 0);
}

#[test]
fn priority_commands_overtake_only_wholly_unwritten_output() {
    let mut outgoing = OutgoingQueue::default();
    outgoing.enqueue(1, 2, "-inspection-one").unwrap();
    outgoing.enqueue(2, 2, "-inspection-two").unwrap();
    outgoing.enqueue(3, 0, "-exec-next").unwrap();

    assert_eq!(
        outgoing
            .commands
            .iter()
            .map(|command| command.token)
            .collect::<Vec<_>>(),
        [3, 1, 2]
    );

    outgoing.advance(1);
    outgoing.enqueue(4, 0, "-exec-step").unwrap();

    assert_eq!(
        outgoing
            .commands
            .iter()
            .map(|command| command.token)
            .collect::<Vec<_>>(),
        [3, 4, 1, 2]
    );
}

#[test]
fn only_cancels_commands_that_have_not_started_writing() {
    let mut outgoing = OutgoingQueue::default();
    outgoing.enqueue(1, 0, "-first").unwrap();
    outgoing.enqueue(2, 0, "-second").unwrap();
    outgoing.advance(1);
    assert!(!outgoing.cancel_unstarted(1));
    assert!(outgoing.cancel_unstarted(2));
    assert_eq!(outgoing.commands.len(), 1);
    assert_eq!(outgoing.commands.front().unwrap().token, 1);
}

#[test]
fn cancels_expired_unstarted_commands_in_one_queue_pass() {
    let mut outgoing = OutgoingQueue::default();
    outgoing.enqueue(1, 0, "-partially-written").unwrap();
    outgoing.enqueue(2, 0, "-expired-two").unwrap();
    outgoing.enqueue(3, 0, "-retained").unwrap();
    outgoing.enqueue(4, 0, "-expired-four").unwrap();
    outgoing.advance(3);
    let before = outgoing.remaining_bytes;
    let removed = outgoing.commands[1].bytes.len() + outgoing.commands[3].bytes.len();

    let cancelled = outgoing.cancel_unstarted_many(&std::collections::HashSet::from([
        1_u64, 2_u64, 4_u64, 99_u64,
    ]));

    assert_eq!(cancelled, std::collections::HashSet::from([2_u64, 4_u64]));
    assert_eq!(outgoing.remaining_bytes, before - removed);

    assert_eq!(
        outgoing
            .commands
            .iter()
            .map(|command| (command.token, command.written))
            .collect::<Vec<_>>(),
        [(1, 3), (3, 0)]
    );
}

#[test]
fn locates_complete_mi_lines_without_claiming_a_partial_record() {
    let incoming = b"1^done\r\n*stopped,reason=\"breakpoint-hit\"\n3^do";

    assert_eq!(
        complete_input_end(incoming),
        Some(b"1^done\r\n*stopped,reason=\"breakpoint-hit\"\n".len())
    );

    assert_eq!(complete_input_end(b"3^do"), None);
    assert_eq!(complete_input_end(b"3^done\n"), Some(7));
}

#[test]
fn locates_a_terminator_relative_to_an_existing_partial_record() {
    let previous = b"17^done,value=\"partial";
    let appended = b" value\"\n*stopped\nnext";
    let complete = complete_input_end(appended).map(|end| previous.len() + end);

    assert_eq!(
        complete,
        Some(previous.len() + b" value\"\n*stopped\n".len())
    );
}

#[test]
fn parses_nested_stack_frames() {
    let record = parse_record(
        r#"17^done,stack=[frame={level="0",addr="0x1",func="main",fullname="/tmp/a.c",line="8"},frame={level="1",addr="0x2",func="_start"}]"#,
    )
    .expect("valid record");
    assert_eq!(record.token, Some(17));
    assert_eq!(record.class, "done");
    let stack = record.field("stack").and_then(MiValue::as_list).unwrap();

    let MiListItem::Result(frame) = &stack[0] else {
        panic!("frame result expected");
    };

    let tuple = frame.value.as_tuple().unwrap();

    assert_eq!(
        result_field(tuple, "fullname").and_then(MiValue::as_const),
        Some("/tmp/a.c")
    );
}

#[test]
fn parses_value_lists_and_escaped_strings() {
    let record =
        parse_record(r#"4^done,register-names=["rax","r\"bx","line\n"]"#).expect("valid record");

    let names = record
        .field("register-names")
        .and_then(MiValue::as_list)
        .unwrap();

    assert_eq!(
        names,
        [
            MiListItem::Value(MiValue::Const(String::from("rax"))),
            MiListItem::Value(MiValue::Const(String::from("r\"bx"))),
            MiListItem::Value(MiValue::Const(String::from("line\n"))),
        ]
    );
}

#[test]
fn parses_unescaped_and_escaped_strings_through_the_same_model() {
    let plain = parse_record(r#"1^done,value="plain ASCII value""#).unwrap();
    let escaped = parse_record(r#"2^done,value="plain\040ASCII\040value""#).unwrap();
    assert_eq!(plain.field("value"), escaped.field("value"));
}

#[test]
fn replaces_invalid_bytes_only_when_an_mi_escape_requires_it() {
    let record = parse_record(r#"1^done,value="\xff""#).expect("valid record");
    assert_eq!(record.field("value").and_then(MiValue::as_const), Some("�"));
}

#[test]
fn rejects_excessively_nested_values() {
    let nested = format!(
        "1^done,value={}\"x\"{}",
        "[".repeat(super::MAX_MI_NESTING + 1),
        "]".repeat(super::MAX_MI_NESTING + 1)
    );

    assert!(parse_record(&nested).unwrap_err().contains("nesting limit"));
}

#[test]
fn rejects_tokens_outside_the_supported_range() {
    assert!(
        parse_record("18446744073709551616^done")
            .unwrap_err()
            .contains("token")
    );
}

#[test]
fn rejects_unsafe_or_unreasonably_large_commands() {
    assert!(validate_mi_command("").is_err());
    assert!(validate_mi_command("-exec-next\n99-gdb-exit").is_err());
    assert!(validate_mi_command(&"x".repeat(super::MAX_MI_COMMAND_BYTES + 1)).is_err());
    assert!(super::validate_console_command("show directories\ngdb-exit").is_err());
}

#[test]
fn quotes_mi_arguments() {
    assert_eq!(quote("src/a file.c:12"), r#""src/a file.c:12""#);
    assert_eq!(quote("a\"b\\c"), r#""a\"b\\c""#);
}

#[test]
fn wraps_mi_commands_in_scoped_print_limits() {
    assert_eq!(
        scoped_mi_command("-stack-list-variables --simple-values", 128, false),
        r#"-interpreter-exec console "with print elements 128 -- interpreter-exec mi \"-stack-list-variables --simple-values\"""#
    );

    let inspection = scoped_mi_command("-var-create root * head", 128, true);

    for setting in [
        "may-call-functions",
        "may-write-memory",
        "may-write-registers",
    ] {
        assert!(inspection.contains(&format!("with {setting} off -- ")));
    }

    let nested = parse_stream_output(r#"~"^done,value=\"bounded\"\n""#).unwrap();
    let record = parse_record(nested.trim()).unwrap();

    assert_eq!(
        record.field("value").and_then(MiValue::as_const),
        Some("bounded")
    );
}

#[test]
fn decodes_gdb_console_escape_extensions_as_terminal_escapes() {
    let output = parse_stream_output(r#"~"\e[1m\e[31m[!]\e[0m Heap not initialized\n""#)
        .expect("valid GDB console stream");

    assert_eq!(
        output,
        "\u{1b}[1m\u{1b}[31m[!]\u{1b}[0m Heap not initialized\n"
    );
}
