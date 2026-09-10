use super::*;

mod array_controls;
mod linked_controls;
use crate::debugger::array::{ArrayPage, ArrayShape};
use crate::debugger::linked::{LinkedListAction, LinkedListProgress};
use array_controls::ArrayControls;
use linked_controls::LinkedControls;

const ARRAY_VIEWER_LIMIT: usize = crate::debugger::array::PAGE_LIMIT;
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
    NativeArray {
        limit: usize,
    },
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
        let native_array_registered = registry.register(NativeArrayProvider);
        let list_registered = registry.register(LinkedListViewerProvider);
        debug_assert!(array_registered && native_array_registered && list_registered);

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

struct NativeArrayProvider;

impl VariableViewerProvider for NativeArrayProvider {
    fn descriptor(&self) -> VariableViewerDescriptor {
        VariableViewerDescriptor {
            id: String::from("native-array"),
            title: String::from("Array / sequence"),
            detail: String::from("Page through native dimensions, bounds, and strided slices"),
            plan: VariableViewerPlan::NativeArray {
                limit: ARRAY_VIEWER_LIMIT,
            },
        }
    }

    fn supports(&self, variable: &Variable) -> bool {
        viewer_can_inspect(variable)
            && variable
                .type_name
                .as_deref()
                .is_some_and(crate::language::has_native_array_bounds)
    }
}

impl VariableViewerProvider for ArrayViewerProvider {
    fn descriptor(&self) -> VariableViewerDescriptor {
        VariableViewerDescriptor {
            id: String::from("indexed-children"),
            title: String::from("Array / sequence"),
            detail: String::from("Browse bounded array ranges and sequence pages"),
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
            detail: String::from(
                "Page through node links with cached navigation and cycle detection",
            ),
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
    pub(crate) ordinal: String,
    pub(crate) name: String,
    pub(crate) value: String,
    pub(crate) type_name: String,
    pub(crate) details: String,
    pub(crate) link: String,
}

pub(crate) struct VariableViewerSession {
    window: glib::WeakRef<gtk::Window>,
    store: gio::ListStore,
    status: gtk::Label,
    shown: Cell<usize>,
    revision: Cell<u64>,
    array: Option<Rc<ArrayControls>>,
    linked: Option<Rc<LinkedControls>>,
}

impl VariableViewerSession {
    #[cfg(test)]
    pub(crate) fn linked_test_session(limit: usize) -> (Rc<Self>, gtk::Window) {
        let linked = LinkedControls::new(limit);
        let root = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
        components::inset(&root, components::DIALOG_INSET);
        root.append(&linked.root);
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let table = variable_viewer_table(
            gtk::NoSelection::new(Some(store.clone())),
            false,
            &crate::ui::ColumnLayouts::default().table(crate::ui::TableId::LinkedListViewer),
        );

        root.append(
            &gtk::ScrolledWindow::builder()
                .vexpand(true)
                .child(&table)
                .build(),
        );
        let status = gtk::Label::new(None);
        root.append(&status);
        let window = gtk::Window::builder()
            .default_width(1150)
            .default_height(600)
            .child(&root)
            .build();
        let session = Rc::new(Self {
            window: window.downgrade(),
            store,
            status,
            shown: Cell::new(0),
            revision: Cell::new(0),
            array: None,
            linked: Some(linked),
        });

        session.bind_window_lifetime(&window);
        window.present();
        (session, window)
    }

    fn bind_window_lifetime(self: &Rc<Self>, window: &gtk::Window) {
        let lifetime = Cell::new(Some(Rc::clone(self)));

        let release = Rc::new(move || {
            if let Some(session) = lifetime.take() {
                session.revision.set(session.revision.get().wrapping_add(1));
            }
        });

        let on_hide = Rc::clone(&release);
        window.connect_hide(move |_| on_hide());
        let on_close = Rc::clone(&release);

        window.connect_close_request(move |_| {
            on_close();
            glib::Propagation::Proceed
        });

        window.connect_destroy(move |_| release());
    }

    pub(crate) fn is_open(&self) -> bool {
        self.window
            .upgrade()
            .is_some_and(|window| window.is_visible())
    }

    pub(crate) fn configure_array(&self, shape: ArrayShape) -> Result<(), &'static str> {
        self.array
            .as_ref()
            .ok_or("This viewer is not an array")?
            .configure(shape)
    }

    pub(crate) fn connect_linked(&self, handler: impl Fn(LinkedListAction) + 'static) {
        if let Some(linked) = &self.linked {
            linked.connect(handler);
        }
    }

    pub(crate) fn begin_linked(&self) -> u64 {
        self.revision.set(self.revision.get().wrapping_add(1));
        self.store.remove_all();
        self.shown.set(0);
        self.finish("Following bounded linked-list page…");

        if let Some(linked) = &self.linked {
            linked.begin();
        }

        self.revision.get()
    }

    pub(crate) fn show_linked_page(
        &self,
        rows: impl IntoIterator<Item = VariableViewerRow>,
        progress: LinkedListProgress,
        message: &str,
    ) {
        self.store.remove_all();
        self.shown.set(0);
        self.append(rows);
        self.update_linked_progress(progress, message);
    }

    pub(crate) fn update_linked_progress(&self, progress: LinkedListProgress, message: &str) {
        if let Some(linked) = &self.linked {
            linked.update(progress);
        }

        self.finish(message);
    }

    pub(crate) fn connect_array_query(&self, handler: impl Fn(ArrayPage) + 'static) {
        if let Some(array) = &self.array {
            array.connect_query(handler);
            array.submit(0);
        }
    }

    pub(crate) fn begin_page(&self, page: &ArrayPage) -> u64 {
        self.revision.set(self.revision.get().wrapping_add(1));
        self.store.remove_all();
        self.shown.set(0);
        self.status.remove_css_class("status-error");
        self.status.set_text("Loading bounded array page…");

        if let Some(array) = &self.array {
            array.begin(page);
        }

        self.revision.get()
    }

    pub(crate) fn page_is_current(&self, revision: u64) -> bool {
        self.is_open() && self.revision.get() == revision
    }

    fn cancel_page(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
        self.finish_page("Cancelled · Partial page retained", false);
    }

    pub(crate) fn finish_page(&self, message: &str, ended: bool) {
        if let Some(array) = &self.array {
            array.complete(self.shown.get(), ended);
        }

        self.finish(message);
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
        if let Some(array) = &self.array {
            array.complete(self.shown.get(), true);
        }

        self.status.add_css_class("status-error");
        self.status.set_text(message);
    }
}

impl Ui {
    pub(crate) fn begin_variable_viewer(
        &self,
        request: &VariableViewerRequest,
        origin: &gtk::Widget,
    ) -> Option<Rc<VariableViewerSession>> {
        let panel = self.panels.containing(origin)?;
        let parent = self.panels.window(panel)?;
        let window = gtk::Window::builder()
            .title(format!(
                "{} - {}",
                request.descriptor.title, request.variable.name
            ))
            .transient_for(&parent)
            .destroy_with_parent(true)
            .default_width(
                if matches!(
                    request.descriptor.plan,
                    VariableViewerPlan::LinkedList { .. }
                ) {
                    1150
                } else {
                    900
                },
            )
            .default_height(620)
            .build();

        window.add_css_class("variable-viewer-window");
        self.panels.track_dialog(panel, &window);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        components::inset(&root, components::DIALOG_INSET);
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
        let array = match request.descriptor.plan {
            VariableViewerPlan::NativeArray { limit }
            | VariableViewerPlan::IndexedChildren { limit } => Some(ArrayControls::new(limit)),
            VariableViewerPlan::LinkedList { .. } => None,
        };

        if let Some(array) = &array {
            root.append(&array.root);
        }

        let linked = match request.descriptor.plan {
            VariableViewerPlan::LinkedList { limit, .. } => Some(LinkedControls::new(limit)),
            _ => None,
        };

        if let Some(linked) = &linked {
            root.append(&linked.root);
        }

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

            query.split_whitespace().all(|term| {
                [
                    &row.ordinal,
                    &row.name,
                    &row.value,
                    &row.type_name,
                    &row.details,
                    &row.link,
                ]
                .iter()
                .any(|field| {
                    field
                        .as_bytes()
                        .windows(term.len())
                        .any(|window| window.eq_ignore_ascii_case(term.as_bytes()))
                })
            })
        });

        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let search = source_search_entry("Filter index, name, value, type, or field");
        let query_for_search = Rc::clone(&query);

