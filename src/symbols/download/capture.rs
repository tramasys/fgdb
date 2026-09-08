//! Own both pipe readers, the child wait, and cancellation in one future.
//!
//! Dropping the operation cancels pending GIO reads. Killing the direct child
//! alone is insufficient when a descendant retains an inherited output pipe.

use gtk::{gio, glib, prelude::*};
use std::{
    future::{Future, poll_fn},
    pin::pin,
    task::Poll,
    time::{Duration, Instant},
};

struct RunningProcess<'a> {
    process: &'a gio::Subprocess,
    exited: bool,
}

impl Drop for RunningProcess<'_> {
    fn drop(&mut self) {
        if !self.exited {
            self.process.force_exit();
        }
    }
}

pub(super) async fn output(
    process: &gio::Subprocess,
    is_current: &dyn Fn() -> bool,
    timeout: Duration,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut running = RunningProcess {
        process,
        exited: false,
    };

    let stdout = process
        .stdout_pipe()
        .ok_or("Debuginfod did not expose its result output")?;

    let stderr = process
        .stderr_pipe()
        .ok_or("Debuginfod did not expose its error output")?;

    let mut stdout = pin!(read_bounded(stdout, 16 * 1024, false));
    let mut stderr = pin!(read_bounded(stderr, 32 * 1024, true));
    let mut waited = pin!(process.wait_future());
    let mut cancellation = pin!(cancelled(is_current, timeout));
    let mut result = None;
    let mut diagnostics = None;

    poll_fn(|context| {
        if let Poll::Ready(reason) = cancellation.as_mut().poll(context) {
            return Poll::Ready(Err(reason));
        }

        if result.is_none()
            && let Poll::Ready(value) = stdout.as_mut().poll(context)
        {
            result = Some(value?);
        }

        if diagnostics.is_none()
            && let Poll::Ready(value) = stderr.as_mut().poll(context)
        {
            diagnostics = Some(value?);
        }

        if !running.exited
            && let Poll::Ready(status) = waited.as_mut().poll(context)
        {
            status.map_err(|error| error.to_string())?;
            running.exited = true;
        }

        if running.exited && result.is_some() && diagnostics.is_some() {
            Poll::Ready(Ok((result.take().unwrap(), diagnostics.take().unwrap())))
        } else {
            Poll::Pending
        }
    })
    .await
}

async fn cancelled(is_current: &dyn Fn() -> bool, timeout: Duration) -> String {
    let started = Instant::now();

    loop {
        if !is_current() {
            return String::from("Symbol request cancelled");
        }

        let remaining = timeout.saturating_sub(started.elapsed());

        if remaining.is_zero() {
            return format!(
                "Debug information download timed out after {} seconds",
                timeout.as_secs()
            );
        }

        glib::timeout_future(remaining.min(Duration::from_millis(100))).await;
    }
}

async fn read_bounded(
    stream: gio::InputStream,
    limit: usize,
    truncate: bool,
) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut truncated = false;

    loop {
        let bytes = stream
            .read_bytes_future(4096, glib::Priority::DEFAULT)
            .await
            .map_err(|error| error.to_string())?;

        if bytes.is_empty() {
            if truncated {
                output.extend_from_slice(b"\nFurther diagnostics omitted");
            }

            return Ok(output);
        }

        if output.len().saturating_add(bytes.len()) > limit {
            if !truncate {
                return Err(String::from("Debuginfod output exceeded its safety limit"));
            }

            output.extend_from_slice(&bytes[..limit.saturating_sub(output.len())]);
            truncated = true;
        } else {
            output.extend_from_slice(&bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, ffi::OsStr};

    fn child(script: &str) -> gio::Subprocess {
        gio::Subprocess::newv(
            &[OsStr::new("sh"), OsStr::new("-c"), OsStr::new(script)],
            gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_PIPE,
        )
        .unwrap()
    }

    #[test]
    fn inherited_pipes_cannot_prevent_timeout_or_cancellation() {
        let context = glib::MainContext::new();

        context
            .with_thread_default(|| {
                context.block_on(async {
                    let started = Instant::now();
                    let process = child("sleep 2 & printf result");
                    let error = output(&process, &|| true, Duration::from_millis(40))
                        .await
                        .unwrap_err();

                    assert!(error.contains("timed out"));
                    assert!(started.elapsed() < Duration::from_secs(1));
                    let process = child("sleep 2 & printf result");
                    let checks = Cell::new(0);

                    let error = output(
                        &process,
                        &|| {
                            checks.set(checks.get() + 1);
                            checks.get() < 2
                        },
                        Duration::from_secs(5),
                    )
                    .await
                    .unwrap_err();

                    assert!(error.contains("cancelled"));
                    assert!(started.elapsed() < Duration::from_secs(1));
                })
            })
            .unwrap();
    }

    #[test]
    fn dropping_capture_terminates_the_owned_child() {
        let context = glib::MainContext::new();

        context
            .with_thread_default(|| {
                context.block_on(async {
                    let process = child("exec sleep 10");

                    {
                        let mut capture = pin!(output(&process, &|| true, Duration::from_secs(10)));

                        poll_fn(|context| {
                            assert!(capture.as_mut().poll(context).is_pending());
                            Poll::Ready(())
                        })
                        .await;
                    }

                    process.wait_future().await.unwrap();
                    assert!(process.has_signaled());
                })
            })
            .unwrap();
    }

    #[test]
    fn both_streams_are_drained_and_result_output_remains_bounded() {
        let context = glib::MainContext::new();

        context
            .with_thread_default(|| {
                context.block_on(async {
                    let process = child("head -c 65536 /dev/zero >&2; printf result");
                    let (result, errors) = output(&process, &|| true, Duration::from_secs(2))
                        .await
                        .unwrap();

                    assert_eq!(result, b"result");
                    assert!(errors.ends_with(b"Further diagnostics omitted"));
                    assert!(errors.len() < 33 * 1024);
                    assert!(process.is_successful());
                    let process = child("head -c 16385 /dev/zero; sleep 2");
                    let error = output(&process, &|| true, Duration::from_secs(2))
                        .await
                        .unwrap_err();

                    assert!(error.contains("safety limit"));
                })
            })
            .unwrap();
    }
}
