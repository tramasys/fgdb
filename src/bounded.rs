use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

pub(crate) fn read_bytes(path: &Path, maximum: usize) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut bytes = Vec::with_capacity(initial_capacity(&file, maximum));

    file.take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;

    if bytes.len() > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} exceeds fgdb's {maximum}-byte read limit",
                path.display()
            ),
        ));
    }

    Ok(bytes)
}

pub(crate) fn read_string(path: &Path, maximum: usize) -> io::Result<String> {
    String::from_utf8(read_bytes(path, maximum)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub(crate) fn read_prefix(path: &Path, maximum: usize) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut bytes = Vec::with_capacity(initial_capacity(&file, maximum));

    file.take(u64::try_from(maximum).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)?;

    Ok(bytes)
}

fn initial_capacity(file: &File, maximum: usize) -> usize {
    const SMALL_READ_CAPACITY: usize = 8 * 1024;
    const METADATA_THRESHOLD: usize = 1024 * 1024;

    if maximum > METADATA_THRESHOLD
        && let Ok(length) = file.metadata().map(|metadata| metadata.len())
        && length > 0
    {
        return usize::try_from(length).unwrap_or(maximum).min(maximum);
    }

    maximum.min(SMALL_READ_CAPACITY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_files_larger_than_the_budget() {
        let path = std::env::temp_dir().join(format!(
            "fgdb-bounded-read-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));

        std::fs::write(&path, b"12345").unwrap();
        assert_eq!(read_bytes(&path, 5).unwrap(), b"12345");

        assert_eq!(
            read_bytes(&path, 4).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        std::fs::remove_file(path).unwrap();
    }
}
