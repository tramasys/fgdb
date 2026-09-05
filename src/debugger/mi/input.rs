use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
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
    receiver: Option<mpsc::Receiver<VecDeque<DecodedLine>>>,
    records: VecDeque<DecodedLine>,
    retry_at: Instant,
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
            receiver: None,
            records: VecDeque::new(),
            retry_at: Instant::now(),
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

        let weak = self.self_weak.clone();
        let epoch = self.transport_epoch.get();
        let source = glib::timeout_add_local(Duration::from_millis(2), move || {
            let Some(client) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };

            if client.transport_epoch.get() != epoch {
                return glib::ControlFlow::Break;
            }

            let complete = client.poll_input();

            // A delivered record can quarantine or reconnect the client.
            if client.transport_epoch.get() != epoch {
                return glib::ControlFlow::Break;
            }

            if complete {
                client.input_source.borrow_mut().take();
                client.install_read_source();
                if !client.outgoing.borrow().is_empty() {
                    client.ensure_write_source();
                }
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        self.input_source.replace(Some(source));
    }

    fn poll_input(&self) -> bool {
        let epoch = self.transport_epoch.get();
        let mut pending = self.pending_input.borrow_mut();
        let Some(batch) = pending.as_mut() else {
            return true;
        };

        if let Some(bytes) = batch.bytes.as_ref() {
            if Instant::now() < batch.retry_at {
                return false;
            }

            let bytes = Arc::clone(bytes);
            let current = Arc::clone(&batch.current);
            let queued = Arc::clone(&current);
            let (sender, receiver) = mpsc::sync_channel(1);
            match crate::background::submit_cancellable_with_priority(
                crate::background::Priority::Critical,
                move || queued.load(Ordering::Relaxed),
                move || {
                    let records = bytes
                        .split(|byte| *byte == b'\n')
                        .take_while(|_| current.load(Ordering::Relaxed))
                        .map(DecodedLine::parse)
                        .collect();
                    let _ = sender.send(records);
                },
            ) {
                Ok(()) => {
                    batch.bytes = None;
                    batch.receiver = Some(receiver);
                }
                Err(crate::background::SubmitError::QueueFull) => {
                    batch.retry_at = Instant::now() + Duration::from_millis(25);
                    return false;
                }
                Err(error) => {
                    drop(pending);
                    self.report_unusable(format!("Could not parse GDB output: {error}"));
                    return true;
                }
            }
        }

        if let Some(receiver) = batch.receiver.as_ref() {
            match receiver.try_recv() {
                Ok(records) => {
                    batch.records = records;
                    batch.receiver = None;
                }
                Err(mpsc::TryRecvError::Empty) => return false,
                Err(mpsc::TryRecvError::Disconnected) => {
                    drop(pending);
                    self.report_unusable(String::from(
                        "The GDB output parser stopped unexpectedly",
                    ));
                    return true;
                }
            }
        }

        drop(pending);
        let started = Instant::now();

        for _ in 0..128 {
            let record = self
                .pending_input
                .borrow_mut()
                .as_mut()
                .and_then(|batch| batch.records.pop_front());
            let Some(record) = record else {
                self.pending_input.borrow_mut().take();
                return true;
            };

            self.process_decoded_line(record);

            if self.transport_epoch.get() != epoch || started.elapsed() >= MAX_MI_READ_BATCH_TIME {
                return false;
            }
        }

        false
    }
}
