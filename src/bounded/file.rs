//! Regular-file validation and cache identity shared by source and ELF readers.

use std::{
    fs::{File, Metadata, OpenOptions},
    io,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    pub(crate) path: PathBuf,
    pub(crate) size: u64,
    pub(crate) modified: SystemTime,
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) changed: (i64, i64),
}

impl FileIdentity {
    pub(crate) fn read(path: &Path) -> io::Result<Self> {
        Self::from_metadata(path, &std::fs::metadata(path)?)
    }

    pub(crate) fn from_metadata(path: &Path, metadata: &Metadata) -> io::Result<Self> {
        ensure_regular(metadata)?;

        Ok(Self {
            path: path.to_owned(),
            size: metadata.len(),
            modified: metadata.modified()?,
            device: metadata.dev(),
            inode: metadata.ino(),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

pub(crate) fn open_regular_file(path: &Path) -> io::Result<(File, Metadata)> {
    // Validate the opened handle. A path can become a FIFO after a metadata check.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits())
        .open(path)?;

    let metadata = file.metadata()?;
    ensure_regular(&metadata)?;
    Ok((file, metadata))
}

fn ensure_regular(metadata: &Metadata) -> io::Result<()> {
    if metadata.is_file() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The path is not a regular file",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_special_files_without_waiting_for_a_writer() {
        let root = gtk::glib::mkdtemp(std::env::temp_dir().join("fgdb-file-XXXXXX")).unwrap();
        let pipe = root.join("pipe");
        nix::unistd::mkfifo(&pipe, nix::sys::stat::Mode::S_IRUSR).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&pipe, &link).unwrap();

        for path in [&root, &pipe, &link, Path::new("/dev/null")] {
            assert_eq!(
                open_regular_file(path).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );

            assert!(FileIdentity::read(path).is_err());
            assert!(super::super::read_regular_bytes(path, 128).is_err());
        }

        std::fs::remove_file(link).unwrap();
        std::fs::remove_file(pipe).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
