//! Asynchronous workspace storage with bounded reads and conflict-safe publication.

use super::{Investigation, MAX_BYTES, message};
use gtk::{gio, glib, prelude::*};
use std::path::Path;

pub(crate) async fn save_revision(
    path: &Path,
    allow_overwrite: bool,
) -> Result<Option<String>, String> {
    glib::future_with_timeout(std::time::Duration::from_secs(15), async {
        let file = gio::File::for_path(path);

        match file.query_info_future(
            "standard::type,etag::value",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
        ).await {
            Ok(_) if !allow_overwrite => Err(String::from(
                "An incomplete restore can only be saved to a new file. Choose a different filename",
            )),
            Ok(info) if info.file_type() != gio::FileType::Regular => {
                Err(String::from("Workspace destination must be a regular file"))
            }
            Ok(info) => revision(&info).map(Some),
            Err(error) if error.matches(gio::IOErrorEnum::NotFound) => Ok(None),
            Err(error) => Err(message(error)),
        }
    }).await.map_err(|_| String::from("Checking the workspace destination timed out"))?
}

fn revision(info: &gio::FileInfo) -> Result<String, String> {
    info.attribute_string("etag::value")
        .filter(|tag| !tag.is_empty())
        .map(|tag| tag.to_string())
        .ok_or_else(|| String::from("The filesystem cannot track workspace changes safely"))
}

pub(crate) async fn read(path: &Path) -> Result<(Investigation, String), String> {
    glib::future_with_timeout(std::time::Duration::from_secs(15), read_inner(path))
        .await
        .map_err(|_| String::from("Reading the workspace timed out"))?
}

async fn read_inner(path: &Path) -> Result<(Investigation, String), String> {
    let file = gio::File::for_path(path);
    let info = file
        .query_info_future(
            "standard::type,standard::size",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
        )
        .await
        .map_err(message)?;

    if info.file_type() != gio::FileType::Regular || info.size() > MAX_BYTES as i64 {
        return Err(String::from(
            "Workspace must be a regular file of at most 1 MiB",
        ));
    }

    let stream = file
        .read_future(glib::Priority::DEFAULT)
        .await
        .map_err(message)?;

    let result = async {
        let before = stream
            .query_info_future("standard::type,etag::value", glib::Priority::DEFAULT)
            .await
            .map_err(message)?;

        if before.file_type() != gio::FileType::Regular {
            return Err(String::from("Workspace must be a regular file"));
        }

        let etag = revision(&before)?;

        let mut bytes = Vec::new();

        loop {
            let chunk = stream
                .read_bytes_future(
                    (MAX_BYTES + 1 - bytes.len()).min(8192),
                    glib::Priority::DEFAULT,
                )
                .await
                .map_err(message)?;

            if chunk.is_empty() {
                break;
            }

            bytes.extend_from_slice(&chunk);

            if bytes.len() > MAX_BYTES {
                return Err(String::from("Workspace exceeds the 1 MiB limit"));
            }
        }

        let after = stream
            .query_info_future("etag::value", glib::Priority::DEFAULT)
            .await
            .map_err(message)?;

        let current = file
            .query_info_future(
                "etag::value",
                gio::FileQueryInfoFlags::NONE,
                glib::Priority::DEFAULT,
            )
            .await
            .map_err(message)?;

        if revision(&after)? != etag || revision(&current)? != etag {
            return Err(String::from(
                "Workspace changed while being read. Open it again",
            ));
        }

        let workspace = gio::spawn_blocking(move || {
            let text = std::str::from_utf8(&bytes).map_err(message)?;
            Investigation::decode(text)
        })
        .await
        .map_err(|_| String::from("Workspace decoding failed"))??;

        Ok((workspace, etag))
    }
    .await;

    let _ = stream.close_future(glib::Priority::DEFAULT).await;
    result
}

pub(crate) async fn write(
    path: &Path,
    workspace: Investigation,
    etag: Option<&str>,
) -> Result<String, String> {
    if etag.is_some_and(str::is_empty) {
        return Err(String::from(
            "An existing workspace requires a valid filesystem revision",
        ));
    }

    glib::future_with_timeout(
        std::time::Duration::from_secs(15),
        write_inner(path, workspace, etag),
    )
    .await
    .map_err(|_| String::from("Saving the workspace timed out. Check the file before retrying"))?
}

