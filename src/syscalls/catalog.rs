use std::sync::OnceLock;

use super::Key;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Abi {
    X86_64,
    I386,
    X32,
    Aarch64,
    Arm,
}

impl Abi {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::I386 => "i386",
            Self::X32 => "x32",
            Self::Aarch64 => "aarch64",
            Self::Arm => "arm",
        }
    }

    pub(crate) fn host_abis() -> &'static [Self] {
        match std::env::consts::ARCH {
            "x86_64" => &[Self::X86_64, Self::I386, Self::X32],
            "aarch64" => &[Self::Aarch64, Self::Arm],
            _ => &[],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Metadata {
    pub(crate) key: Key,
    pub(crate) name: &'static str,
    pub(crate) category: &'static str,
    pub(crate) arguments: &'static str,
}

pub(crate) fn catalog() -> &'static [Metadata] {
    static CATALOG: OnceLock<Vec<Metadata>> = OnceLock::new();

    CATALOG.get_or_init(|| {
        let mut rows = Vec::new();

        for line in include_str!("catalog.tsv")
            .lines()
            .filter(|line| !line.starts_with('#'))
        {
            let mut fields = line.split('\t');

            let abi = match fields.next().expect("bundled syscall ABI") {
                "x86_64" => Abi::X86_64,
                "i386" => Abi::I386,
                "x32" => Abi::X32,
                "aarch64" => Abi::Aarch64,
                "arm" => Abi::Arm,
                _ => unreachable!("bundled syscall ABI"),
            };

            let number = fields
                .next()
                .unwrap()
                .parse()
                .expect("bundled syscall number");

            rows.push(Metadata {
                key: Key { abi, number },
                name: fields.next().expect("bundled syscall name"),
                category: fields.next().expect("bundled syscall category"),
                arguments: fields.next().expect("bundled syscall arguments"),
            });
        }

        rows.sort_unstable_by_key(|row| row.key);
        rows
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_preserves_abi_identity_and_unknown_numbers() {
        let rows = catalog();
        assert!(rows.windows(2).all(|pair| pair[0].key < pair[1].key));

        for (abi, number, name) in [
            (Abi::X86_64, 0, "read"),
            (Abi::I386, 3, "read"),
            (Abi::X32, 0x40000000, "read"),
            (Abi::Aarch64, 63, "read"),
            (Abi::Arm, 3, "read"),
            (Abi::X86_64, 449, "futex_waitv"),
        ] {
            let key = Key { abi, number };
            let row = &rows[rows.binary_search_by_key(&key, |row| row.key).unwrap()];
            assert_eq!(row.name, name);
        }

        assert!(rows.iter().all(|row| row.key.number != u64::MAX));
        assert_eq!(
            Key {
                abi: Abi::X86_64,
                number: u64::MAX
            }
            .number_text(),
            "-1"
        );
    }
}
