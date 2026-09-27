//! Source documents, navigation, and breakpoint gutter interactions.

use super::*;

mod actions;
mod breakpoints;
pub(super) mod freshness;
mod gutter;
mod loading;
mod navigation;
mod view;

pub(super) use breakpoints::SourceBreakpointRefresh;
pub(super) use gutter::BreakpointGutterRenderer;
use gutter::LineStyle;
pub(super) use view::*;

pub(in crate::ui) struct SourceWorkspace {
    pub(in crate::ui) notebook: gtk::Notebook,
    pub(in crate::ui) documents: Rc<RefCell<Vec<SourceDocument>>>,
    pub(in crate::ui) navigation: SourceNavigationControls,
    pub(in crate::ui) tree: SourceTreeControls,
    back_history: RefCell<Vec<SourceNavigationLocation>>,
    forward_history: RefCell<Vec<SourceNavigationLocation>>,
    pub(in crate::ui) closed_tabs: Rc<RefCell<Vec<ClosedSourceTab>>>,
    find_state: RefCell<Option<SourceFindState>>,
    palette: RefCell<Option<SourcePalette>>,
    palette_generation: Arc<AtomicU64>,
    pub(in crate::ui) loaded_generation: Arc<AtomicU64>,
    loaded_cache: RefCell<Option<Arc<Vec<PathBuf>>>>,
    loaded_search: RefCell<Option<Arc<source::SourceSearchIndex>>>,
    pub(in crate::ui) loaded_files: RefCell<Vec<SourceFile>>,
    tree_base_roots: Vec<PathBuf>,
    tree_roots: RefCell<Vec<PathBuf>>,
    tree_cache: RefCell<Option<Arc<Vec<PathBuf>>>>,
    tree_search: RefCell<Option<Arc<source::SourceSearchIndex>>>,
    index: RefCell<Option<Arc<source::SourceIndex>>>,
    pub(in crate::ui) breakpoint_index: Rc<RefCell<source::SourceBreakpointIndex>>,
    pub(in crate::ui) breakpoint_refresh: RefCell<editor::SourceBreakpointRefresh>,
    pub(in crate::ui) tree_indexing: Cell<bool>,
    pub(in crate::ui) tree_generation: Arc<AtomicU64>,
    pub(in crate::ui) tree_render_generation: Arc<AtomicU64>,
    pub(in crate::ui) tree_initialized: Cell<bool>,
    pub(in crate::ui) execution_path: RefCell<Option<PathBuf>>,
    pub(in crate::ui) execution_line: Cell<Option<u32>>,
    theme: Theme,
    style_scheme: Option<sourceview5::StyleScheme>,
    resolved_paths: RefCell<crate::performance::BoundedLruCache<String, PathBuf>>,
    pub(in crate::ui) open_generation: Arc<AtomicU64>,
    annotation_epoch: Arc<AtomicU64>,
    annotation_pending: RefCell<HashMap<PathBuf, Arc<AtomicBool>>>,
    annotation_cache:
        RefCell<crate::performance::BoundedLruCache<PathBuf, (u64, Option<source::CachedSource>)>>,
    pub(in crate::ui) roots: RefCell<Vec<PathBuf>>,
    base_roots: Vec<PathBuf>,
}

