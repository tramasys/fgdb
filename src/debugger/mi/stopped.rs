use super::*;
use crate::debugger::StopContext;
use std::borrow::Cow;

/// Requests bound to one immutable stop and one backend incarnation.
///
/// The supplied authority validates model selection without depending on GTK.
/// A clone retains the same identity, it never rebinds to a newer stop.
#[derive(Clone)]
pub(crate) struct StopRequests(Rc<BoundStop>);

struct BoundStop {
    client: Weak<MiClient>,
    context: StopContext,
    is_current: Box<dyn Fn(&StopContext) -> bool>,
}

impl MiClient {
    pub(crate) fn bind_stop_requests(
        &self,
        context: StopContext,
        is_current: impl Fn(&StopContext) -> bool + 'static,
    ) -> StopRequests {
        StopRequests(Rc::new(BoundStop {
            client: self.weak(),
            context,
            is_current: Box::new(is_current),
        }))
    }
}

impl StopRequests {
    pub(crate) fn generation(&self) -> u64 {
        self.0.context.generation()
    }

    pub(crate) fn is_current(&self) -> bool {
        self.0.client.upgrade().is_some_and(|client| {
            client.is_ready()
                && client.transport_epoch() == self.0.context.transport_epoch()
                && (self.0.is_current)(&self.0.context)
        })
    }

    pub(crate) fn frame<'a>(&'a self, command: &'a str) -> StopRequest<'a> {
        self.command(command, Scope::Frame)
    }

    pub(crate) fn thread<'a>(&'a self, command: &'a str) -> StopRequest<'a> {
        self.command(command, Scope::Thread)
    }

    /// For commands with their own object identity or no selection dependency,
    /// such as varobj updates and register names. Stop validity still applies.
    pub(crate) fn unscoped<'a>(&'a self, command: &'a str) -> StopRequest<'a> {
        self.command(command, Scope::Unscoped)
    }

    fn command<'a>(&'a self, command: &'a str, scope: Scope) -> StopRequest<'a> {
        StopRequest {
            requests: self,
            command,
            scope,
            guard: None,
        }
    }
}

#[derive(Clone, Copy)]
enum Scope {
    Frame,
    Thread,
    Unscoped,
}

/// Select scope explicitly, then choose the transport operation.
///
/// As with ordinary MI requests, Err invokes no callback and Ok invokes exactly
/// one terminal callback. Stale work completes with `superseded`, including at
/// submission, so cleanup and batch accounting must still run in the callback.
pub(crate) struct StopRequest<'a> {
    requests: &'a StopRequests,
    command: &'a str,
    scope: Scope,
    guard: Option<Box<dyn Fn() -> bool>>,
}

