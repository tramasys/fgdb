//! Cancellable debuginfod transfers do not occupy GDB's command queue. The
//! standard client owns its cache and server protocol, including cached misses.

use std::{
    cell::Cell,
    ffi::OsStr,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

use super::backend::Configuration;
use gtk::{gio, glib, prelude::*};

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);

struct Download {
    process: gio::Subprocess,
    monitor: Option<glib::SourceId>,
    finished: bool,
}

impl Drop for Download {
    fn drop(&mut self) {
        if let Some(source) = self.monitor.take() {
            source.remove();
        }

        if !self.finished {
            self.process.force_exit();
        }
    }
}

async fn read_bounded(
    stream: gio::InputStream,
    process: gio::Subprocess,
    limit: usize,
    truncate: bool,
) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut truncated = false;

    loop {
        let bytes = match stream
            .read_bytes_future(4096, glib::Priority::DEFAULT)
            .await
        {
            Ok(bytes) => bytes,
            Err(error) => {
                process.force_exit();
                return Err(error.to_string());
            }
        };

        if bytes.is_empty() {
            if truncated {
                output.extend_from_slice(b"\nFurther diagnostics omitted");
            }

            return Ok(output);
        }

        if output.len().saturating_add(bytes.len()) > limit {
            if truncate {
                output.extend_from_slice(&bytes[..limit.saturating_sub(output.len())]);
                truncated = true;
                continue;
            }

            process.force_exit();
            return Err(String::from("Debuginfod output exceeded its safety limit"));
        }

        output.extend_from_slice(&bytes);
    }
}

pub(super) async fn fetch(
    build_id: &str,
    configuration: &Configuration,
    is_current: Rc<dyn Fn() -> bool>,
) -> Result<PathBuf, String> {
    if !super::backend::valid_build_id(build_id) {
        return Err(String::from(
            "A valid module build ID is required for downloading",
        ));
    }

    if configuration.urls.trim().is_empty() {
        return Err(String::from("No debuginfod servers are configured in GDB"));
    }

    if !is_current() {
        return Err(String::from("Symbol request cancelled"));
    }

    let launcher = gio::SubprocessLauncher::new(
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_PIPE,
    );

    launcher.setenv("DEBUGINFOD_URLS", configuration.urls.trim(), true);
    launcher.unsetenv("DEBUGINFOD_PROGRESS");
    launcher.setenv("DEBUGINFOD_TIMEOUT", "30", true);
    launcher.setenv("DEBUGINFOD_MAXTIME", "180", true);
    launcher.setenv("DEBUGINFOD_MAXSIZE", "1073741824", true);

    if let Some(cache) = configuration
        .cache_override
        .as_ref()
        .or(configuration.search.caches.first())
    {
        launcher.setenv("DEBUGINFOD_CACHE_PATH", cache, true);
    }

    let process = launcher.spawn(&[OsStr::new("debuginfod-find"), OsStr::new("debuginfo"), OsStr::new(build_id)])
        .map_err(|error| format!("Could not start debuginfod-find: {error}. Install the debuginfod client or provide a matching local debug file"))?;

    let cancelled = Rc::new(Cell::new(false));
    let timed_out = Rc::new(Cell::new(false));
    let started = Instant::now();
    let child = process.clone();
    let cancelled_for_timer = Rc::clone(&cancelled);
    let timed_out_for_timer = Rc::clone(&timed_out);

    let monitor = glib::timeout_add_local(Duration::from_millis(100), move || {
        if !is_current() || started.elapsed() >= DOWNLOAD_TIMEOUT {
            cancelled_for_timer.set(true);
            timed_out_for_timer.set(started.elapsed() >= DOWNLOAD_TIMEOUT);
            child.force_exit();
        }

        glib::ControlFlow::Continue
    });

    let mut download = Download {
        process: process.clone(),
        monitor: Some(monitor),
        finished: false,
    };

    let stderr = process
        .stderr_pipe()
        .ok_or("Debuginfod did not expose its error output")?;

    let stdout = process
        .stdout_pipe()
        .ok_or("Debuginfod did not expose its result output")?;

    let errors = glib::spawn_future_local(read_bounded(stderr, process.clone(), 32 * 1024, true));
    let output = read_bounded(stdout, process.clone(), 16 * 1024, false).await;
    let errors = errors.await.map_err(|error| error.to_string())?;
    process
        .wait_future()
        .await
        .map_err(|error| error.to_string())?;

    download.finished = true;

    if cancelled.get() {
        return Err(String::from(if timed_out.get() {
            "Debug information download timed out after 180 seconds"
        } else {
            "Symbol request cancelled"
        }));
    }

    let output = output?;
    let errors = errors?;

    if !process.is_successful() {
        let errors = String::from_utf8_lossy(&errors);
        let detail = errors.trim();

        return Err(format!(
            "Debuginfod could not resolve build ID {build_id}: {}",
            if detail.is_empty() {
                "the client exited without a debug file"
            } else {
                detail
            }
        ));
    }

    let path = String::from_utf8(output).map_err(|_| "Debuginfod returned a non-UTF-8 path")?;
    let path = path.strip_suffix('\n').unwrap_or(&path);

    if path.is_empty()
        || path.contains(['\0', '\n', '\r'])
        || !std::path::Path::new(path).is_absolute()
    {
        return Err(String::from(
            "Debuginfod returned an invalid debug-file path",
        ));
    }

    Ok(PathBuf::from(path))
}