impl SourceWorkspace {
    pub(in crate::ui) fn new(
        (base_roots, tree_base_roots): (Vec<PathBuf>, Vec<PathBuf>),
        theme: &Theme,
        source_style_scheme: Option<sourceview5::StyleScheme>,
        source_notebook: gtk::Notebook,
        source_documents: Rc<RefCell<Vec<SourceDocument>>>,
        source_navigation: SourceNavigationControls,
        source_tree: SourceTreeControls,
    ) -> Self {
        Self {
            notebook: source_notebook,
            documents: source_documents,
            navigation: source_navigation,
            tree: source_tree,
            back_history: RefCell::new(Vec::new()),
            forward_history: RefCell::new(Vec::new()),
            closed_tabs: Rc::new(RefCell::new(Vec::new())),
            find_state: RefCell::new(None),
            palette: RefCell::new(None),
            palette_generation: Arc::new(AtomicU64::new(0)),
            loaded_generation: Arc::new(AtomicU64::new(0)),
            loaded_cache: RefCell::new(None),
            loaded_search: RefCell::new(None),
            loaded_files: RefCell::new(Vec::new()),
            tree_roots: RefCell::new(tree_base_roots.clone()),
            tree_base_roots,
            tree_cache: RefCell::new(None),
            tree_search: RefCell::new(None),
            index: RefCell::new(None),
            breakpoint_index: Rc::new(RefCell::new(Default::default())),
            breakpoint_refresh: RefCell::new(Default::default()),
            tree_indexing: Cell::new(false),
            tree_generation: Arc::new(AtomicU64::new(0)),
            tree_render_generation: Arc::new(AtomicU64::new(0)),
            tree_initialized: Cell::new(false),
            execution_path: RefCell::new(None),
            execution_line: Cell::new(None),
            theme: theme.clone(),
            style_scheme: source_style_scheme,
            resolved_paths: RefCell::new(crate::performance::BoundedLruCache::new(
                crate::performance::RESOLVED_SOURCE_PATH_CACHE_BUDGET,
            )),
            open_generation: Arc::new(AtomicU64::new(0)),
            annotation_epoch: Arc::new(AtomicU64::new(0)),
            annotation_pending: RefCell::new(HashMap::new()),
            annotation_cache: RefCell::new(crate::performance::BoundedLruCache::new(
                crate::performance::DISASSEMBLY_SOURCE_CACHE_BUDGET,
            )),
            roots: RefCell::new(base_roots.clone()),
            base_roots,
        }
    }

    pub(in crate::ui) fn add_directory(&self, path: PathBuf) {
        if !self.roots.borrow().contains(&path) {
            self.roots.borrow_mut().push(path.clone());
        }

        if !self.tree_roots.borrow().contains(&path) {
            self.tree_roots.borrow_mut().push(path);
        }
    }

    pub(in crate::ui) fn remove_directory(&self, path: &Path) {
        self.roots.borrow_mut().retain(|root| root != path);
        self.tree_roots.borrow_mut().retain(|root| root != path);
    }

    pub(in crate::ui) fn clear_resolved_paths(&self) {
        self.resolved_paths.borrow_mut().clear();
    }

    pub(in crate::ui) fn invalidate_discovery(&self) {
        self.resolved_paths.borrow_mut().clear();
        self.loaded_cache.borrow_mut().take();
        self.loaded_search.borrow_mut().take();
        self.tree_cache.borrow_mut().take();
        self.tree_search.borrow_mut().take();
        self.index.borrow_mut().take();
        self.tree.file_routes.borrow_mut().clear();
        self.tree_generation.fetch_add(1, Ordering::Relaxed);
        self.tree_render_generation.fetch_add(1, Ordering::Relaxed);

        // A worker for the old generation may still complete, but it must not
        // keep a new generation from starting its own index.
        self.tree_indexing.set(false);
    }

    pub(in crate::ui) fn configure_session(&self, session_directory: Option<&Path>) -> bool {
        self.close_palette();
        self.loaded_cache.borrow_mut().take();
        self.loaded_search.borrow_mut().take();
        self.loaded_generation.fetch_add(1, Ordering::Relaxed);
        self.tree_render_generation.fetch_add(1, Ordering::Relaxed);

        let mut resolution_roots = self.base_roots.clone();
        prioritize_source_root(&mut resolution_roots, session_directory);

        if *self.roots.borrow() != resolution_roots {
            self.roots.replace(resolution_roots);
            self.resolved_paths.borrow_mut().clear();
        }

        let mut tree_roots = self.tree_base_roots.clone();
        prioritize_source_root(&mut tree_roots, session_directory);

        let changed = *self.tree_roots.borrow() != tree_roots;

        if changed {
            self.tree_roots.replace(tree_roots);
            self.tree_cache.borrow_mut().take();
            self.tree_search.borrow_mut().take();
            self.index.borrow_mut().take();
            self.tree.file_routes.borrow_mut().clear();
            self.tree_indexing.set(false);
            self.tree_generation.fetch_add(1, Ordering::Relaxed);
        }

        changed
    }
}

