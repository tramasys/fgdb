//! Bounded asynchronous configuration I/O with optimistic replacement.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use gtk::{gio, glib, prelude::*};

use super::{MAX_CONFIG_BYTES, settings::Document};

pub(crate) struct Snapshot {
    pub document: Document,
    pub watch_paths: Vec<PathBuf>,
    etag: String,
}

pub(crate) struct Watch(Vec<gio::FileMonitor>);

impl Watch {
    pub(crate) fn new(paths: &[PathBuf], changed: impl Fn() + 'static) -> Result<Self, String> {
        let files: Rc<Vec<_>> = Rc::new(paths.iter().map(gio::File::for_path).collect());
        let parents: HashSet<_> = paths.iter().filter_map(|path| path.parent()).collect();
        let callback = Rc::new(changed);
        let mut watch = Self(Vec::with_capacity(parents.len()));

        // Directory watches survive atomic saves. GFile equality compares paths, not GObject identity.
        for parent in parents {
            let monitor = gio::File::for_path(parent)
                .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, None::<&gio::Cancellable>)
                .map_err(|error| format!("Automatic configuration reload is unavailable: {error}. Use Reload to read external edits"))?;

            let files = Rc::clone(&files);
            let callback = Rc::clone(&callback);

            monitor.connect_changed(move |_, changed, other, _| {
                if files
                    .iter()
                    .any(|file| file.equal(changed) || other.is_some_and(|other| file.equal(other)))
                {
                    callback();
                }
            });

            watch.0.push(monitor);
        }

        Ok(watch)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        for monitor in &self.0 {
            monitor.cancel();
        }
    }
}

pub(crate) async fn read(path: &Path) -> Result<Snapshot, String> {
    let file = gio::File::for_path(path);

    glib::future_with_timeout(Duration::from_secs(10), async {
        let stream = file
            .read_future(glib::Priority::DEFAULT)
            .await
            .map_err(|error| error.to_string())?;

        let result = async {
            let before = stream
                .query_info_future("standard::type,etag::value", glib::Priority::DEFAULT)
                .await
                .map_err(|error| error.to_string())?;

            if before.file_type() != gio::FileType::Regular {
                return Err(String::from("Configuration must be a regular file"));
            }

            let etag = etag(&before).ok_or_else(|| {
                String::from("The filesystem cannot safely track configuration changes")
            })?;
            let mut bytes = Vec::new();

            loop {
                let chunk = stream
                    .read_bytes_future(
                        (MAX_CONFIG_BYTES + 1 - bytes.len()).min(8192),
                        glib::Priority::DEFAULT,
                    )
                    .await
                    .map_err(|error| error.to_string())?;

                if chunk.is_empty() {
                    break;
                }

                bytes.extend_from_slice(chunk.as_ref());

                if bytes.len() > MAX_CONFIG_BYTES {
                    return Err(String::from("Configuration exceeds the 64 KiB limit"));
                }
            }

            let after = stream
                .query_info_future("etag::value", glib::Priority::DEFAULT)
                .await
                .map_err(|error| error.to_string())?;

            let current = file
                .query_info_future(
                    "etag::value",
                    gio::FileQueryInfoFlags::NONE,
                    glib::Priority::DEFAULT,
                )
                .await
                .map_err(|error| error.to_string())?;

            if self::etag(&after).as_deref() != Some(etag.as_str())
                || self::etag(&current).as_deref() != Some(etag.as_str())
            {
                return Err(String::from(
                    "Configuration changed while it was being read. Reload to try again",
                ));
            }

            let text = String::from_utf8(bytes)
                .map_err(|_| String::from("Configuration must be UTF-8 text"))?;

            Ok(Snapshot {
                document: Document::parse(text, path)?,
                watch_paths: watch_paths(path).await?,
                etag: etag.into(),
            })
        }
        .await;

        // Close on both successful reads and validation failures.
        let _ = stream.close_future(glib::Priority::DEFAULT).await;
        result
    })
    .await
    .map_err(|_| String::from("Reading configuration timed out"))?
}

async fn watch_paths(path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = vec![path.to_path_buf()];
    let mut file = gio::File::for_path(path);

    // Follow explicit file links so edits to a target outside the config directory are observed too.
    for _ in 0..32 {
        let info = file
            .query_info_future(
                "standard::symlink-target",
                gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                glib::Priority::DEFAULT,
            )
            .await
            .map_err(|error| error.to_string())?;

        let Some(target) = info
            .has_attribute("standard::symlink-target")
            .then(|| info.symlink_target())
            .flatten()
        else {
            return Ok(paths);
        };

        let target = if target.is_absolute() {
            target
        } else {
            paths
                .last()
                .and_then(|path| path.parent())
                .unwrap_or(Path::new("."))
                .join(target)
        };

        file = gio::File::for_path(&target);
        paths.push(target);
    }

    Err(String::from("Configuration has too many symbolic links"))
}

