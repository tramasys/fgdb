use std::{
    collections::VecDeque,
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Condvar, Mutex, OnceLock},
    thread,
};

const BACKGROUND_WORKERS: usize = 3;
const BACKGROUND_QUEUE_CAPACITY: usize = 24;
const RESERVED_CRITICAL_SLOTS: usize = 6;
type Job = Box<dyn FnOnce() + Send + 'static>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Priority {
    Critical,
    Interactive,
    #[default]
    Background,
}

impl Priority {
    const fn index(self) -> usize {
        match self {
            Self::Critical => 0,
            Self::Interactive => 1,
            Self::Background => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubmitError {
    QueueFull,
    Disconnected,
    Unavailable,
}

impl fmt::Display for SubmitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::QueueFull => "the background work queue is full",
            Self::Disconnected => "the background workers are unavailable",
            Self::Unavailable => "the background worker pool could not be started",
        })
    }
}

struct QueuedJob {
    priority: Priority,
    is_current: Box<dyn Fn() -> bool + Send + 'static>,
    run: Job,
}

struct QueueState {
    queues: [VecDeque<QueuedJob>; 3],
    running: [usize; 3],
    queued: usize,
    closed: bool,
}

impl Default for QueueState {
    fn default() -> Self {
        Self {
            queues: std::array::from_fn(|_| VecDeque::new()),
            running: [0; 3],
            queued: 0,
            closed: false,
        }
    }
}

struct SharedQueue {
    state: Mutex<QueueState>,
    ready: Condvar,
    capacity: usize,
    reserved_critical: usize,
    max_running_noncritical: usize,
}

struct WorkerPool {
    shared: Arc<SharedQueue>,
    _workers: Vec<thread::JoinHandle<()>>,
}

impl WorkerPool {
    fn new(worker_count: usize, queue_capacity: usize) -> std::io::Result<Self> {
        let shared = Arc::new(SharedQueue {
            state: Mutex::new(QueueState::default()),
            ready: Condvar::new(),
            capacity: queue_capacity,
            reserved_critical: RESERVED_CRITICAL_SLOTS.min(queue_capacity.saturating_sub(1)),
            max_running_noncritical: worker_count.saturating_sub(1).max(1),
        });

        let mut workers = Vec::with_capacity(worker_count);

        for index in 0..worker_count {
            let shared = Arc::clone(&shared);

            workers.push(
                thread::Builder::new()
                    .name(format!("fgdb-worker-{index}"))
                    .spawn(move || worker_loop(&shared))?,
            );
        }

        Ok(Self {
            shared,
            _workers: workers,
        })
    }

    fn submit_with_priority(
        &self,
        priority: Priority,
        job: impl FnOnce() + Send + 'static,
    ) -> Result<(), SubmitError> {
        self.submit_cancellable_with_priority(priority, || true, job)
    }

    fn submit_cancellable_with_priority(
        &self,
        priority: Priority,
        is_current: impl Fn() -> bool + Send + 'static,
        job: impl FnOnce() + Send + 'static,
    ) -> Result<(), SubmitError> {
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| SubmitError::Disconnected)?;

        if state.closed {
            return Err(SubmitError::Disconnected);
        }

        let ordinary_capacity = self
            .shared
            .capacity
            .saturating_sub(self.shared.reserved_critical);

        if state.queued >= self.shared.capacity
            || (priority != Priority::Critical && state.queued >= ordinary_capacity)
        {
            return Err(SubmitError::QueueFull);
        }

        state.queues[priority.index()].push_back(QueuedJob {
            priority,
            is_current: Box::new(is_current),
            run: Box::new(job),
        });

        state.queued += 1;
        drop(state);
        self.shared.ready.notify_one();

        Ok(())
    }

    #[cfg(test)]
    fn submit(&self, job: impl FnOnce() + Send + 'static) -> Result<(), SubmitError> {
        self.submit_with_priority(Priority::Background, job)
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.closed = true;
        }

        self.shared.ready.notify_all();
    }
}

fn worker_loop(shared: &SharedQueue) {
    loop {
        let job = {
            let mut state = match shared.state.lock() {
                Ok(state) => state,
                Err(_) => return,
            };

            loop {
                // Filesystem searches are interactive work too. Reserve a
                // worker for transport parsing even while those reads block.
                let noncritical_available = state.running[Priority::Interactive.index()]
                    + state.running[Priority::Background.index()]
                    < shared.max_running_noncritical;
                let next = state.queues[Priority::Critical.index()]
                    .pop_front()
                    .or_else(|| {
                        if noncritical_available {
                            state.queues[Priority::Interactive.index()]
                                .pop_front()
                                .or_else(|| state.queues[Priority::Background.index()].pop_front())
                        } else {
                            None
                        }
                    });

                if let Some(job) = next {
                    state.queued = state.queued.saturating_sub(1);
                    state.running[job.priority.index()] += 1;
                    break job;
                }

                if state.closed {
                    return;
                }

                state = match shared.ready.wait(state) {
                    Ok(state) => state,
                    Err(_) => return,
                };
            }
        };

        let QueuedJob {
            priority,
            is_current,
            run,
        } = job;

        let _ = catch_unwind(AssertUnwindSafe(|| {
            if is_current() {
                run();
            }
        }));

        if let Ok(mut state) = shared.state.lock() {
            state.running[priority.index()] = state.running[priority.index()].saturating_sub(1);
        }

        shared.ready.notify_all();
    }
}

