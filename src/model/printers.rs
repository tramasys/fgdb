//! Runtime script ownership outlives the Debug Data dialog, but not its GDB
//! backend. Late validation/completion callbacks cannot finish a newer load.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PrinterLoadId(u64);

#[derive(Default)]
pub(crate) struct PrinterScripts {
    generation: u64,
    pending: Option<PrinterLoadId>,
    loaded: Vec<PathBuf>,
}

impl PrinterScripts {
    pub(crate) fn begin(&mut self) -> Result<PrinterLoadId, &'static str> {
        if self.pending.is_some() {
            return Err("Another pretty-printer script is still loading");
        }

        self.generation = self.generation.wrapping_add(1);
        let request = PrinterLoadId(self.generation);
        self.pending = Some(request);

        Ok(request)
    }

    pub(crate) fn is_current(&self, request: PrinterLoadId) -> bool {
        self.pending == Some(request)
    }

    pub(crate) fn is_loading(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn loaded(&self) -> &[PathBuf] {
        &self.loaded
    }

    pub(crate) fn contains(&self, path: &Path) -> bool {
        self.loaded.iter().any(|loaded| loaded == path)
    }

    pub(crate) fn finish(&mut self, request: PrinterLoadId, path: Option<PathBuf>) -> bool {
        if !self.is_current(request) {
            return false;
        }

        self.pending = None;

        if let Some(path) = path
            && !self.contains(&path)
        {
            self.loaded.push(path);
        }

        true
    }

    pub(crate) fn reset(&mut self) {
        self.pending = None;
        self.loaded.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_loads_cannot_clear_or_populate_a_new_backend() {
        let mut scripts = PrinterScripts::default();
        let old = scripts.begin().unwrap();
        assert!(scripts.begin().is_err());
        scripts.reset();
        let current = scripts.begin().unwrap();
        assert!(!scripts.finish(old, Some(PathBuf::from("old.py"))));
        assert!(scripts.is_current(current));
        assert!(scripts.loaded().is_empty());
        assert!(scripts.finish(current, Some(PathBuf::from("new.py"))));
        assert!(!scripts.is_loading());
        assert!(scripts.contains(Path::new("new.py")));
        assert!(!scripts.finish(current, Some(PathBuf::from("duplicate.py"))));
        scripts.reset();
        assert!(scripts.loaded().is_empty());
    }
}