pub(in crate::ui) fn prioritize_source_root(roots: &mut Vec<PathBuf>, priority: Option<&Path>) {
    let Some(priority) = priority else {
        return;
    };

    if let Some(index) = roots.iter().position(|root| root == priority) {
        roots.remove(index);
    }

    roots.insert(0, priority.to_path_buf());
}

impl Drop for SourceWorkspace {
    fn drop(&mut self) {
        self.invalidate_io();
        self.loaded_generation.fetch_add(1, Ordering::Relaxed);
        self.palette_generation.fetch_add(1, Ordering::Relaxed);
        self.tree_generation.fetch_add(1, Ordering::Relaxed);
        self.tree_render_generation.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display, run separately from other GTK tests"]
    fn source_publication_and_navigation_reject_superseded_work() {
        gtk::init().unwrap();
        let theme = Theme::graphite();
        let notebook = build_source_notebook(None);
        let panel = build_editor_panel(&notebook);
        let source = SourceWorkspace::new(
            (vec![PathBuf::from("/base")], vec![PathBuf::from("/base")]),
            &theme,
            None,
            notebook,
            Rc::default(),
            panel.navigation,
            build_source_tree_view(),
        );
        let files = Arc::new(vec![PathBuf::from("/base/source.c")]);
        let index = Arc::new(source::SourceIndex::new(&files, &[]));
        let search = Arc::new(source::SourceSearchIndex::new(&files));
        assert!(source.publish_tree_index(
            0,
            Arc::clone(&files),
            Arc::clone(&index),
            Arc::clone(&search)
        ));
        assert_eq!(
            source.publish_loaded_files(0, (*files).clone(), None, Arc::clone(&search)),
            Some(false)
        );
        assert!(source.configure_session(Some(Path::new("/next"))));
        assert!(!source.publish_tree_index(0, Arc::clone(&files), index, Arc::clone(&search)));
        assert_eq!(
            source.publish_loaded_files(0, (*files).clone(), None, search),
            None
        );
        assert!(source.index_snapshot().is_none());
        assert!(source.loaded_cache.borrow().is_none());
        let pending = Arc::new(AtomicBool::new(true));
        source
            .annotation_pending
            .borrow_mut()
            .insert(PathBuf::from("source.c"), Arc::clone(&pending));
        let generation = source.open_generation.load(Ordering::Relaxed);
        source.invalidate_io();
        assert_ne!(source.open_generation.load(Ordering::Relaxed), generation);
        assert!(!pending.load(Ordering::Relaxed));
        assert!(source.annotation_pending.borrow().is_empty());
        let first = SourceNavigationLocation {
            path: PathBuf::from("first.c"),
            line: 3,
        };
        let second = SourceNavigationLocation {
            path: PathBuf::from("second.c"),
            line: 9,
        };
        source.back_history.borrow_mut().push(first.clone());
        source.complete_history_navigation(false, &second, None, true);
        assert_eq!(source.back_history.borrow().len(), 1);
        source.complete_history_navigation(false, &first, Some(second.clone()), false);
        assert_eq!(source.back_history.borrow().len(), 1);
        source.complete_history_navigation(false, &first, Some(second), true);
        assert!(source.back_history.borrow().is_empty());
        assert_eq!(source.forward_history.borrow().len(), 1);
        let lifetime = Arc::clone(&source.tree_generation);
        let generation = lifetime.load(Ordering::Relaxed);
        drop(source);
        assert_ne!(lifetime.load(Ordering::Relaxed), generation);
    }
}