static BACKGROUND_POOL: OnceLock<Result<WorkerPool, String>> = OnceLock::new();

pub(crate) fn submit_with_priority(
    priority: Priority,
    job: impl FnOnce() + Send + 'static,
) -> Result<(), SubmitError> {
    match BACKGROUND_POOL.get_or_init(|| {
        WorkerPool::new(BACKGROUND_WORKERS, BACKGROUND_QUEUE_CAPACITY)
            .map_err(|error| error.to_string())
    }) {
        Ok(pool) => pool.submit_with_priority(priority, job),
        Err(_) => Err(SubmitError::Unavailable),
    }
}

/// Like [`submit_with_priority`], but stale queued work is discarded before it
/// occupies a worker. Long-running jobs should still check the same generation
/// cooperatively inside their own loops.
pub(crate) fn submit_cancellable_with_priority(
    priority: Priority,
    is_current: impl Fn() -> bool + Send + 'static,
    job: impl FnOnce() + Send + 'static,
) -> Result<(), SubmitError> {
    match BACKGROUND_POOL.get_or_init(|| {
        WorkerPool::new(BACKGROUND_WORKERS, BACKGROUND_QUEUE_CAPACITY)
            .map_err(|error| error.to_string())
    }) {
        Ok(pool) => pool.submit_cancellable_with_priority(priority, is_current, job),
        Err(_) => Err(SubmitError::Unavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::{Priority, SubmitError, WorkerPool};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        time::Duration,
    };

    #[test]
    fn bounded_pool_reserves_capacity_for_critical_work() {
        let pool = WorkerPool::new(1, 2).unwrap();
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();

        pool.submit(move || {
            started_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
        })
        .unwrap();

        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        pool.submit(|| {}).unwrap();
        assert_eq!(pool.submit(|| {}), Err(SubmitError::QueueFull));
        assert!(pool.submit_with_priority(Priority::Critical, || {}).is_ok());
        release_sender.send(()).unwrap();

        let pool = WorkerPool::new(3, 8).unwrap();
        let (started_sender, started_receiver) = mpsc::channel();
        let mut releases: Vec<mpsc::Sender<()>> = Vec::new();
        for (index, priority) in [
            Priority::Background,
            Priority::Interactive,
            Priority::Interactive,
        ]
        .into_iter()
        .enumerate()
        {
            let (release, wait) = mpsc::channel();
            releases.push(release);
            let started = started_sender.clone();
            pool.submit_with_priority(priority, move || {
                started.send(()).unwrap();
                let _ = wait.recv();
            })
            .unwrap();
            if index < 2 {
                started_receiver
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap();
            }
        }
        assert_eq!(
            started_receiver.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        let (critical, completed) = mpsc::channel();
        pool.submit_with_priority(Priority::Critical, move || {
            let _ = critical.send(());
        })
        .unwrap();
        completed.recv_timeout(Duration::from_secs(1)).unwrap();
        drop(releases);
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
    }

    #[test]
    fn critical_work_overtakes_queued_background_work() {
        let pool = WorkerPool::new(1, 8).unwrap();
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();

        pool.submit(move || {
            started_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
        })
        .unwrap();

        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let (order_sender, order_receiver) = mpsc::channel();
        let background_sender = order_sender.clone();

        pool.submit(move || background_sender.send("background").unwrap())
            .unwrap();

        pool.submit_with_priority(Priority::Critical, move || {
            order_sender.send("critical").unwrap();
        })
        .unwrap();
        release_sender.send(()).unwrap();

        assert_eq!(
            order_receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            "critical"
        );
    }

    #[test]
    fn panicking_jobs_do_not_remove_the_worker() {
        let pool = WorkerPool::new(1, 8).unwrap();
        let (job_started_sender, job_started_receiver) = mpsc::channel();

        pool.submit(move || {
            job_started_sender.send(()).unwrap();
            panic!("test worker panic");
        })
        .unwrap();

        job_started_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let (predicate_started_sender, predicate_started_receiver) = mpsc::channel();

        pool.submit_cancellable_with_priority(
            Priority::Interactive,
            move || {
                predicate_started_sender.send(()).unwrap();
                panic!("test cancellation predicate panic");
            },
            || {},
        )
        .unwrap();

        predicate_started_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let (sender, receiver) = mpsc::channel();

        pool.submit_with_priority(Priority::Critical, move || sender.send(()).unwrap())
            .unwrap();

        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn cancelled_queued_work_does_not_occupy_a_worker() {
        let pool = WorkerPool::new(1, 8).unwrap();
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();

        pool.submit(move || {
            started_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
        })
        .unwrap();

        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let current = Arc::new(AtomicBool::new(true));
        let ran = Arc::new(AtomicBool::new(false));
        let current_for_job = Arc::clone(&current);
        let ran_for_job = Arc::clone(&ran);

        pool.submit_cancellable_with_priority(
            Priority::Interactive,
            move || current_for_job.load(Ordering::Relaxed),
            move || ran_for_job.store(true, Ordering::Relaxed),
        )
        .unwrap();
        current.store(false, Ordering::Relaxed);
        let (finished_sender, finished_receiver) = mpsc::channel();

        pool.submit_with_priority(Priority::Interactive, move || {
            finished_sender.send(()).unwrap();
        })
        .unwrap();
        release_sender.send(()).unwrap();

        finished_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        assert!(!ran.load(Ordering::Relaxed));
    }
}
