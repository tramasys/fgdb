//! One process owns the shared layout. All filesystem work stays on workers.

use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Saved,
    ReadOnly,
}

enum Lease {
    Unclaimed,
    Owned { _file: File },
    ReadOnly,
    Released,
}

pub(super) struct Writer {
    path: PathBuf,
    gate: Mutex<Lease>,
    finished: AtomicBool,
}

impl Writer {
    pub(super) fn new(path: PathBuf) -> Self {
        Self {
            path,
            gate: Mutex::new(Lease::Unclaimed),
            finished: AtomicBool::new(false),
        }
    }

    pub(super) fn initialize(&self) -> io::Result<Outcome> {
        self.update(None)
    }

    pub(super) fn write(&self, contents: &str) -> io::Result<Outcome> {
        self.update(Some(contents))
    }

    fn update(&self, contents: Option<&str>) -> io::Result<Outcome> {
        let mut lease = self
            .gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if self.finished.load(Ordering::Acquire) {
            return Ok(Outcome::Saved);
        }

        if !self.acquire(&mut lease)? {
            return Ok(Outcome::ReadOnly);
        }

        if let Some(contents) = contents {
            self.replace(contents)?;
        }

        Ok(Outcome::Saved)
    }

    pub(super) fn retire(&self) {
        self.finished.store(true, Ordering::Release);
    }

    pub(super) fn finish(&self, contents: &str) -> io::Result<Outcome> {
        self.retire();
        let mut lease = self
            .gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let result = match self.acquire(&mut lease) {
            Ok(true) => self.replace(contents).map(|()| Outcome::Saved),
            Ok(false) => Ok(Outcome::ReadOnly),
            Err(error) => Err(error),
        };

        // Read-only instances never take over with a stale snapshot. A newly
        // started instance can acquire ownership after this lease is released.
        *lease = Lease::Released;

        result
    }

    fn acquire(&self, lease: &mut Lease) -> io::Result<bool> {
        match lease {
            Lease::Owned { .. } => return Ok(true),
            Lease::ReadOnly | Lease::Released => return Ok(false),
            Lease::Unclaimed => {}
        }

        let parent = self.parent()?;
        fs::create_dir_all(parent)?;

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.path.with_extension("lock"))?;

        match file.try_lock() {
            Ok(()) => *lease = Lease::Owned { _file: file },
            Err(TryLockError::WouldBlock) => *lease = Lease::ReadOnly,
            Err(TryLockError::Error(error)) => return Err(error),
        }

        Ok(matches!(lease, Lease::Owned { .. }))
    }

    fn parent(&self) -> io::Result<&std::path::Path> {
        self.path
            .parent()
            .ok_or_else(|| io::Error::other("Layout has no parent directory"))
    }

    fn replace(&self, contents: &str) -> io::Result<()> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let parent = self.parent()?;

        // Never replace another process's temporary file, including leftovers
        // from a crashed process whose PID has since been reused.
        for _ in 0..32 {
            let temporary = parent.join(format!(
                ".layout.{}.{}.tmp",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));

            let mut file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
            {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };

            let result = (|| {
                file.write_all(contents.as_bytes())?;
                file.sync_all()?;
                fs::rename(&temporary, &self.path)?;
                File::open(parent)?.sync_all()
            })();

            if result.is_err() {
                let _ = fs::remove_file(&temporary);
            }

            return result;
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Could not create a unique layout temporary file",
        ))
    }
}
