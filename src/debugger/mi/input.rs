use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(super) const INLINE_INPUT_BYTES: usize = 32 * 1024;

pub(super) enum DecodedLine {
    Prompt,
    Stream {
        kind: u8,
        output: String,
        nested: Option<MiRecord>,
    },
    Record(MiRecord),
    Invalid {
        header: MiRecordHeader,
        error: String,
        state: bool,
    },
    Oversized(MiRecordHeader),
    Ignored,
}

impl DecodedLine {
    pub(super) fn parse(bytes: &[u8]) -> Self {
        if bytes.len() > MAX_MI_RECORD_BYTES {
            return Self::Oversized(mi_record_header(bytes));
        }

        let line = String::from_utf8_lossy(bytes);
        let line = line.trim();

        if line.is_empty() {
            return Self::Ignored;
        }

        if line == "(gdb)" {
            return Self::Prompt;
        }

        if let Some(&kind @ (b'~' | b'&' | b'@')) = line.as_bytes().first() {
            return match parse_any_stream_output(line) {
                Ok(output) => {
                    let nested = (kind == b'~'
                        && mi_record_header(output.trim().as_bytes()).kind == Some(b'^'))
                    .then(|| parse_record(output.trim()).ok())
                    .flatten();
                    Self::Stream {
                        kind,
                        output,
                        nested,
                    }
                }
                Err(_) => Self::Ignored,
            };
        }

        match parse_record(line) {
            Ok(record) => Self::Record(record),
            Err(error) => Self::Invalid {
                header: mi_record_header(line.as_bytes()),
                error,
                state: looks_like_mi_record(line),
            },
        }
    }
}

pub(super) struct PendingInput {
    bytes: Option<Arc<Vec<u8>>>,
    current: Arc<AtomicBool>,
    records: VecDeque<DecodedLine>,
}

impl Drop for PendingInput {
    fn drop(&mut self) {
        self.current.store(false, Ordering::Relaxed);
    }
}

impl MiClient {
    pub(super) fn defer_input(&self, bytes: Vec<u8>) {
        debug_assert!(self.pending_input.borrow().is_none());
        self.pending_input.replace(Some(PendingInput {
            bytes: Some(Arc::new(bytes)),
            current: Arc::new(AtomicBool::new(true)),
            records: VecDeque::new(),
        }));

        // Stop reading until this batch is applied. The PTY provides bounded
        // backpressure, and later stop/result records cannot overtake it.
        if let Some(source) = self.read_source.borrow_mut().take() {
            source.remove();
        }

        // Callbacks may queue more commands, but do not write them ahead of
        // state records that are already in this batch and not yet applied.
        if let Some(source) = self.write_source.borrow_mut().take() {
            source.remove();
        }

        self.start_input();
    }

    fn start_input(&self) {
        let (bytes, current) = {
            let pending = self.pending_input.borrow();
            let Some(batch) = pending.as_ref() else {
                return;
            };

            let Some(bytes) = batch.bytes.as_ref() else {
                return;
            };

            (Arc::clone(bytes), Arc::clone(&batch.current))
        };

        let queued = Arc::clone(&current);
        let parsing = Arc::clone(&current);
        let result = crate::background::submit_cancellable_result(
            crate::background::Priority::Critical,
            move || queued.load(Ordering::Relaxed),
            move || {
                bytes
                    .split(|byte| *byte == b'\n')
                    .take_while(|_| parsing.load(Ordering::Relaxed))
                    .map(DecodedLine::parse)
                    .collect::<VecDeque<_>>()
            },
        );

        let receiver = match result {
            Ok(receiver) => receiver,
            Err(crate::background::SubmitError::QueueFull) => {
                let weak = self.self_weak.clone();
                let source = glib::timeout_add_local_once(Duration::from_millis(25), move || {
                    if let Some(client) = weak.upgrade() {
                        client.input_source.borrow_mut().take();
                        client.start_input();
                    }
                });

                self.input_source.replace(Some(source));
                return;
            }
            Err(error) => {
                self.report_unusable(format!("Could not parse GDB output: {error}"));
                return;
            }
        };

        self.pending_input.borrow_mut().as_mut().unwrap().bytes = None;
        let weak = self.self_weak.clone();
        glib::spawn_future_local(async move {
            let result = receiver.await;
            let Some(client) = weak.upgrade().filter(|_| current.load(Ordering::Relaxed)) else {
                return;
            };

            match result {
                Ok(records) => {
                    client.pending_input.borrow_mut().as_mut().unwrap().records = records;
                    client.drain_input();
                }
                Err(_) => client
                    .report_unusable(String::from("The GDB output parser stopped unexpectedly")),
            }
        });
    }

