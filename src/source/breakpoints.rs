use super::{SourceId, SourceIndex};
use crate::debugger::Breakpoint;
use std::{collections::HashMap, path::Path};

/// Immutable positions into the breakpoint snapshot published with this index.
/// Rendering and activation never resolve paths or scan the breakpoint list.
#[derive(Default)]
pub(crate) struct SourceBreakpointIndex {
    files: HashMap<SourceId, HashMap<u32, usize>>,
}

impl SourceBreakpointIndex {
    /// Run on a worker. Resolve each distinct reported path once, not once per
    /// breakpoint or visible source line. Preserve first-match ordering when
    /// several locations refer to the same source line.
    pub(crate) fn build_while(
        breakpoints: &[Breakpoint],
        sources: Option<&SourceIndex>,
        mut is_current: impl FnMut() -> bool,
    ) -> Option<Self> {
        let mut result = Self::default();
        let mut identities = HashMap::<&str, Vec<SourceId>>::new();

        for (position, breakpoint) in breakpoints.iter().enumerate() {
            if !is_current() {
                return None;
            }
            let (Some(line), Some(reported)) = (breakpoint.line, breakpoint.source_path()) else {
                continue;
            };

            let ids = identities.entry(reported).or_insert_with(|| {
                let path = Path::new(reported);
                if path.is_absolute() {
                    let literal = SourceId::from_indexed_path(path);
                    let canonical = SourceId::from_path(path);
                    if literal == canonical {
                        vec![literal]
                    } else {
                        vec![literal, canonical]
                    }
                } else {
                    sources
                        .and_then(|sources| sources.relative_reported_file(path).ok())
                        .map(SourceId::from_indexed_path)
                        .into_iter()
                        .collect()
                }
            });

            for id in ids {
                if let Some(lines) = result.files.get_mut(id) {
                    lines.entry(line).or_insert(position);
                } else {
                    result
                        .files
                        .insert(id.clone(), HashMap::from([(line, position)]));
                }
            }
        }

        is_current().then_some(result)
    }

    pub(crate) fn at_line(&self, source: &SourceId, line: u32) -> Option<usize> {
        self.files.get(source)?.get(&line).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn breakpoint(path: &str, line: u32, number: &str) -> Breakpoint {
        let record = crate::debugger::parse_record(&format!(
            "^done,BreakpointTable={{body=[bkpt={{number={},type=\"breakpoint\",enabled=\"y\",fullname={},line=\"{line}\"}}]}}",
            crate::debugger::quote(number),
            crate::debugger::quote(path),
        )).unwrap();
        crate::debugger::breakpoints(&record).remove(0)
    }

    #[test]
    fn indexed_locations_preserve_path_matching_and_first_breakpoint_order() {
        let directory = super::super::tests::temporary_test_directory("gutter-index");
        let first = directory.join("one/main.rs");
        let second = directory.join("two/main.rs");
        std::fs::create_dir_all(first.parent().unwrap()).unwrap();
        std::fs::create_dir_all(second.parent().unwrap()).unwrap();
        std::fs::write(&first, "fn main() {}\n").unwrap();
        std::fs::write(&second, "fn main() {}\n").unwrap();
        let alias = directory.join("alias.rs");
        std::os::unix::fs::symlink(&first, &alias).unwrap();
        let sources = SourceIndex::new(&[first.clone(), second.clone()], &[]);
        let mut breakpoints = vec![
            breakpoint(alias.to_str().unwrap(), 1, "1.2"),
            breakpoint(first.to_str().unwrap(), 1, "2"),
            breakpoint("one/main.rs", 2, "3"),
            breakpoint("main.rs", 3, "4"),
            breakpoint(second.to_str().unwrap(), 4, "5"),
        ];
        breakpoints[0].enabled = false;
        for source_index in [None, Some(&sources)] {
            let index =
                SourceBreakpointIndex::build_while(&breakpoints, source_index, || true).unwrap();
            for path in [&first, &second, &alias] {
                let id = SourceId::from_indexed_path(path);
                for line in 1..=5 {
                    let expected = breakpoints.iter().position(|bp| {
                        bp.line == Some(line)
                            && super::super::paths_match_id(
                                source_index,
                                &id,
                                bp.source_path().unwrap(),
                            )
                    });
                    assert_eq!(index.at_line(&id, line), expected);
                }
            }
            assert_eq!(index.at_line(&SourceId::from_path(&first), 1), Some(0));
        }
        let index =
            SourceBreakpointIndex::build_while(&breakpoints, Some(&sources), || true).unwrap();
        assert!(
            SourceBreakpointIndex::build_while(&breakpoints, Some(&sources), || false).is_none()
        );
        std::fs::remove_dir_all(&directory).unwrap();
        // Once published, painting does not depend on the filesystem remaining available.
        assert_eq!(
            index.at_line(&SourceId::from_indexed_path(&first), 2),
            Some(2)
        );
    }
}
