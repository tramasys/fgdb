use super::*;
use std::os::unix::net::UnixStream;

fn with_client(test: impl FnOnce(Rc<MiClient>, &mut UnixStream)) {
    let _lock = tests::MI_CLIENT_TEST_LOCK.lock().unwrap();
    glib::MainContext::new()
        .with_thread_default(|| {
            let (transport, mut peer) = test_transport().unwrap();
            let client = MiClient::from_transport(transport, Rc::new(open_transport), |_, _| {});
            client.ready.set(true);
            test(client, &mut peer);
        })
        .unwrap();
}

fn write_ready(client: &Rc<MiClient>, peer: &mut UnixStream) -> String {
    // Drive the callback directly, without leaving its registered GLib source
    // behind when it returns Break outside the main loop.
    if let Some(source) = client.write_source.borrow_mut().take() {
        source.remove();
    }

    MiClient::on_write_ready(&Rc::downgrade(client), glib::IOCondition::OUT);
    let mut output = String::new();

    if let Err(error) = peer.read_to_string(&mut output) {
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    }

    output
}

fn iterate_until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(
            Instant::now() < deadline,
            "MI main-loop work did not complete"
        );
        glib::MainContext::default().iteration(false);
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn breakpoint_failure_quarantine_then_restart_retires_glib_sources() {
    use std::io::Write;

    with_client(|client, peer| {
        let token = client
            .request("-break-insert rust_fixture.rs:51", |client, record| {
                assert_eq!(record.class, "error");
                client.quarantine("breakpoint backend failure");
            })
            .unwrap();
        write_ready(&client, peer);
        peer.write_all(format!("{token}^error,msg=\"backend failure\"\n").as_bytes())
            .unwrap();
        iterate_until(|| client.quarantined.get());

        assert!(client.read_source.borrow().is_none());
        assert!(client.write_source.borrow().is_none());
        assert!(client.timeout_source.borrow().is_none());
        client.reconnect().unwrap();
        assert!(client.read_source.borrow().is_some());
        assert!(!client.quarantined.get());
    });
}

#[test]
fn deferred_input_preserves_order_and_cannot_publish_into_a_replacement_transport() {
    with_client(|client, peer| {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let first = Rc::clone(&calls);
        let one = client
            .request("-thread-info", move |_, record| {
                assert!(record.is_done());
                first.borrow_mut().push(1);
            })
            .unwrap();
        let second = Rc::clone(&calls);
        let two = client
            .request("-thread-info", move |_, record| {
                assert!(record.is_done());
                second.borrow_mut().push(2);
            })
            .unwrap();
        write_ready(&client, peer);
        let padding = "x".repeat(input::INLINE_INPUT_BYTES * 2);
        client.consume(format!("{one}^done,value=\"{padding}\"\n{two}^done\n").as_bytes());
        assert!(calls.borrow().is_empty());
        assert!(client.read_source.borrow().is_none());
        client.request("-thread-info", |_, _| {}).unwrap();
        assert!(client.write_source.borrow().is_none());
        iterate_until(|| client.pending_input.borrow().is_none());
        assert_eq!(*calls.borrow(), [1, 2]);
        assert!(client.read_source.borrow().is_some());
        assert!(client.write_source.borrow().is_some());

        client.consume(format!("*stopped,reason=\"{padding}\"\n").as_bytes());
        client.reconnect().unwrap();
        assert!(client.pending_input.borrow().is_none());
        assert!(client.input_source.borrow().is_none());
        assert!(client.read_source.borrow().is_some());
    });
}

#[test]
fn variable_cleanup_retains_ownership_until_background_capacity_returns() {
    with_client(|client, peer| {
        let mut tokens = Vec::new();
        for _ in 0..MAX_ACTIVE_BACKGROUND_REQUESTS {
            tokens.push(
                client
                    .request_inner(
                        "-thread-info",
                        CommandClass::Background,
                        None,
                        None,
                        Box::new(|_, _| {}),
                    )
                    .unwrap(),
            );
        }
        write_ready(&client, peer);
        client.delete_variable_object(String::from("var-owned"));
        client.delete_variable_object(String::from("var-owned"));
        assert_eq!(client.deferred_variable_deletions.borrow().len(), 1);
        assert!(write_ready(&client, peer).is_empty());

        client.process_line(&format!("{}^done", tokens[0]));
        let wire = write_ready(&client, peer);
        assert_eq!(wire.matches("-var-delete").count(), 1);
        assert!(wire.contains("var-owned"));
        assert!(client.deferred_variable_deletions.borrow().is_empty());

        client.delete_variable_object(String::from("old-backend"));
        client.reconnect().unwrap();
        assert!(client.deferred_variable_deletions.borrow().is_empty());
    });
}

