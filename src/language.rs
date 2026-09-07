//! Source-language policy shared by discovery, presentation, and value editing.
//!
//! A source language is not an expression dialect. In particular, Zig and Odin
//! currently rely on GDB's DWARF support rather than native expression parsers.

use std::path::Path;

pub(crate) mod python;
pub(crate) mod scripts;
pub(crate) mod toolchain;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Language {
    C,
    Cpp,
    Rust,
    Fortran,
    Zig,
    Odin,
    #[default]
    Unknown,
}

pub(crate) struct LanguageSupport {
    pub language: Language,
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    pub syntax: &'static str,
    pub gdb_dialects: &'static [&'static str],
    pub entrypoint_pattern: Option<&'static str>,
    pub inspection: &'static str,
    pub expressions: &'static str,
}

pub(crate) const PRIMARY_LANGUAGES: &[LanguageSupport] = &[
    LanguageSupport {
        language: Language::C,
        name: "C",
        extensions: &["c", "h"],
        syntax: "c",
        gdb_dialects: &["c"],
        entrypoint_pattern: None,
        inspection: "Native values, pointers, arrays, structs and unions",
        expressions: "GDB C expressions",
    },
    LanguageSupport {
        language: Language::Cpp,
        name: "C++",
        extensions: &[
            "cc", "cpp", "cxx", "hpp", "hh", "hxx", "tcc", "ipp", "ixx", "cppm",
        ],
        syntax: "cpp",
        gdb_dialects: &["c++"],
        entrypoint_pattern: None,
        inspection: "Native values and available libstdc++ pretty printers",
        expressions: "GDB C++ expressions",
    },
    LanguageSupport {
        language: Language::Rust,
        name: "Rust",
        extensions: &["rs"],
        syntax: "rust",
        gdb_dialects: &["rust"],
        entrypoint_pattern: None,
        inspection: "Native values and available Rust toolchain pretty printers",
        expressions: "GDB Rust expressions, subject to GDB's parser support",
    },
    LanguageSupport {
        language: Language::Fortran,
        name: "Fortran",
        extensions: &[
            "f", "for", "ftn", "f77", "f90", "f95", "f03", "f08", "f18", "f23",
        ],
        syntax: "fortran",
        gdb_dialects: &["fortran"],
        entrypoint_pattern: None,
        inspection: "GDB arrays with native bounds, derived types, logical and complex values",
        expressions: "GDB Fortran expressions, including a(i,j) and value%member",
    },
    LanguageSupport {
        language: Language::Zig,
        name: "Zig",
        extensions: &["zig"],
        syntax: "fgdb-zig",
        gdb_dialects: &[],
        entrypoint_pattern: Some("[.]main$"),
        inspection: "Byte-slice text previews, slices, optionals, error unions and tagged unions with supported debug layouts",
        expressions: "GDB's DWARF-selected dialect, not native Zig expressions. Use Debug with LLVM (-fllvm) when the native backend emits incomplete debug information.",
    },
    LanguageSupport {
        language: Language::Odin,
        name: "Odin",
        extensions: &["odin"],
        syntax: "fgdb-odin",
        gdb_dialects: &[],
        entrypoint_pattern: Some("::main$"),
        inspection: "Strings, slices, dynamic arrays and active union values, maps use raw fields",
        expressions: "GDB's DWARF-selected dialect, not native Odin expressions",
    },
];

// These remain usable without promoting their runtimes to primary targets.
pub(crate) const OTHER_SOURCE_EXTENSIONS: &[&str] = &[
    "s", "asm", "inc", "inl", "m", "mm", "go", "swift", "adb", "ads", "d", "di", "cu", "cuh", "cl",
    "pas", "pp", "java", "kt", "kts", "scala", "cs", "vala", "vapi", "py", "pyx", "pxd", "js",
    "jsx", "ts", "tsx", "sh", "bash", "zsh", "fish", "lua", "rb", "php",
];

pub(crate) fn source_extensions() -> impl Iterator<Item = &'static str> {
    PRIMARY_LANGUAGES
        .iter()
        .flat_map(|support| support.extensions.iter().copied())
        .chain(OTHER_SOURCE_EXTENSIONS.iter().copied())
}