        search.connect_changed(move |search| {
            query_for_search.replace(search.text().trim().to_ascii_lowercase());
            filter.changed(gtk::FilterChange::Different);
        });

        root.append(&search);
        let selection = gtk::NoSelection::new(Some(filtered));
        let array_viewer = !matches!(
            request.descriptor.plan,
            VariableViewerPlan::LinkedList { .. }
        );

        let id = if array_viewer {
            TableId::ArrayViewer
        } else {
            TableId::LinkedListViewer
        };

        let view = variable_viewer_table(selection, array_viewer, &self.column_layouts.table(id));

        let scrolled = gtk::ScrolledWindow::builder()
            .child(&view)
            .vexpand(true)
            .overlay_scrolling(false)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .build();

        root.append(&scrolled);
        let status = gtk::Label::new(Some("Loading bounded debugger data..."));
        status.add_css_class("muted");
        status.set_halign(gtk::Align::Fill);
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.set_wrap_mode(pango::WrapMode::WordChar);
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

        let session = Rc::new(VariableViewerSession {
            window: window.downgrade(),
            store,
            status,
            shown: Cell::new(0),
            revision: Cell::new(0),
            array,
            linked,
        });

        if let Some(array) = &session.array {
            let weak = Rc::downgrade(&session);

            array.connect_cancel(move || {
                if let Some(session) = weak.upgrade() {
                    session.cancel_page();
                }
            });
        }