    fn drain_input(&self) {
        let epoch = self.transport_epoch.get();
        let started = Instant::now();

        for _ in 0..128 {
            let record = self
                .pending_input
                .borrow_mut()
                .as_mut()
                .and_then(|batch| batch.records.pop_front());
            let Some(record) = record else {
                self.pending_input.borrow_mut().take();
                self.install_read_source();

                if !self.outgoing.borrow().is_empty() {
                    self.ensure_write_source();
                }

                return;
            };

            self.process_decoded_line(record);

            if self.transport_epoch.get() != epoch || self.pending_input.borrow().is_none() {
                return;
            }

            if started.elapsed() >= MAX_MI_READ_BATCH_TIME {
                break;
            }
        }

        // Preserve the delivery budget and default priority under a busy GTK
        // loop. Only long ready batches yield; worker completion never polls.
        let weak = self.self_weak.clone();
        let source = glib::timeout_add_local_once(Duration::from_millis(2), move || {
            if let Some(client) = weak.upgrade() {
                client.input_source.borrow_mut().take();
                client.drain_input();
            }
        });

        self.input_source.replace(Some(source));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_input_preserves_order_and_rejects_a_retired_batch() {
        let _guard = super::super::tests::MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = glib::MainContext::default();

        context
            .with_thread_default(|| {
                context.block_on(async {
                    let (transport, peer) = super::super::test_transport().unwrap();
                    let peers = Rc::new(RefCell::new(vec![peer]));
                    let retained = Rc::clone(&peers);
                    let client = MiClient::from_transport(
                        transport,
                        Rc::new(move || {
                            let (transport, peer) = super::super::test_transport()?;
                            retained.borrow_mut().push(peer);
                            Ok(transport)
                        }),
                        |_, _| {},
                    );
                    let (sender, receiver) = futures_channel::oneshot::channel();
                    let token = client
                        .request("-thread-info", move |_, record| {
                            sender.send(record.class).unwrap();
                        })
                        .unwrap();

                    // More than one delivery slice, followed by the authoritative result.
                    let input = format!("{}{token}^done\n", "~\"output\"\n".repeat(300));
                    client.defer_input(input.into_bytes());
                    assert!(
                        client
                            .pending_input
                            .borrow()
                            .as_ref()
                            .unwrap()
                            .bytes
                            .is_none()
                    );
                    assert!(client.read_source.borrow().is_none());
                    assert_eq!(
                        glib::future_with_timeout(Duration::from_secs(2), receiver)
                            .await
                            .unwrap()
                            .unwrap(),
                        "done",
                    );

                    assert!(client.pending_input.borrow().is_none());
                    assert!(client.read_source.borrow().is_some());

                    client.defer_input(b"^done\n".to_vec());
                    let retired =
                        Arc::clone(&client.pending_input.borrow().as_ref().unwrap().current);
                    client.reconnect().unwrap();
                    assert!(!retired.load(Ordering::Relaxed));
                    glib::timeout_future(Duration::from_millis(20)).await;
                    assert!(client.pending_input.borrow().is_none());
                    assert!(client.read_source.borrow().is_some());
                });
            })
            .unwrap();
    }
}
