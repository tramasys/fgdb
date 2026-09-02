use super::*;

const ARRAY_VIEWER_LIMIT: usize = 512;
const LINKED_LIST_VIEWER_LIMIT: usize = 128;
const MAX_OPEN_VARIABLE_VIEWERS: usize = 16;

/// A bounded, debugger-side query plan for a variable viewer.
///
/// The UI and the GDB transport consume this common representation, so adding
/// another built-in viewer does not require teaching the context menu about
/// its implementation. A future plugin layer can register providers that
/// produce the same plans.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum VariableViewerPlan {
    IndexedChildren {
        limit: usize,
    },
    LinkedList {
        next_members: Vec<String>,
        limit: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VariableViewerDescriptor {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) detail: String,
    pub(crate) plan: VariableViewerPlan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VariableViewerRequest {
    pub(crate) descriptor: VariableViewerDescriptor,
    pub(crate) variable: Variable,
}

/// Extension point for type-specific variable presentation.
///
/// Providers only decide whether they apply and return a bounded traversal
/// plan. Query scheduling, stale-stop protection, and rendering remain shared,
/// which keeps contributed viewers from bypassing debugger safety limits.
pub(crate) trait VariableViewerProvider {
    fn descriptor(&self) -> VariableViewerDescriptor;
    fn supports(&self, variable: &Variable) -> bool;
}

#[derive(Default)]
pub(crate) struct VariableViewerRegistry {
    providers: Vec<Rc<dyn VariableViewerProvider>>,
}

impl VariableViewerRegistry {
    pub(crate) fn with_builtins() -> Self {
        let mut registry = Self::default();
        let array_registered = registry.register(ArrayViewerProvider);
        let list_registered = registry.register(LinkedListViewerProvider);
        debug_assert!(array_registered && list_registered);

        registry
    }

    pub(crate) fn register(&mut self, provider: impl VariableViewerProvider + 'static) -> bool {
        let descriptor = provider.descriptor();

        if descriptor.id.trim().is_empty()
            || descriptor.title.trim().is_empty()
            || self
                .providers
                .iter()
                .any(|existing| existing.descriptor().id == descriptor.id)
        {
            return false;
        }

        self.providers.push(Rc::new(provider));

        true
    }

    pub(crate) fn matching(&self, variable: &Variable) -> Vec<VariableViewerDescriptor> {
        self.providers
            .iter()
            .filter(|provider| provider.supports(variable))
            .map(|provider| provider.descriptor())
            .collect()
    }
}

struct ArrayViewerProvider;

impl VariableViewerProvider for ArrayViewerProvider {
    fn descriptor(&self) -> VariableViewerDescriptor {
        VariableViewerDescriptor {
            id: String::from("indexed-children"),
            title: String::from("Array / sequence"),
            detail: String::from("Browse indexed elements in a compact table"),
            plan: VariableViewerPlan::IndexedChildren {
                limit: ARRAY_VIEWER_LIMIT,
            },
        }
    }

    fn supports(&self, variable: &Variable) -> bool {
        viewer_can_inspect(variable)
            && (variable.display_hint.as_deref() == Some("array")
                || variable.type_name.as_deref().is_some_and(is_indexed_type))
    }
}

struct LinkedListViewerProvider;

impl VariableViewerProvider for LinkedListViewerProvider {
    fn descriptor(&self) -> VariableViewerDescriptor {
        VariableViewerDescriptor {
            id: String::from("linked-list"),
            title: String::from("Linked list"),
            detail: String::from("Follow next links with cycle detection"),
            plan: VariableViewerPlan::LinkedList {
                next_members: [
                    "next",
                    "next_",
                    "next_node",
                    "next_ptr",
                    "_m_next",
                    "link",
                    "flink",
                    "forward",
                    "forward_link",
                    "successor",
                    "succ",
                ]
                .into_iter()
                .map(String::from)
                .collect(),
                limit: LINKED_LIST_VIEWER_LIMIT,
            },
        }
    }

    fn supports(&self, variable: &Variable) -> bool {
        viewer_can_inspect(variable)
            && !variable.is_null_pointer()
            && (is_linked_name(&variable.name)
                || variable.type_name.as_deref().is_some_and(|type_name| {
                    is_linked_type(type_name) || is_object_pointer(type_name)
                }))
    }
}

fn viewer_can_inspect(variable: &Variable) -> bool {
    let value = variable.value.trim();

    !variable.name.trim().is_empty()
        && (variable.is_available() || value.starts_with("<not available"))
}

fn is_indexed_type(type_name: &str) -> bool {
    let compact = compact_variable_type(type_name);
    let lower = compact.to_ascii_lowercase();

    let native_array = (compact.contains('[') && compact.contains(']'))
        || (compact.starts_with('[') && compact.contains(';'));

    native_array
        || [
            "std::array<",
            "std::vector<",
            "std::deque<",
            "std::list<",
            "std::forward_list<",
            "vec<",
            "vecdeque<",
            "linkedlist<",
            "smallvec<",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
}

fn is_linked_type(type_name: &str) -> bool {
    let compact = compact_variable_type(type_name).to_ascii_lowercase();

    let outer = compact
        .trim()
        .trim_start_matches("const ")
        .trim_start_matches("struct ")
        .trim_start_matches("class ")
        .trim_start_matches(['&', '*'])
        .trim_end_matches(['&', '*', ' '])
        .split('<')
        .next()
        .unwrap_or_default()
        .rsplit("::")
        .next()
        .unwrap_or_default();

    let direct_node = outer.ends_with("node")
        || outer.ends_with("list_node")
        || outer.ends_with("listnode")
        || outer.ends_with("link")
        || outer.ends_with("entry")
        || outer.contains("intrusive_list");

    let known_wrapper = ["rc", "arc", "box", "refcell", "option", "nonnull"].contains(&outer);

    direct_node || (known_wrapper && compact.contains("node"))
}

fn is_object_pointer(type_name: &str) -> bool {
    let compact = compact_variable_type(type_name).to_ascii_lowercase();
    let trimmed = compact.trim();

    let pointer = trimmed.contains('*')
        || trimmed.starts_with("&mut ")
        || trimmed.starts_with("&")
        || trimmed.starts_with("*mut ")
        || trimmed.starts_with("*const ");

    if !pointer || trimmed.contains("(*)") || trimmed.contains("(* ") {
        return false;
    }

    let pointee = trimmed
        .trim_start_matches("const ")
        .trim_start_matches("volatile ")
        .trim_start_matches("&mut ")
        .trim_start_matches('&')
        .trim_start_matches("*mut ")
        .trim_start_matches("*const ")
        .trim_start_matches("struct ")
        .trim_start_matches("class ")
        .trim_end_matches(['*', '&', ' '])
        .trim();

    ![
        "void",
        "bool",
        "char",
        "signed char",
        "unsigned char",
        "wchar_t",
        "char8_t",
        "char16_t",
        "char32_t",
        "short",
        "short int",
        "unsigned short",
        "int",
        "unsigned",
        "unsigned int",
        "long",
        "long int",
        "unsigned long",
        "long long",
        "unsigned long long",
        "float",
        "double",
        "long double",
    ]
    .contains(&pointee)
}

fn is_linked_name(name: &str) -> bool {
    let name = name
        .trim()
        .trim_matches(['[', ']'])
        .rsplit("::")
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();

    ["head", "list", "node", "entry", "first"]
        .iter()
        .any(|hint| name == *hint || name.ends_with(&format!("_{hint}")))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VariableViewerRow {
    pub(crate) ordinal: usize,
    pub(crate) name: String,
    pub(crate) value: String,
    pub(crate) type_name: String,
    pub(crate) details: String,
}

pub(crate) struct VariableViewerSession {
    window: gtk::Window,
    store: gio::ListStore,
    status: gtk::Label,
    shown: Cell<usize>,
}

impl VariableViewerSession {
    pub(crate) fn is_open(&self) -> bool {
        self.window.is_visible()
    }

    pub(crate) fn append(&self, rows: impl IntoIterator<Item = VariableViewerRow>) {
        let rows = rows
            .into_iter()
            .map(glib::BoxedAnyObject::new)
            .collect::<Vec<_>>();

        if rows.is_empty() {
            return;
        }

        self.shown.set(self.shown.get().saturating_add(rows.len()));
        self.store.extend_from_slice(&rows);

        self.status.set_text(&format!(
            "{} item{} loaded",
            self.shown.get(),
            if self.shown.get() == 1 { "" } else { "s" }
        ));
    }

    pub(crate) fn finish(&self, message: &str) {
        self.status.remove_css_class("status-error");
        self.status.set_text(message);
    }

    pub(crate) fn fail(&self, message: &str) {
        self.status.add_css_class("status-error");
        self.status.set_text(message);
    }
}

impl Ui {
    pub(crate) fn begin_variable_viewer(
        &self,
        request: &VariableViewerRequest,
    ) -> Rc<VariableViewerSession> {
        let window = gtk::Window::builder()
            .title(format!(
                "{} - {}",
                request.descriptor.title, request.variable.name
            ))
            .transient_for(&self.window)
            .destroy_with_parent(true)
            .default_width(900)
            .default_height(620)
            .build();

        crate::install_window_icon(&window);
        window.add_css_class("variable-viewer-window");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_margin_top(12);
        root.set_margin_bottom(12);
        root.set_margin_start(12);
        root.set_margin_end(12);
        let identity = gtk::Box::new(gtk::Orientation::Vertical, 3);
        identity.add_css_class("variable-viewer-identity");
        let caption = gtk::Label::new(Some(&request.descriptor.title.to_ascii_uppercase()));
        caption.add_css_class("section-title");
        caption.set_halign(gtk::Align::Start);
        let name = gtk::Label::new(Some(&request.variable.name));
        name.add_css_class("variable-viewer-name");
        name.set_halign(gtk::Align::Start);
        name.set_ellipsize(pango::EllipsizeMode::Middle);

        let type_name = compact_variable_type(
            request
                .variable
                .type_name
                .as_deref()
                .unwrap_or("<unknown type>"),
        );

        let detail = gtk::Label::new(Some(&type_name));
        detail.add_css_class("muted");
        detail.set_halign(gtk::Align::Start);
        detail.set_ellipsize(pango::EllipsizeMode::Middle);
        identity.append(&caption);
        identity.append(&name);
        identity.append(&detail);
        root.append(&identity);
        let query = Rc::new(RefCell::new(String::new()));
        let query_for_filter = Rc::clone(&query);

        let filter = gtk::CustomFilter::new(move |object| {
            let Some(object) = object.downcast_ref::<glib::BoxedAnyObject>() else {
                return false;
            };

            let row = object.borrow::<VariableViewerRow>();
            let query = query_for_filter.borrow();

            if query.is_empty() {
                return true;
            }

            let searchable = format!(
                "{} {} {} {}",
                row.name, row.value, row.type_name, row.details
            )
            .to_ascii_lowercase();

            query
                .split_whitespace()
                .all(|term| searchable.contains(term))
        });

        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let search = source_search_entry("Filter name, value, type, or field");
        let query_for_search = Rc::clone(&query);

        search.connect_changed(move |search| {
            query_for_search.replace(search.text().trim().to_ascii_lowercase());
            filter.changed(gtk::FilterChange::Different);
        });

        root.append(&search);
        let selection = gtk::NoSelection::new(Some(filtered));
        let view = gtk::ColumnView::new(Some(selection));
        view.add_css_class("debug-table");
        view.add_css_class("variable-viewer-table");
        view.set_vexpand(true);

        for (title, width, expand, field) in [
            ("INDEX", 75, false, 0_u8),
            ("NAME / ADDRESS", 180, false, 1),
            ("VALUE / FIELDS", 360, true, 2),
            ("TYPE", 240, false, 3),
        ] {
            view.append_column(&variable_viewer_column(title, width, expand, field));
        }

        let scrolled = gtk::ScrolledWindow::builder()
            .child(&view)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .build();

        root.append(&scrolled);
        let status = gtk::Label::new(Some("Loading bounded debugger data..."));
        status.add_css_class("muted");
        status.set_halign(gtk::Align::Start);
        status.set_ellipsize(pango::EllipsizeMode::End);
        root.append(&status);
        window.set_child(Some(&root));
        connect_escape_to_close(&window);

        let oldest = {
            let mut windows = self.variable_viewer_windows.borrow_mut();
            windows.retain(|window| window.is_visible());

            (windows.len() >= MAX_OPEN_VARIABLE_VIEWERS).then(|| windows.remove(0))
        };

        if let Some(oldest) = oldest {
            oldest.close();
        }

        self.variable_viewer_windows
            .borrow_mut()
            .push(window.clone());

        let windows = Rc::downgrade(&self.variable_viewer_windows);
        let weak_window = window.downgrade();

        window.connect_close_request(move |_| {
            if let (Some(windows), Some(window)) = (windows.upgrade(), weak_window.upgrade()) {
                windows
                    .borrow_mut()
                    .retain(|candidate| candidate != &window);
            }

            glib::Propagation::Proceed
        });

        window.present();

        Rc::new(VariableViewerSession {
            window,
            store,
            status,
            shown: Cell::new(0),
        })
    }
}

fn variable_viewer_column(
    title: &str,
    width: i32,
    expand: bool,
    field: u8,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(|_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };

        let label = gtk::Label::new(None);
        label.add_css_class("debug-table-cell");
        label.set_halign(gtk::Align::Fill);
        label.set_xalign(0.0);
        label.set_ellipsize(pango::EllipsizeMode::Middle);
        enable_stable_text_selection(&label);
        item.set_child(Some(&label));
    });

    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };

        let Some(label) = item.child().and_downcast::<gtk::Label>() else {
            return;
        };

        let Some(data) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };

        let row = data.borrow::<VariableViewerRow>();

        let text = match field {
            0 => row.ordinal.to_string(),
            1 => row.name.clone(),
            2 if !row.details.is_empty() => row.details.clone(),
            2 => row.value.clone(),
            _ => row.type_name.clone(),
        };

        label.set_text(&text);

        label.set_tooltip_text(Some(&format!(
            "{}\n{}\n{}",
            row.name, row.value, row.type_name
        )));
    });

    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_fixed_width(width);
    column.set_expand(expand);
    column.set_resizable(true);

    column
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variable(type_name: &str) -> Variable {
        Variable {
            name: String::from("value"),
            value: String::from("{...}"),
            type_name: Some(type_name.to_owned()),
            argument: false,
            varobj: Some(String::from("var1")),
            num_children: 3,
            has_more: false,
            display_hint: None,
            dynamic: false,
        }
    }

    #[test]
    fn builtins_match_cpp_and_rust_collections() {
        let registry = VariableViewerRegistry::with_builtins();
        let cpp = registry.matching(&variable("std::vector<int>"));
        assert!(cpp.iter().any(|viewer| viewer.id == "indexed-children"));
        let rust = registry.matching(&variable("alloc::vec::Vec<crate::Node>"));
        assert!(rust.iter().any(|viewer| viewer.id == "indexed-children"));
        assert!(!rust.iter().any(|viewer| viewer.id == "linked-list"));
        let rust_node = registry.matching(&variable("crate::Node"));
        assert!(rust_node.iter().any(|viewer| viewer.id == "linked-list"));

        let rust_owner =
            registry.matching(&variable("alloc::rc::Rc<core::cell::RefCell<crate::Node>>"));

        assert!(rust_owner.iter().any(|viewer| viewer.id == "linked-list"));
        let node = registry.matching(&variable("fixture::Node *"));
        assert!(node.iter().any(|viewer| viewer.id == "linked-list"));
        let opaque_node = registry.matching(&variable("fixture::Task *"));
        assert!(opaque_node.iter().any(|viewer| viewer.id == "linked-list"));
        let scalar_pointer = registry.matching(&variable("unsigned long *"));

        assert!(
            !scalar_pointer
                .iter()
                .any(|viewer| viewer.id == "linked-list")
        );

        let mut lazy_array = variable("std::array<int, 4>");
        lazy_array.value = String::from("<not available>");
        lazy_array.varobj = None;
        lazy_array.num_children = 0;

        assert!(
            registry
                .matching(&lazy_array)
                .iter()
                .any(|viewer| viewer.id == "indexed-children")
        );

        lazy_array.value = String::from("<optimized out>");
        assert!(registry.matching(&lazy_array).is_empty());
    }

    struct CustomViewer;

    impl VariableViewerProvider for CustomViewer {
        fn descriptor(&self) -> VariableViewerDescriptor {
            VariableViewerDescriptor {
                id: String::from("custom"),
                title: String::from("Custom"),
                detail: String::from("Test viewer"),
                plan: VariableViewerPlan::IndexedChildren { limit: 4 },
            }
        }

        fn supports(&self, variable: &Variable) -> bool {
            variable.type_name.as_deref() == Some("CustomType")
        }
    }

    #[test]
    fn registry_accepts_additional_providers() {
        let mut registry = VariableViewerRegistry::default();
        assert!(registry.register(CustomViewer));
        assert!(!registry.register(CustomViewer));
        assert_eq!(registry.matching(&variable("CustomType"))[0].id, "custom");
    }
}
