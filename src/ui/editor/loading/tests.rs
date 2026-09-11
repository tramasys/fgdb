use super::*;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);

        let path = std::env::temp_dir().join(format!(
            "fgdb-source-loading-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));

        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn source_loading_resolves_unique_local_files_without_extension_restrictions() {
    let directory = TestDirectory::new();
    let path = directory.0.join("source without extension");
    let contents = "int main() {\n    return 0;\n}\n";
    std::fs::write(&path, contents).unwrap();
    let index = source::SourceIndex::new(std::slice::from_ref(&path), &[]);

    let (resolved, snapshot) = load_source(
        Path::new("/build/source without extension"),
        &[],
        Some(&index),
        1024,
    )
    .unwrap();

    assert_eq!(resolved, path.canonicalize().unwrap());
    assert_eq!(&*snapshot.contents, contents);
    let link = directory.0.join("source.c");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert_eq!(load_source(&link, &[], None, 1024).unwrap().0, resolved);
}

#[test]
fn source_loading_rejects_missing_special_and_binary_files() {
    let directory = TestDirectory::new();
    assert!(load_source(&directory.0.join("missing.c"), &[], None, 1024).is_err());
    assert!(load_source(&directory.0, &[], None, 1024).is_err());
    let fifo = directory.0.join("pipe.c");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR).unwrap();
    assert!(load_source(&fifo, &[], None, 1024).is_err());
    let path = directory.0.join("binary.c");
    std::fs::write(&path, b"\x7fELF\0not source").unwrap();

    assert!(
        load_source(&path, &[], None, 1024)
            .err()
            .unwrap()
            .contains("binary data")
    );
}

#[test]
fn source_loading_rejects_ambiguous_paths() {
    let directory = TestDirectory::new();
    let roots = [directory.0.join("first"), directory.0.join("second")];

    let files = roots
        .iter()
        .map(|root| {
            std::fs::create_dir(root).unwrap();
            let path = root.join("main.c");
            std::fs::write(&path, "int main() {}\n").unwrap();
            path
        })
        .collect::<Vec<_>>();

    let index = source::SourceIndex::new(&files, &roots);

    assert!(
        load_source(Path::new("main.c"), &roots, Some(&index), 1024)
            .err()
            .unwrap()
            .contains("More than one")
    );

    assert!(load_source(Path::new("main.c"), &roots, None, 1024).is_err());
    assert!(load_source(&files[0], &roots, Some(&index), 1024).is_ok());
}

#[test]
fn source_loading_enforces_byte_and_line_limits() {
    let directory = TestDirectory::new();
    let path = directory.0.join("large.c");
    std::fs::write(&path, "1234").unwrap();
    assert!(load_source(&path, &[], None, 4).is_ok());
    assert!(load_source(&path, &[], None, 3).is_err());
    std::fs::write(&path, "\n".repeat(250_001)).unwrap();

    assert!(
        load_source(&path, &[], None, 1024 * 1024)
            .err()
            .unwrap()
            .contains("250000-line")
    );
}