        session.bind_window_lifetime(&window);
        Some(session)
    }
}

fn variable_viewer_table(
    selection: gtk::NoSelection,
    array_viewer: bool,
    layout: &TableLayout,
) -> gtk::ColumnView {
    let view = components::column_view(selection);
    view.add_css_class("debug-table");
    view.add_css_class("variable-viewer-table");
    view.set_vexpand(true);

    for (key, title, width, field) in [
        ("index", "INDEX", if array_viewer { 160 } else { 75 }, 0_u8),
        ("owner", "NODE / OWNER", 180, 1),
        ("link", "LINK", 230, 4),
        ("value", "VALUE / FIELDS", 360, 2),
        ("type", "TYPE", 240, 3),
    ] {
        if array_viewer && matches!(field, 1 | 4) {
            continue;
        }

        layout.append(&view, key, &variable_viewer_column(title, width, field));
    }

    view
}

fn variable_viewer_column(title: &str, width: i32, field: u8) -> gtk::ColumnViewColumn {
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
        enable_recycled_text_selection(&label);
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
        clear_label_selection(&label);

        let text = match field {
            0 => &row.ordinal,
            1 => &row.name,
            2 if !row.details.is_empty() => &row.details,
            2 => &row.value,
            4 => &row.link,
            _ => &row.type_name,
        };

        label.set_text(text);

        let identity = if row.name.is_empty() {
            &row.ordinal
        } else {
            &row.name
        };

        let mut tooltip = format!("{identity}\n{}\n{}", row.value, row.type_name);

        for detail in [&row.link, &row.details] {
            if !detail.is_empty() {
                tooltip.push('\n');
                tooltip.push_str(detail);
            }
        }

        label.set_tooltip_text(Some(&tooltip));
    });

    components::table_column(title, width, factory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display, run separately from other GTK tests"]
    fn type_column_grows_when_its_left_divider_moves_left() {
        gtk::init().unwrap();
        Theme::graphite().install();
        let main = glib::MainContext::default();

        for array_viewer in [true, false] {
            let store = gio::ListStore::new::<glib::BoxedAnyObject>();

            store.append(&glib::BoxedAnyObject::new(VariableViewerRow {
                ordinal: String::from("[0]"),
                name: String::from("item"),
                value: String::from("\"two words\""),
                type_name: String::from(
                    "std::basic_string<char, std::char_traits<char>, std::allocator<char>>",
                ),
                details: String::new(),
                link: String::new(),
            }));

            let view = variable_viewer_table(
                gtk::NoSelection::new(Some(store)),
                array_viewer,
                &ColumnLayouts::default().table(if array_viewer {
                    TableId::ArrayViewer
                } else {
                    TableId::LinkedListViewer
                }),
            );

            let scroll = gtk::ScrolledWindow::builder()
                .child(&view)
                .overlay_scrolling(false)
                .build();

            let window = gtk::Window::builder()
                .default_width(1200)
                .default_height(300)
                .child(&scroll)
                .build();

            window.present();
            main.block_on(glib::timeout_future(Duration::from_millis(80)));
            let header = view.first_child().unwrap();
            assert_eq!(header.css_name(), "header");
            let type_header = header.last_child().unwrap();
            let before = type_header.compute_bounds(&view).unwrap();
            let columns = view.columns();

            let value = columns
                .item(columns.n_items() - 2)
                .and_downcast::<gtk::ColumnViewColumn>()
                .unwrap();

            assert!(value.is_resizable());
            let original_width = value.fixed_width();

            // GTK's header drag adjusts the preceding column's fixed width.
            for distance in [120, 40, 0] {
                value.set_fixed_width(original_width - distance);
                main.block_on(glib::timeout_future(Duration::from_millis(50)));
                let after = type_header.compute_bounds(&view).unwrap();
                assert!((before.x() - after.x() - distance as f32).abs() <= 1.0);
                assert!((after.width() - before.width() - distance as f32).abs() <= 1.0);
                assert!(scroll.hadjustment().upper() <= scroll.hadjustment().page_size());
            }

            window.close();
        }
    }

    fn variable(type_name: &str) -> Variable {
        Variable {
            local_index: None,
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

    #[test]
    fn d_sequences_and_ada_arrays_use_the_existing_bounded_viewers() {
        let registry = VariableViewerRegistry::with_builtins();
        let d = registry.matching(&variable("int[]"));
        assert_eq!(d.len(), 1);

        assert!(matches!(
            d[0].plan,
            VariableViewerPlan::IndexedChildren { .. }
        ));

        let ada = registry.matching(&variable("array (-2 .. 2) of integer"));
        assert_eq!(ada.len(), 1);

        assert!(matches!(
            ada[0].plan,
            VariableViewerPlan::NativeArray { .. }
        ));

        let mut null = variable("access fixture.node");
        null.value = "0x0".into();
        assert!(registry.matching(&null).is_empty());
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
