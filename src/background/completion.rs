//! Wake on worker completion while retaining deadline and owner checks.

use futures_channel::oneshot;
use gtk::glib;
use std::{
    future::{Future, poll_fn},
    pin::pin,
    task::Poll,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionError {
    Superseded,
    TimedOut,
    Disconnected,
}

/// The owner is checked before accepting even an already completed result.
/// Only cancellation is polled. Results wake the main context immediately.
pub(crate) async fn receive_current<T>(
    receiver: oneshot::Receiver<T>,
    timeout: Duration,
    is_current: impl Fn() -> bool,
) -> Result<T, CompletionError> {
    let started = Instant::now();
    let mut receiver = pin!(receiver);
    let mut check = pin!(glib::timeout_future(timeout.min(Duration::from_millis(20))));

    poll_fn(|context| {
        if !is_current() {
            return Poll::Ready(Err(CompletionError::Superseded));
        }

        let remaining = timeout.saturating_sub(started.elapsed());

        if remaining.is_zero() {
            return Poll::Ready(Err(CompletionError::TimedOut));
        }

        if let Poll::Ready(result) = receiver.as_mut().poll(context) {
            return Poll::Ready(result.map_err(|_| CompletionError::Disconnected));
        }

        if check.as_mut().poll(context).is_ready() {
            check.set(glib::timeout_future(
                remaining.min(Duration::from_millis(20)),
            ));

            let _ = check.as_mut().poll(context);
        }

        Poll::Pending
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn ready_results_do_not_wait_for_the_cancellation_timer() {
        let (sender, receiver) = oneshot::channel();
        sender.send(42).unwrap();
        let mut result = pin!(receive_current(receiver, Duration::from_secs(1), || true));

        assert_eq!(
            result
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop())),
            Poll::Ready(Ok(42))
        );
    }

    #[test]
    fn stale_or_expired_results_are_not_published() {
        for (current, timeout, expected) in [
            (false, Duration::from_secs(1), CompletionError::Superseded),
            (true, Duration::ZERO, CompletionError::TimedOut),
        ] {
            let (sender, receiver) = oneshot::channel();
            sender.send(42).unwrap();
            let mut result = pin!(receive_current(receiver, timeout, || current));

            assert_eq!(
                result
                    .as_mut()
                    .poll(&mut std::task::Context::from_waker(std::task::Waker::noop())),
                Poll::Ready(Err(expected))
            );
        }
    }

    #[test]
    fn pending_workers_time_out_cancel_and_report_lost_senders() {
        let context = glib::MainContext::new();

        context
            .with_thread_default(|| {
                context.block_on(async {
                    let (sender, receiver) = oneshot::channel::<()>();

                    assert_eq!(
                        receive_current(receiver, Duration::from_millis(5), || true).await,
                        Err(CompletionError::TimedOut)
                    );

                    assert!(sender.is_canceled());
                    let (sender, receiver) = oneshot::channel::<()>();
                    let checks = Cell::new(0);

                    assert_eq!(
                        receive_current(receiver, Duration::from_secs(1), || {
                            checks.set(checks.get() + 1);
                            checks.get() == 1
                        })
                        .await,
                        Err(CompletionError::Superseded)
                    );

                    assert!(sender.is_canceled());
                    let (sender, receiver) = oneshot::channel::<()>();
                    drop(sender);

                    assert_eq!(
                        receive_current(receiver, Duration::from_secs(1), || true).await,
                        Err(CompletionError::Disconnected)
                    );
                });
            })
            .unwrap();
    }
}