#[test]
fn stale_on_admission_completes_once_without_writing() {
    with_client(|client, peer| {
        let calls = Rc::new(Cell::new(0));
        let response = Rc::clone(&calls);
        client
            .request_when(
                "-data-evaluate-expression stale",
                || false,
                move |_, record| {
                    assert_eq!(record.class, "superseded");
                    response.set(response.get() + 1);
                },
            )
            .unwrap();

        assert!(write_ready(&client, peer).is_empty());
        assert_eq!(calls.get(), 1);
        assert!(client.pending.borrow().is_empty());
    });
}

#[test]
fn revalidates_unwritten_requests_and_allows_reentrant_callbacks() {
    with_client(|client, peer| {
        let current = Rc::new(Cell::new(true));
        let guard = Rc::clone(&current);
        let weak = Rc::downgrade(&client);
        client
            .request_when(
                "-data-evaluate-expression stale",
                move || {
                    // A guard must not run while the client holds its pending-map borrow.
                    drop(weak.upgrade().unwrap().pending.borrow_mut());
                    guard.get()
                },
                |client, record| {
                    assert_eq!(record.class, "superseded");
                    client.request("-thread-info", |_, _| {}).unwrap();
                },
            )
            .unwrap();
        current.set(false);

        iterate_until(|| client.outgoing.borrow().is_empty());
        assert!(client.write_source.borrow().is_none());
        let wire = write_ready(&client, peer);
        assert!(!wire.contains("stale"));
        assert!(wire.contains("-thread-info"));
    });
}

#[test]
fn revalidates_inspection_waiting_for_a_dispatch_credit() {
    with_client(|client, peer| {
        for _ in 0..MAX_ACTIVE_INSPECTION_REQUESTS {
            client
                .request_when("-thread-info", || true, |_, _| {})
                .unwrap();
        }

        let current = Rc::new(Cell::new(true));
        let guard = Rc::clone(&current);
        let token = client
            .request_when(
                "-data-evaluate-expression stale",
                move || guard.get(),
                |_, record| {
                    assert_eq!(record.class, "superseded");
                },
            )
            .unwrap();
        assert!(client.pending.borrow()[&token].started_at.is_none());
        write_ready(&client, peer);
        current.set(false);
        client.process_line("1^done");

        assert!(!client.pending.borrow().contains_key(&token));
        assert!(write_ready(&client, peer).is_empty());
    });
}

#[test]
fn rejected_requests_never_invoke_their_callback() {
    with_client(|client, _| {
        client.quarantine("test");
        let calls = Rc::new(Cell::new(0));
        let response = Rc::clone(&calls);
        assert!(
            client
                .request("-thread-info", move |_, _| response.set(response.get() + 1))
                .is_err()
        );
        assert_eq!(calls.get(), 0);
    });
}

#[test]
fn sent_stale_requests_keep_credits_until_their_real_result() {
    with_client(|client, peer| {
        let calls = Rc::new(Cell::new(0));

        for _ in 0..MAX_ACTIVE_INSPECTION_REQUESTS {
            let response = Rc::clone(&calls);
            client
                .request_for_stop(
                    "-thread-info",
                    1,
                    || true,
                    move |_, record| {
                        assert_eq!(record.class, "superseded");
                        response.set(response.get() + 1);
                    },
                )
                .unwrap();
        }

        write_ready(&client, peer);
        client.cancel_stale_stop_requests(2);
        let next = client
            .request_for_stop("-thread-info", 2, || true, |_, _| {})
            .unwrap();
        assert!(client.pending.borrow()[&next].started_at.is_none());
        assert_eq!(calls.get(), MAX_ACTIVE_INSPECTION_REQUESTS);
        assert!(write_ready(&client, peer).is_empty());

        client.process_line("1^done");
        assert!(client.pending.borrow()[&next].started_at.is_some());
        assert_eq!(calls.get(), MAX_ACTIVE_INSPECTION_REQUESTS);
    });
}

#[test]
fn a_sent_stale_request_still_has_an_enforced_deadline() {
    with_client(|client, peer| {
        let calls = Rc::new(Cell::new(0));
        let response = Rc::clone(&calls);
        let token = client
            .request_for_stop(
                "-thread-info",
                1,
                || true,
                move |_, _| {
                    response.set(response.get() + 1);
                },
            )
            .unwrap();
        write_ready(&client, peer);
        client.cancel_stale_stop_requests(2);
        client
            .pending
            .borrow_mut()
            .get_mut(&token)
            .unwrap()
            .deadline = Instant::now();
        client.expire_requests();

        assert!(client.quarantined.get());
        assert_eq!(calls.get(), 1);
    });
}