impl StopRequest<'_> {
    /// Add object, editor, or viewer validity without replacing stop validation.
    pub(crate) fn when(mut self, guard: impl Fn() -> bool + 'static) -> Self {
        let previous = self.guard.take();
        self.guard = Some(Box::new(move || {
            previous.as_ref().is_none_or(|previous| previous()) && guard()
        }));
        self
    }

    fn encoded_command(&self) -> Cow<'_, str> {
        match self.scope {
            Scope::Frame => Cow::Owned(self.requests.0.context.scope_frame(self.command)),
            Scope::Thread => Cow::Owned(self.requests.0.context.scope_thread(self.command)),
            Scope::Unscoped => Cow::Borrowed(self.command),
        }
    }

    fn client(&self) -> io::Result<Rc<MiClient>> {
        validate_mi_command(self.command)?;
        self.requests.0.client.upgrade().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "GDB/MI connection is unavailable",
            )
        })
    }

    fn guard(&mut self) -> impl Fn() -> bool + 'static {
        let requests = self.requests.clone();
        let extra = self.guard.take();

        move || requests.is_current() && extra.as_ref().is_none_or(|guard| guard())
    }

    pub(crate) fn request(
        mut self,
        handler: impl FnOnce(&MiClient, MiRecord) + 'static,
    ) -> io::Result<u64> {
        let client = self.client()?;
        let guard = self.guard();
        client.request_for_stop(
            &self.encoded_command(),
            self.requests.generation(),
            guard,
            handler,
        )
    }

    pub(crate) fn control(
        mut self,
        handler: impl FnOnce(&MiClient, MiRecord) + 'static,
    ) -> io::Result<u64> {
        let client = self.client()?;
        let guard = self.guard();
        client.request_control_for_stop(
            &self.encoded_command(),
            self.requests.generation(),
            guard,
            handler,
        )
    }

    pub(crate) fn with_print_limit(
        mut self,
        elements: usize,
        handler: impl FnOnce(&MiClient, MiRecord) + 'static,
    ) -> io::Result<u64> {
        let client = self.client()?;
        let guard = self.guard();
        client.request_with_print_limit_for_stop(
            &self.encoded_command(),
            elements,
            self.requests.generation(),
            guard,
            handler,
        )
    }

    /// Capture the console output of an encoded MI interpreter command.
    pub(crate) fn capture(
        mut self,
        handler: impl FnOnce(&MiClient, MiRecord, String) + 'static,
    ) -> io::Result<u64> {
        let client = self.client()?;
        let guard = self.guard();
        client.request_console_for_stop(
            &self.encoded_command(),
            self.requests.generation(),
            guard,
            handler,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DebuggerModel;
    use std::os::unix::net::UnixStream;

    fn with_client(test: impl FnOnce(Rc<MiClient>, UnixStream)) {
        let _lock = super::super::tests::MI_CLIENT_TEST_LOCK.lock().unwrap();
        glib::MainContext::new()
            .with_thread_default(|| {
                let (client, peer) = MiClient::open_with_injected_transport(|_, _| {}).unwrap();
                client.ready.set(true);
                test(client, peer);
            })
            .unwrap();
    }

    fn bind(client: &MiClient, model: &Rc<DebuggerModel>) -> StopRequests {
        model.start_stop_refresh();
        let context = model.bind_stop_context(client.transport_epoch()).unwrap();
        let model = Rc::downgrade(model);
        client.bind_stop_requests(context, move |context| {
            model
                .upgrade()
                .is_some_and(|model| model.is_stop_context_current(context))
        })
    }

    fn stopped_model() -> Rc<DebuggerModel> {
        let model = Rc::new(DebuggerModel::new(None));
        model.set_controls_ready(true);
        model.set_inferior_started(true);
        model.set_current_thread_id(Some("7"));
        model.select_frame(2);
        model
    }

    #[test]
    fn scope_and_additional_guards_preserve_the_request_contract() {
        with_client(|client, _peer| {
            let model = stopped_model();
            let requests = bind(&client, &model);
            let frame = requests
                .frame("-stack-info-frame")
                .request(|_, _| {})
                .unwrap();
            let thread = requests
                .thread("-stack-list-frames 0 24")
                .request(|_, _| {})
                .unwrap();
            let object = requests
                .unscoped("-var-info-path-expression value")
                .request(|_, _| {})
                .unwrap();
            let written = client
                .outgoing
                .borrow()
                .commands
                .iter()
                .map(|command| String::from_utf8(command.bytes.clone()).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                written,
                [
                    format!("{frame}-stack-info-frame --thread 7 --frame 2\n"),
                    format!("{thread}-stack-list-frames --thread 7 0 24\n"),
                    format!("{object}-var-info-path-expression value\n"),
                ]
            );

            let result = Rc::new(RefCell::new(Vec::new()));
            let observed = Rc::clone(&result);
            requests
                .frame("-data-evaluate-expression 1")
                .when(|| false)
                .when(|| true)
                .control(move |_, record| observed.borrow_mut().push(record.class))
                .unwrap();
            assert_eq!(result.borrow().as_slice(), ["superseded"]);

            assert!(
                requests
                    .frame("")
                    .request(|_, _| panic!("Err must not invoke a handler"))
                    .is_err()
            );
            assert_eq!(client.outgoing.borrow().commands.len(), 3);
        });
    }

    #[test]
    fn selection_and_backend_changes_revoke_every_bound_request_kind() {
        // Returning to the same selection or execution state must not revive a
        // lease. Backend replacement revokes it before the model sees recovery.
        for change in 0..5 {
            with_client(|client, _peer| {
                let model = stopped_model();
                let requests = bind(&client, &model);
                let results = Rc::new(RefCell::new(Vec::new()));
                let observed = Rc::clone(&results);
                requests
                    .frame("-stack-list-variables --simple-values")
                    .with_print_limit(32, move |_, record| {
                        observed.borrow_mut().push(record.class)
                    })
                    .unwrap();
                let observed = Rc::clone(&results);
                requests
                    .frame("-interpreter-exec console \"print 1\"")
                    .capture(move |_, record, _| observed.borrow_mut().push(record.class))
                    .unwrap();
                let observed = Rc::clone(&results);
                requests
                    .unscoped("-var-assign value 1")
                    .control(move |_, record| observed.borrow_mut().push(record.class))
                    .unwrap();

                match change {
                    0 => {
                        model.select_frame(3);
                        model.select_frame(2);
                    }
                    1 => {
                        model.set_current_thread_id(Some("8"));
                        model.set_current_thread_id(Some("7"));
                    }
                    2 => {
                        model.set_controls_ready(false);
                        model.set_controls_ready(true);
                    }
                    3 => {
                        model.set_controls_running(true);
                        model.set_controls_running(false);
                    }
                    _ => {
                        client.reconnect().unwrap();
                        client.ready.set(true);
                    }
                }

                assert!(!requests.is_current());
                client.cancel_stale_stop_requests(requests.generation().wrapping_add(1));
                assert_eq!(
                    results.borrow().as_slice(),
                    ["superseded", "superseded", "superseded"]
                );
                assert!(client.pending.borrow().is_empty());
                assert!(client.scoped_request.borrow().is_none());
                assert!(client.scoped_queue.borrow().is_empty());

                let observed = Rc::clone(&results);
                requests
                    .frame("-data-evaluate-expression 1")
                    .when(|| true)
                    .request(move |_, record| observed.borrow_mut().push(record.class))
                    .unwrap();
                assert_eq!(results.borrow().len(), 4);
                assert_eq!(results.borrow()[3], "superseded");
                assert!(client.outgoing.borrow().is_empty());
            });
        }
    }

    #[test]
    fn cancelled_sent_leases_keep_wire_credits_until_the_real_reply() {
        with_client(|client, mut peer| {
            let model = stopped_model();
            let requests = bind(&client, &model);
            let results = Rc::new(RefCell::new(Vec::new()));
            let tokens = (0..MAX_ACTIVE_INSPECTION_REQUESTS)
                .map(|_| {
                    let observed = Rc::clone(&results);
                    requests
                        .frame("-stack-info-frame")
                        .request(move |_, record| observed.borrow_mut().push(record.class))
                        .unwrap()
                })
                .collect::<Vec<_>>();
            MiClient::on_write_ready(&client.weak(), glib::IOCondition::OUT);
            peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
            assert!(peer.read(&mut [0; 4096]).unwrap() > 0);

            model.select_frame(3);
            client.cancel_invalid_pending_requests();
            assert_eq!(results.borrow().len(), tokens.len());
            assert!(results.borrow().iter().all(|result| result == "superseded"));
            assert_eq!(client.pending.borrow().len(), tokens.len());

            let current = bind(&client, &model);
            let observed = Rc::clone(&results);
            let next = current
                .frame("-stack-info-frame")
                .request(move |_, record| observed.borrow_mut().push(record.class))
                .unwrap();
            assert!(client.pending.borrow()[&next].started_at.is_none());

            for token in tokens {
                client.process_line(&format!("{token}^done"));
                assert!(client.pending.borrow()[&next].started_at.is_some());
            }

            client.process_line(&format!("{next}^done"));
            assert_eq!(results.borrow().last().map(String::as_str), Some("done"));
            assert_eq!(results.borrow().len(), MAX_ACTIVE_INSPECTION_REQUESTS + 1);
            assert!(client.pending.borrow().is_empty());
            assert!(client.is_ready());
        });
    }
}