async fn write_inner(
    path: &Path,
    workspace: Investigation,
    etag: Option<&str>,
) -> Result<String, String> {
    let text = gio::spawn_blocking(move || workspace.encode())
        .await
        .map_err(|_| String::from("Workspace encoding failed"))??;

    let file = gio::File::for_path(path);

    if etag.is_none() {
        let parent = file.parent().ok_or("Workspace needs a parent directory")?;
        let temporary = parent.child(format!(".fgdb-workspace-{}", glib::uuid_string_random()));
        let stream = temporary
            .create_future(gio::FileCreateFlags::PRIVATE, glib::Priority::DEFAULT)
            .await
            .map_err(message)?;

        // Only retire a temporary file after exclusive creation succeeded.
        let mut cleanup = TemporaryFile(Some(temporary.clone()));
        let (_, _, error) = stream
            .write_all_future(text.into_bytes(), glib::Priority::DEFAULT)
            .await
            .map_err(|(_, error)| error.to_string())?;

        stream
            .close_future(glib::Priority::DEFAULT)
            .await
            .map_err(message)?;

        if let Some(error) = error {
            return Err(error.to_string());
        }

        let info = temporary
            .query_info_future(
                "etag::value",
                gio::FileQueryInfoFlags::NONE,
                glib::Priority::DEFAULT,
            )
            .await
            .map_err(message)?;

        // Capture our revision before publication, never a concurrent editor's revision.
        let etag = revision(&info)?;
        let source = temporary.path().ok_or("Workspace must be a local file")?;
        let destination = path.to_owned();

        gio::spawn_blocking(move || {
            std::fs::File::open(&source)
                .and_then(|file| file.sync_all())
                .map_err(message)?;

            rustix::fs::renameat_with(
                rustix::fs::CWD,
                &source,
                rustix::fs::CWD,
                &destination,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(message)
        })
        .await
        .map_err(|_| String::from("Workspace publication failed"))??;

        cleanup.0.take();
        return Ok(etag);
    }

    let (_, etag) = file
        .replace_contents_future(
            text.into_bytes(),
            etag,
            false,
            gio::FileCreateFlags::PRIVATE,
        )
        .await
        .map_err(|(_, error)| {
            if error.matches(gio::IOErrorEnum::WrongEtag) {
                String::from(
                    "Workspace changed outside fgdb. \
                     Use Save workspace as to preserve both versions",
                )
            } else {
                error.to_string()
            }
        })?;

    etag.map(|etag| etag.to_string()).ok_or_else(|| {
        String::from(
            "Workspace saved, but the filesystem did not return its revision. \
             Use Save workspace as for further saves",
        )
    })
}

struct TemporaryFile(Option<gio::File>);

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if let Some(file) = self.0.take() {
            file.delete_async(glib::Priority::DEFAULT, None::<&gio::Cancellable>, |_| {});
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::workspace;
    use super::*;

    #[test]
    fn workspace_writes_are_private_and_detect_conflicts() {
        let directory = std::env::temp_dir().join(format!(
            "fgdb-workspace-test-{}",
            glib::uuid_string_random()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("session.fgdb-workspace");
        let context = glib::MainContext::new();

        context
            .with_thread_default(|| {
                context.block_on(async {
                    let workspace = workspace();
                    let etag = write(&path, workspace.clone(), None).await.unwrap();
                    let (loaded, read_etag) = read(&path).await.unwrap();
                    assert_eq!(read_etag, etag);
                    assert_eq!(loaded.watches, workspace.watches);
                    assert!(write(&path, workspace.clone(), None).await.is_err());
                    assert!(save_revision(&path, false).await.is_err());
                    let alias = directory.join("alias.fgdb-workspace");
                    std::os::unix::fs::symlink(&path, &alias).unwrap();
                    assert!(save_revision(&alias, false).await.is_err());
                    assert!(save_revision(&directory, true).await.is_err());

                    let other = directory.join("concurrent.fgdb-workspace");
                    let left = glib::spawn_future_local({
                        let path = other.clone();
                        let workspace = workspace.clone();
                        async move { write(&path, workspace, None).await }
                    });
                    let right = write(&other, workspace.clone(), None).await;
                    assert_ne!(left.await.unwrap().is_ok(), right.is_ok());

                    use std::os::unix::fs::PermissionsExt;
                    assert_eq!(
                        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                        0o600
                    );
                    std::fs::write(&path, "external edit").unwrap();
                    assert!(write(&path, workspace, Some(&etag)).await.is_err());
                    assert_eq!(std::fs::read_to_string(&path).unwrap(), "external edit");
                })
            })
            .unwrap();

        std::fs::remove_file(&path).unwrap();
        // Temporary-file cleanup is asynchronous and may still be queued.
        while context.pending() {
            context.iteration(false);
        }

        for entry in std::fs::read_dir(&directory).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }
        std::fs::remove_dir(directory).unwrap();
    }
}