fn etag(info: &gio::FileInfo) -> Option<glib::GString> {
    info.has_attribute("etag::value")
        .then(|| info.etag())
        .flatten()
}

pub(crate) async fn write(path: &Path, snapshot: &Snapshot, text: String) -> Result<(), String> {
    // Recheck content as well as metadata. This also refuses to recreate a deleted file.
    let current = read(path).await?;

    if current.document.text != snapshot.document.text {
        return Err(String::from(
            "Configuration changed outside fgdb. Reload before saving to avoid overwriting it",
        ));
    }

    Document::parse(text.clone(), path)?;
    let file = gio::File::for_path(path);

    glib::future_with_timeout(
        Duration::from_secs(10),
        file.replace_contents_future(
            text.into_bytes(),
            Some(&current.etag),
            false,
            gio::FileCreateFlags::NONE,
        ),
    )
    .await
    .map_err(|_| {
        String::from("Saving configuration timed out. Reload to check the file before trying again")
    })?
    .map(|_| ())
    .map_err(|(_, error)| {
        if error.matches(gio::IOErrorEnum::WrongEtag) {
            String::from(
                "Configuration changed outside fgdb. Reload before saving to avoid overwriting it",
            )
        } else {
            error.to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    #[test]
    fn bounded_io_observes_replacements_preserves_links_and_rejects_stale_saves() {
        let root = std::env::temp_dir().join(format!("fgdb-settings-io-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let path = root.join("config.conf");
        let alias = root.join("linked.conf");
        let context = glib::MainContext::new();

        context
            .with_thread_default(|| {
                context.block_on(async {
                    fs::write(&path, "source_tab_width=4\n").unwrap();
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
                    symlink(&path, &alias).unwrap();
                    let original = read(&alias).await.unwrap();
                    assert_eq!(original.watch_paths, [alias.clone(), path.clone()]);
                    let notifications = Rc::new(Cell::new(0));
                    let counter = Rc::clone(&notifications);
                    let watch = Watch::new(&original.watch_paths, move || {
                        counter.set(counter.get() + 1)
                    })
                    .unwrap();

                    let replacement = root.join("replacement");
                    fs::write(&replacement, "source_tab_width=8\n").unwrap();
                    glib::timeout_future(Duration::from_millis(30)).await;
                    assert_eq!(
                        notifications.get(),
                        0,
                        "Unrelated files must not trigger reload"
                    );

                    fs::rename(&replacement, &path).unwrap();

                    for _ in 0..100 {
                        if notifications.get() > 0 {
                            break;
                        }

                        glib::timeout_future(Duration::from_millis(10)).await;
                    }

                    assert!(
                        notifications.get() > 0,
                        "Atomic replacement must trigger reload"
                    );

                    drop(watch);
                    fs::write(&path, "source_tab_width=4\n").unwrap();
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
                    write(&alias, &original, "source_tab_width=8\n".into())
                        .await
                        .unwrap();

                    assert!(fs::symlink_metadata(&alias).unwrap().is_symlink());
                    assert_eq!(fs::read_to_string(&path).unwrap(), "source_tab_width=8\n");
                    assert_eq!(
                        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                        0o600
                    );

                    assert!(
                        write(&alias, &original, "source_tab_width=2\n".into())
                            .await
                            .is_err()
                    );

                    assert_eq!(fs::read_to_string(&path).unwrap(), "source_tab_width=8\n");
                    let snapshot = read(&path).await.unwrap();
                    fs::remove_file(&path).unwrap();
                    assert!(
                        write(&path, &snapshot, "source_tab_width=2\n".into())
                            .await
                            .is_err()
                    );

                    assert!(!path.exists());
                    fs::write(&path, vec![b'x'; MAX_CONFIG_BYTES + 1]).unwrap();
                    assert!(read(&path).await.is_err());
                    fs::write(&path, [0xff]).unwrap();
                    assert!(read(&path).await.is_err());
                    assert!(read(&root).await.is_err());
                })
            })
            .unwrap();

        fs::remove_file(alias).unwrap();
        fs::remove_file(path).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