#[test]
fn partial_commands_are_retired_without_removing_their_remaining_bytes() {
    with_client(|client, _| {
        let token = client
            .request_for_stop(
                "-thread-info",
                1,
                || true,
                |_, record| {
                    assert_eq!(record.class, "superseded");
                },
            )
            .unwrap();
        client.outgoing.borrow_mut().advance(1);
        client.cancel_stale_stop_requests(2);
        assert!(client.pending.borrow()[&token].handler.is_none());
        let outgoing = client.outgoing.borrow();
        assert_eq!(outgoing.commands.front().unwrap().token, token);
        assert_eq!(outgoing.commands.front().unwrap().written, 1);
    });
}

#[test]
fn a_timeout_callback_can_replace_the_transport_without_quarantining_its_replacement() {
    with_client(|client, peer| {
        let token = client
            .request("-thread-info", |client, record| {
                assert_eq!(record.class, "timeout");
                client.reconnect().unwrap();
            })
            .unwrap();
        write_ready(&client, peer);
        client
            .pending
            .borrow_mut()
            .get_mut(&token)
            .unwrap()
            .deadline = Instant::now();
        let epoch = client.transport_epoch.get();
        client.expire_requests();
        assert_ne!(client.transport_epoch.get(), epoch);
        assert!(!client.quarantined.get());
    });
}

#[test]
fn stale_scoped_commands_are_checked_again_at_the_write_boundary() {
    with_client(|client, peer| {
        let current = Rc::new(Cell::new(true));
        let guard = Rc::clone(&current);
        let calls = Rc::new(Cell::new(0));
        let response = Rc::clone(&calls);
        client
            .request_console_when(
                "show directories",
                move || guard.get(),
                move |client, record, _| {
                    assert_eq!(record.class, "superseded");
                    response.set(response.get() + 1);
                    client.request("-thread-info", |_, _| {}).unwrap();
                },
            )
            .unwrap();
        current.set(false);
        let wire = write_ready(&client, peer);
        assert!(!wire.contains("show directories"));
        assert!(wire.contains("-thread-info"));
        assert_eq!(calls.get(), 1);
    });
}

#[test]
fn captured_output_is_isolated_from_ordinary_console_commands() {
    with_client(|client, peer| {
        let before = client
            .request("-interpreter-exec console \"echo before\"", |_, _| {})
            .unwrap();
        let output = Rc::new(RefCell::new(String::new()));
        let response = Rc::clone(&output);
        let capture = client
            .request_console("show directories", move |_, record, output| {
                assert!(record.is_done());
                response.replace(output);
            })
            .unwrap();
        assert!(client.scoped_request.borrow().is_none());
        write_ready(&client, peer);
        client.process_line("~\"before\\n\"");
        client.process_line(&format!("{before}^done"));

        let after = client
            .request("-interpreter-exec console \"echo after\"", |_, _| {})
            .unwrap();
        let wire = write_ready(&client, peer);
        assert!(wire.contains("show directories"));
        assert!(!wire.contains("echo after"));
        client.process_line("~\"directories\\n\"");
        client.process_line(&format!("{capture}^done"));
        assert!(write_ready(&client, peer).contains("echo after"));
        client.process_line("~\"after\\n\"");
        client.process_line(&format!("{after}^done"));
        assert_eq!(&*output.borrow(), "directories\n");
    });
}

#[test]
fn controls_overtake_waiting_background_captures_at_command_boundaries() {
    with_client(|client, peer| {
        let first = client
            .request_console("show directories", |_, _, _| {})
            .unwrap();
        client
            .request_console("show version", |_, _, _| {})
            .unwrap();
        let control = client.request("-thread-info", |_, _| {}).unwrap();
        write_ready(&client, peer);
        client.process_line(&format!("{first}^done"));
        let wire = write_ready(&client, peer);
        assert!(wire.contains("-thread-info"));
        assert!(!wire.contains("show version"));
        client.process_line(&format!("{control}^done"));
        assert!(write_ready(&client, peer).contains("show version"));
    });
}

#[test]
fn execution_can_interrupt_a_capture_without_publishing_misattributed_output() {
    with_client(|client, peer| {
        let capture = client
            .request_console("show directories", |_, record, output| {
                assert_eq!(record.class, "superseded");
                assert!(output.is_empty());
            })
            .unwrap();
        write_ready(&client, peer);
        let deadline = client.scoped_request.borrow().as_ref().unwrap().deadline;
        client
            .request_inner(
                "-exec-interrupt --all",
                CommandClass::Execution,
                None,
                None,
                Box::new(|_, _| {}),
            )
            .unwrap();
        assert!(write_ready(&client, peer).contains("-exec-interrupt"));
        client.process_line("~\"unrelated\\n\"");
        assert_eq!(
            client.scoped_request.borrow().as_ref().unwrap().deadline,
            deadline
        );
        client.process_line(&format!("{capture}^done"));
    });
}