impl Language {
    pub fn from_path(path: &Path) -> Self {
        let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
            return Self::Unknown;
        };

        // GCC treats uppercase .C and .H as C++ rather than C.
        if matches!(extension, "C" | "H") {
            return Self::Cpp;
        }

        PRIMARY_LANGUAGES
            .iter()
            .find(|support| {
                support
                    .extensions
                    .iter()
                    .any(|known| extension.eq_ignore_ascii_case(known))
            })
            .map_or(Self::Unknown, |support| support.language)
    }

    pub fn from_gdb(name: &str) -> Self {
        PRIMARY_LANGUAGES
            .iter()
            .find(|support| support.gdb_dialects.contains(&name))
            .map_or(Self::Unknown, |support| support.language)
    }

    pub fn support(self) -> Option<&'static LanguageSupport> {
        PRIMARY_LANGUAGES
            .iter()
            .find(|support| support.language == self)
    }

    /// GDB's default source can belong to the runtime when main is qualified.
    pub fn entrypoint_pattern(self) -> Option<&'static str> {
        self.support()
            .and_then(|support| support.entrypoint_pattern)
    }

    pub const fn boolean_literal(self, value: bool) -> &'static str {
        match (self, value) {
            (Self::Fortran, true) => ".true.",
            (Self::Fortran, false) => ".false.",
            (_, true) => "1",
            (_, false) => "0",
        }
    }
}

pub(crate) fn is_fortran_type(name: &str) -> bool {
    [
        "integer",
        "real",
        "logical",
        "complex",
        "character",
        "type ",
    ]
    .iter()
    .any(|prefix| {
        name.get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
            && (prefix.ends_with(' ')
                || name.get(prefix.len()..).is_some_and(|rest| {
                    let rest = rest.trim_start();

                    rest.is_empty()
                        || rest.starts_with(['(', ','])
                        || rest.strip_prefix('*').is_some_and(|kind| {
                            kind.starts_with('(')
                                || kind.as_bytes().first().is_some_and(u8::is_ascii_digit)
                        })
                }))
    })
}

pub(crate) fn is_fortran_array(name: &str) -> bool {
    is_fortran_type(name)
        && name.rsplit_once('(').is_some_and(|(_, bounds)| {
            bounds.strip_suffix(')').is_some_and(|bounds| {
                !bounds.is_empty()
                    && bounds
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || b"-+:, *".contains(&byte))
            })
        })
}

pub(crate) fn uses_fortran_kind_star(name: &str) -> bool {
    name.split_once('*').is_some_and(|(base, kind)| {
        let kind = kind.trim_start();

        is_fortran_type(base)
            && !base.contains(['(', ')', ','])
            && (kind.starts_with('(') || kind.as_bytes().first().is_some_and(u8::is_ascii_digit))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_identity_and_expression_dialects_are_separate() {
        for (path, language) in [
            ("x.F90", Language::Fortran),
            ("x.C", Language::Cpp),
            ("x.c", Language::C),
            ("x.zig", Language::Zig),
            ("x.odin", Language::Odin),
            ("x.go", Language::Unknown),
        ] {
            assert_eq!(Language::from_path(Path::new(path)), language);
        }

        assert_eq!(Language::from_gdb("minimal"), Language::Unknown);
        assert_eq!(Language::Fortran.boolean_literal(false), ".false.");
        assert!(is_fortran_array("integer(kind=4) (-2:2,4:5)"));
        assert!(is_fortran_array("integer(kind=4), allocatable (:)"));
        assert!(!is_fortran_array("integer(kind=4)"));
        assert!(!is_fortran_array("std::function<void(int)>"));
        assert!(is_fortran_type("character*24"));
        assert!(!is_fortran_type("real *"));
        assert!(!is_fortran_type("integer * const"));
        assert!(uses_fortran_kind_star("character*24"));
        assert!(!uses_fortran_kind_star("integer (*)(int)"));
        assert!(!uses_fortran_kind_star("real *"));
    }
}
