//! Explain owner inference without treating every futex as a mutex.

use super::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LockObservation {
    pub word: Option<u32>,
    pub mapping: Option<ProcessMapping>,
    pub ownership: LockOwnership,
    pub cycle: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum LockOwnership {
    #[default]
    NoAddress,
    Unreadable(String),
    UnknownEndian,
    Unencoded,
    Changed,
    OwnerDied,
    NoOwner,
    OutsideSnapshot(u32),
    Requeue,
    Pi(u32),
    RobustCandidate(u32),
}

impl LockOwnership {
    pub(crate) fn owner(&self) -> Option<u32> {
        match self {
            Self::Pi(tid) | Self::RobustCandidate(tid) => Some(*tid),
            _ => None,
        }
    }

    pub(crate) fn explanation(&self) -> String {
        match self {
            Self::NoAddress => "No individual futex address is available. A wait vector or unreadable syscall arguments cannot identify a lock word".into(),
            Self::Unreadable(error) => format!("The current 32-bit futex word could not be read. {error}"),
            Self::UnknownEndian => "The target byte order is unknown. The futex word was not decoded".into(),
            Self::Unencoded => "This ordinary futex word does not establish an owner. Futex values may be counters, condition variables or implementation-specific state".into(),
            Self::Changed => "The current word differs from the expected wait value. Robust owner inference was withheld".into(),
            Self::OwnerDied => "The owner-died bit is set in an owner-encoded candidate. Ownership and recovery cannot be established from this word alone".into(),
            Self::NoOwner => "The owner-encoded word contains no owner TID".into(),
            Self::OutsideSnapshot(tid) => format!("The word suggests owner TID {tid}, but that thread is outside the scanned process snapshot. No dependency was added"),
            Self::Requeue => "This wait address is a requeue source, not the destination PI mutex. Its word is not interpreted as an owner".into(),
            Self::Pi(tid) => format!("The PI lock operation defines an owner-encoded word. Its low 30 bits identify scanned TID {tid}"),
            Self::RobustCandidate(tid) => format!("The waiters bit, unchanged expected word and scanned TID {tid} match a robust owner encoding. This is an inference, not a verified pthread mutex layout"),
        }
    }
}

pub(super) fn read_word(
    memory: Result<&File, &io::Error>,
    address: u64,
    endian: Option<TargetEndian>,
) -> Result<u32, LockOwnership> {
    let endian = endian.ok_or(LockOwnership::UnknownEndian)?;
    let memory = memory.map_err(|error| LockOwnership::Unreadable(error.to_string()))?;
    let mut bytes = [0; 4];

    match memory.read_at(&mut bytes, address) {
        Ok(4) => Ok(match endian {
            TargetEndian::Little => u32::from_le_bytes(bytes),
            TargetEndian::Big => u32::from_be_bytes(bytes),
        }),
        Ok(_) => Err(LockOwnership::Unreadable(
            "Incomplete four-byte read".into(),
        )),
        Err(error) => Err(LockOwnership::Unreadable(error.to_string())),
    }
}

pub(super) fn ownership(
    wait: &LockWait,
    value: u32,
    threads: &HashMap<u32, String>,
) -> LockOwnership {
    if wait.operation == "FUTEX_WAIT_REQUEUE_PI" {
        return LockOwnership::Requeue;
    }

    let pi = matches!(wait.operation.as_str(), "FUTEX_LOCK_PI" | "FUTEX_LOCK_PI2");

    let robust_candidate = matches!(wait.operation.as_str(), "FUTEX_WAIT" | "FUTEX_WAIT_BITSET")
        && value & 0x8000_0000 != 0;

    if !pi && !robust_candidate {
        return LockOwnership::Unencoded;
    }

    if !pi && wait.expected != Some(u64::from(value)) {
        return LockOwnership::Changed;
    }

    if value & 0x4000_0000 != 0 {
        return LockOwnership::OwnerDied;
    }

    let tid = value & 0x3fff_ffff;

    if tid == 0 {
        LockOwnership::NoOwner
    } else if !threads.contains_key(&tid) {
        LockOwnership::OutsideSnapshot(tid)
    } else if pi {
        LockOwnership::Pi(tid)
    } else {
        LockOwnership::RobustCandidate(tid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explains_owner_inference_and_rejects_ambiguous_words() {
        let threads = HashMap::from([(1, "init".into()), (20, "owner".into())]);

        for (operation, value, expected, evidence) in [
            ("FUTEX_WAIT", 2, Some(2), LockOwnership::Unencoded),
            ("FUTEX_WAIT", 20, Some(20), LockOwnership::Unencoded),
            (
                "FUTEX_WAIT",
                0x8000_0014,
                Some(0x8000_0014),
                LockOwnership::RobustCandidate(20),
            ),
            ("FUTEX_WAIT", 0x8000_0014, Some(2), LockOwnership::Changed),
            (
                "FUTEX_WAIT",
                0xc000_0014,
                Some(0xc000_0014),
                LockOwnership::OwnerDied,
            ),
            (
                "FUTEX_WAIT_REQUEUE_PI",
                0x8000_0014,
                Some(0x8000_0014),
                LockOwnership::Requeue,
            ),
            ("FUTEX_LOCK_PI", 20, None, LockOwnership::Pi(20)),
            ("FUTEX_LOCK_PI2", 1, None, LockOwnership::Pi(1)),
            ("FUTEX_LOCK_PI", 0, None, LockOwnership::NoOwner),
            (
                "FUTEX_LOCK_PI",
                0x8000_0015,
                None,
                LockOwnership::OutsideSnapshot(21),
            ),
        ] {
            let wait = LockWait {
                operation: operation.into(),
                expected,
                ..Default::default()
            };

            let actual = ownership(&wait, value, &threads);
            assert_eq!(actual, evidence);
            assert!(!actual.explanation().is_empty());
        }
    }

    #[test]
    fn reads_target_endian_words_and_reports_short_or_unavailable_reads() {
        let root = gtk::glib::mkdtemp(std::env::temp_dir().join("fgdb-lock-word-XXXXXX")).unwrap();
        let path = root.join("mem");
        std::fs::write(&path, [0x80, 0, 0, 20]).unwrap();
        let file = File::open(&path).unwrap();

        assert_eq!(
            read_word(Ok(&file), 0, Some(TargetEndian::Big)),
            Ok(0x8000_0014)
        );

        assert_eq!(
            read_word(Ok(&file), 0, Some(TargetEndian::Little)),
            Ok(0x1400_0080)
        );

        assert!(matches!(
            read_word(Ok(&file), 2, Some(TargetEndian::Big)),
            Err(LockOwnership::Unreadable(_))
        ));

        assert_eq!(
            read_word(Ok(&file), 0, None),
            Err(LockOwnership::UnknownEndian)
        );

        let error = io::Error::from(io::ErrorKind::PermissionDenied);

        assert!(matches!(
            read_word(Err(&error), 0, Some(TargetEndian::Big)),
            Err(LockOwnership::Unreadable(_))
        ));

        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
