//! Cancellable debuginfod transfers do not occupy GDB's command queue. The
//! standard client owns its cache and server protocol, including cached misses.

use std::{ffi::OsStr, path::PathBuf, rc::Rc, time::Duration};

use super::backend::Configuration;
use gtk::gio;

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);

mod capture;

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

    let (output, errors) = capture::output(&process, is_current.as_ref(), DOWNLOAD_TIMEOUT).await?;

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
