use super::*;

static NEXT_MEMORY_WATCH_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy)]
enum MemoryRowColumn {
    Address,
    Offset,
    Value,
    Decoded,
    Interpretation,
}

#[derive(Clone, PartialEq, Eq)]
struct MemoryRowData {
    address: u64,
    offset: usize,
    value: String,
    decoded: String,
    interpretation: String,
    pointer: Option<u64>,
    changed: bool,
    kind: MemoryKind,
    pointer_bits: u32,
}

struct MemoryRenderContext<'a> {
    pointer_bits: u32,
    endian: TargetEndian,
    previous_begin: Option<u64>,
    previous: &'a [u8],
    regions: &'a [MemoryRegion],
}

pub(super) fn add_memory_watch(
    container: &MemoryWatchContainer,
    watches: &Rc<RefCell<Vec<MemoryWatchView>>>,
    handler: &Rc<RefCell<Option<MemoryWatchHandler>>>,
    expression: String,
    byte_count: usize,
    format: MemoryWatchFormat,
) -> bool {
    let existing = {
        let watches = watches.borrow();

        watches
            .iter()
            .find(|watch| {
                watch.expression == expression
                    && watch.byte_count == byte_count
                    && watch.format == format
            })
            .cloned()
    };

    if let Some(watch) = existing {
        select_memory_watch(container, &watch);
        request_memory_watch(&watch, handler);
        return true;
    }

    if watches.borrow().len() >= MAX_MEMORY_WATCHES {
        return false;
    }

    let id = NEXT_MEMORY_WATCH_ID.fetch_add(1, Ordering::Relaxed).max(1);
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.add_css_class("memory-watch-page");
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    toolbar.add_css_class("memory-watch-toolbar");
    let title = gtk::Label::new(Some(&expression));
    title.add_css_class("memory-watch-expression");
    title.set_halign(gtk::Align::Start);
    title.set_ellipsize(pango::EllipsizeMode::Middle);
    title.set_hexpand(true);
    title.set_tooltip_text(Some(&expression));
    enable_stable_text_selection(&title);
    let offset = gtk::Label::new(Some("base"));
    offset.add_css_class("memory-watch-offset");
    let previous = memory_toolbar_button("‹ Page", "Read the preceding block");
    let base = memory_toolbar_button("Base", "Return to the original expression");
    base.set_sensitive(false);
    let next = memory_toolbar_button("Page ›", "Read the following block");
    let follow = memory_toolbar_button("Follow pointer", "Inspect the selected pointer value");
    follow.set_sensitive(false);
    let refresh = memory_toolbar_button("Refresh", "Read this memory again");
    let remove = memory_toolbar_button("Close", "Close this memory inspector");
    remove.add_css_class("danger-action");
    toolbar.append(&title);
    toolbar.append(&offset);
    toolbar.append(&previous);
    toolbar.append(&base);
    toolbar.append(&next);
    toolbar.append(&follow);
    toolbar.append(&refresh);
    toolbar.append(&remove);
    page.append(&toolbar);
    let summary = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    summary.add_css_class("memory-watch-summary");
    let status = gtk::Label::new(Some("reading…"));
    status.set_halign(gtk::Align::Start);
    status.set_ellipsize(pango::EllipsizeMode::Middle);
    status.set_hexpand(true);
    enable_stable_text_selection(&status);

    let range = gtk::Label::new(Some(&format!(
        "{} · {}",
        format_memory_size(byte_count as u64),
        format.label()
    )));

    range.add_css_class("memory-watch-range");
    range.set_halign(gtk::Align::End);
    enable_stable_text_selection(&range);
    summary.append(&status);
    summary.append(&range);
    page.append(&summary);
    let (view, store, selection) = build_memory_watch_table();

    let scrolled = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();

    page.append(&scrolled);
    let tab = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tab.add_css_class("memory-watch-tab");
    let tab_label = gtk::Label::new(Some(&expression));
    tab_label.add_css_class("memory-watch-tab-label");
    tab_label.set_ellipsize(pango::EllipsizeMode::Middle);
    tab_label.set_width_chars(18);
    tab_label.set_max_width_chars(22);
    tab_label.set_hexpand(true);
    tab_label.set_tooltip_text(Some(&expression));
    let tab_close = gtk::Button::with_label("×");
    tab_close.add_css_class("memory-watch-tab-close");
    tab_close.set_focus_on_click(false);
    tab_close.set_tooltip_text(Some("Close this memory inspector"));
    tab.append(&tab_label);
    tab.append(&tab_close);
    container.notebook.append_page(&page, Some(&tab));

    let watch = MemoryWatchView {
        id,
        expression,
        byte_count,
        format,
        page,
        page_offset: Rc::new(Cell::new(0)),
        status,
        range,
        offset,
        store,
        selection: selection.clone(),
        follow_button: follow.clone(),
        previous_begin: Rc::new(Cell::new(None)),
        previous_bytes: Rc::new(RefCell::new(Vec::new())),
    };

    watches.borrow_mut().push(watch.clone());
    update_memory_container_state(container, false);
    select_memory_watch(container, &watch);
    let weak_follow = follow.downgrade();

    selection.connect_selected_notify(move |selection| {
        if let Some(follow) = weak_follow.upgrade() {
            follow.set_sensitive(selected_memory_pointer(selection).is_some());
        }
    });

    connect_memory_page_navigation(&previous, id, -(byte_count as i64), watches, handler);
    connect_memory_page_navigation(&next, id, byte_count as i64, watches, handler);
    let weak_watches = Rc::downgrade(watches);
    let handler_for_base = Rc::clone(handler);

    base.connect_clicked(move |button| {
        let Some(watches) = weak_watches.upgrade() else {
            return;
        };

        let watch = watches
            .borrow()
            .iter()
            .find(|watch| watch.id == id)
            .cloned();

        let Some(watch) = watch else {
            return;
        };

        watch.page_offset.set(0);
        update_memory_watch_offset(&watch);
        button.set_sensitive(false);
        request_memory_watch(&watch, &handler_for_base);
    });

    let weak_watches = Rc::downgrade(watches);
    let handler_for_refresh = Rc::clone(handler);

    refresh.connect_clicked(move |_| {
        let Some(watches) = weak_watches.upgrade() else {
            return;
        };

        if let Some(watch) = watches
            .borrow()
            .iter()
            .find(|watch| watch.id == id)
            .cloned()
        {
            request_memory_watch(&watch, &handler_for_refresh);
        }
    });

    connect_memory_follow(&follow, id, container, watches, handler);
    connect_memory_remove(&remove, id, container, watches);
    connect_memory_remove(&tab_close, id, container, watches);
    request_memory_watch(&watch, handler);

    true
}

fn memory_toolbar_button(label: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("inline-action");
    button.set_tooltip_text(Some(tooltip));

    button
}

fn connect_memory_page_navigation(
    button: &gtk::Button,
    id: u64,
    delta: i64,
    watches: &Rc<RefCell<Vec<MemoryWatchView>>>,
    handler: &Rc<RefCell<Option<MemoryWatchHandler>>>,
) {
    let weak_watches = Rc::downgrade(watches);
    let handler = Rc::clone(handler);

    button.connect_clicked(move |_| {
        let Some(watches) = weak_watches.upgrade() else {
            return;
        };

        let watch = watches
            .borrow()
            .iter()
            .find(|watch| watch.id == id)
            .cloned();

        let Some(watch) = watch else {
            return;
        };

        let Some(offset) = watch.page_offset.get().checked_add(delta) else {
            return;
        };

        watch.page_offset.set(offset);
        update_memory_watch_offset(&watch);
        request_memory_watch(&watch, &handler);
    });
}

fn connect_memory_follow(
    button: &gtk::Button,
    id: u64,
    container: &MemoryWatchContainer,
    watches: &Rc<RefCell<Vec<MemoryWatchView>>>,
    handler: &Rc<RefCell<Option<MemoryWatchHandler>>>,
) {
    let weak_notebook = container.notebook.downgrade();
    let weak_empty = container.empty.downgrade();
    let weak_refresh_all = container.refresh_all.downgrade();
    let weak_clear_all = container.clear_all.downgrade();
    let weak_watches = Rc::downgrade(watches);
    let refresh_batch = Rc::clone(&container.refresh_batch);
    let commands_available = Rc::clone(&container.commands_available);
    let handler = Rc::clone(handler);

    button.connect_clicked(move |_| {
        let (Some(notebook), Some(empty), Some(refresh_all), Some(clear_all), Some(watches)) = (
            weak_notebook.upgrade(),
            weak_empty.upgrade(),
            weak_refresh_all.upgrade(),
            weak_clear_all.upgrade(),
            weak_watches.upgrade(),
        ) else {
            return;
        };

        let pointer = watches
            .borrow()
            .iter()
            .find(|watch| watch.id == id)
            .and_then(|watch| selected_memory_pointer(&watch.selection));

        let Some(pointer) = pointer else {
            return;
        };

        let container = MemoryWatchContainer {
            notebook,
            empty,
            refresh_all,
            clear_all,
            refresh_batch: Rc::clone(&refresh_batch),
            commands_available: Rc::clone(&commands_available),
        };

        let _ = add_memory_watch(
            &container,
            &watches,
            &handler,
            format!("0x{pointer:x}"),
            128,
            MemoryWatchFormat::Bytes,
        );
    });
}

fn connect_memory_remove(
    button: &gtk::Button,
    id: u64,
    container: &MemoryWatchContainer,
    watches: &Rc<RefCell<Vec<MemoryWatchView>>>,
) {
    let weak_notebook = container.notebook.downgrade();
    let weak_empty = container.empty.downgrade();
    let weak_refresh_all = container.refresh_all.downgrade();
    let weak_clear_all = container.clear_all.downgrade();
    let weak_watches = Rc::downgrade(watches);
    let refresh_batch = Rc::clone(&container.refresh_batch);
    let commands_available = Rc::clone(&container.commands_available);

    button.connect_clicked(move |_| {
        let (Some(notebook), Some(empty), Some(refresh_all), Some(clear_all), Some(watches)) = (
            weak_notebook.upgrade(),
            weak_empty.upgrade(),
            weak_refresh_all.upgrade(),
            weak_clear_all.upgrade(),
            weak_watches.upgrade(),
        ) else {
            return;
        };

        let page = watches
            .borrow()
            .iter()
            .find(|watch| watch.id == id)
            .map(|watch| watch.page.clone());

        if let Some(page) = page
            && let Some(position) = notebook.page_num(&page)
        {
            notebook.remove_page(Some(position));
        }

        watches.borrow_mut().retain(|watch| watch.id != id);
        let reading = refresh_batch.borrow_mut().remove(id);

        update_memory_container_state(
            &MemoryWatchContainer {
                notebook,
                empty,
                refresh_all,
                clear_all,
                refresh_batch: Rc::clone(&refresh_batch),
                commands_available: Rc::clone(&commands_available),
            },
            reading,
        );
    });
}

pub(super) fn clear_memory_watches(
    container: &MemoryWatchContainer,
    watches: &Rc<RefCell<Vec<MemoryWatchView>>>,
) {
    while container.notebook.n_pages() > 0 {
        container.notebook.remove_page(Some(0));
    }

    watches.borrow_mut().clear();
    container.refresh_batch.borrow_mut().clear();
    update_memory_container_state(container, false);
}

pub(super) fn update_memory_container_state(container: &MemoryWatchContainer, reading: bool) {
    let has_watches = container.notebook.n_pages() > 0;
    container.notebook.set_visible(has_watches);
    container.empty.set_visible(!has_watches);

    container
        .refresh_all
        .set_sensitive(has_watches && !reading && container.commands_available.get());

    container.clear_all.set_sensitive(has_watches);
}

fn select_memory_watch(container: &MemoryWatchContainer, watch: &MemoryWatchView) {
    if let Some(position) = container.notebook.page_num(&watch.page) {
        container.notebook.set_current_page(Some(position));
    }
}

pub(super) fn request_memory_watch(
    watch: &MemoryWatchView,
    handler: &Rc<RefCell<Option<MemoryWatchHandler>>>,
) {
    set_memory_watch_reading(watch);
    let handler = handler.borrow().clone();

    if let Some(handler) = handler {
        handler(
            watch.id,
            memory_watch_request_expression(watch),
            watch.byte_count,
        );
    }
}

pub(super) fn set_memory_watch_reading(watch: &MemoryWatchView) {
    watch.status.remove_css_class("memory-watch-error");
    watch.status.set_text("reading…");
}

pub(super) fn memory_watch_request_expression(watch: &MemoryWatchView) -> String {
    memory_watch_request_expression_at(&watch.expression, watch.page_offset.get())
}

fn memory_watch_request_expression_at(expression: &str, offset: i64) -> String {
    match offset.cmp(&0) {
        std::cmp::Ordering::Less => format!("({})-0x{:x}", expression, offset.unsigned_abs()),
        std::cmp::Ordering::Equal => expression.to_owned(),
        std::cmp::Ordering::Greater => format!("({expression})+0x{offset:x}"),
    }
}

fn update_memory_watch_offset(watch: &MemoryWatchView) {
    let offset = watch.page_offset.get();

    let text = match offset.cmp(&0) {
        std::cmp::Ordering::Less => format!("−0x{:x}", offset.unsigned_abs()),
        std::cmp::Ordering::Equal => String::from("base"),
        std::cmp::Ordering::Greater => format!("+0x{offset:x}"),
    };

    watch.offset.set_text(&text);
}

pub(super) fn show_memory_watch_data(
    watch: &MemoryWatchView,
    memory: MemoryBlock,
    regions: &[MemoryRegion],
    pointer_bits: u32,
    endian: TargetEndian,
) {
    let previous_begin = watch.previous_begin.get();
    let previous = watch.previous_bytes.borrow();

    let context = MemoryRenderContext {
        pointer_bits,
        endian,
        previous_begin,
        regions,
        previous: &previous,
    };

    let rows = format_memory_rows(memory.begin, &memory.bytes, watch.format, &context);
    let changed = rows.iter().filter(|row| row.changed).count();
    drop(previous);

    if replace_boxed_store_if_changed(&watch.store, rows) {
        watch.selection.set_selected(gtk::INVALID_LIST_POSITION);
        watch.follow_button.set_sensitive(false);
    }

    let byte_count = memory.bytes.len();
    watch.previous_begin.set(Some(memory.begin));
    watch.previous_bytes.replace(memory.bytes);
    let width = usize::try_from(pointer_bits / 4).unwrap_or(16).clamp(8, 16);
    let end = memory.begin.saturating_add(byte_count as u64);

    let region = memory_region_for_address(regions, memory.begin)
        .map(MemoryRegion::description)
        .unwrap_or_else(|| String::from("unmapped"));

    watch.status.remove_css_class("memory-watch-error");
    let status = format!("0x{:0width$x} · {region}", memory.begin);
    watch.status.set_text(&status);
    watch.status.set_tooltip_text(Some(&status));

    let change_text = if previous_begin == Some(memory.begin) {
        format!(" · {changed} changed row(s)")
    } else {
        String::new()
    };

    let range = format!(
        "[0x{:0width$x}, 0x{:0width$x}) · {} · {}{change_text}",
        memory.begin,
        end,
        format_memory_size(byte_count as u64),
        watch.format.label(),
    );

    watch.range.set_text(&range);
    watch.range.set_tooltip_text(Some(&range));
}

pub(super) fn show_memory_watch_error(watch: &MemoryWatchView, error: &str) {
    watch.status.add_css_class("memory-watch-error");
    watch.status.set_text(error);
    watch.status.set_tooltip_text(Some(error));
    watch.range.set_text("");
    watch.store.remove_all();
    watch.follow_button.set_sensitive(false);
}

fn selected_memory_pointer(selection: &gtk::SingleSelection) -> Option<u64> {
    selection
        .selected_item()
        .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
        .and_then(|item| item.borrow::<MemoryRowData>().pointer)
        .filter(|pointer| *pointer != 0)
}

fn build_memory_watch_table() -> (gtk::ColumnView, gio::ListStore, gtk::SingleSelection) {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection.clone()));
    view.add_css_class("debug-table");
    view.add_css_class("memory-watch-table");
    view.set_vexpand(true);
    view.set_reorderable(true);

    for (title, width, expand, column) in [
        ("ADDRESS", 180, false, MemoryRowColumn::Address),
        ("OFFSET", 75, false, MemoryRowColumn::Offset),
        ("VALUE", 300, true, MemoryRowColumn::Value),
        ("DECODED", 170, false, MemoryRowColumn::Decoded),
        (
            "INTERPRETATION / TARGET",
            290,
            true,
            MemoryRowColumn::Interpretation,
        ),
    ] {
        view.append_column(&memory_watch_column(
            title, width, expand, column, &selection,
        ));
    }

    (view, store, selection)
}

fn memory_watch_column(
    title: &str,
    width: i32,
    expand: bool,
    column: MemoryRowColumn,
    selection: &gtk::SingleSelection,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let selection = selection.clone();

    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };

        let label = gtk::Label::new(None);
        label.add_css_class("debug-table-cell");
        label.add_css_class("memory-watch-cell");
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(pango::EllipsizeMode::Middle);
        enable_stable_text_selection(&label);
        let click = gtk::GestureClick::new();
        let weak_item = item.downgrade();
        let selection = selection.clone();

        click.connect_pressed(move |_, _, _, _| {
            if let Some(item) = weak_item.upgrade() {
                selection.set_selected(item.position());
            }
        });

        label.add_controller(click);
        item.set_child(Some(&label));
    });

    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };

        let (Some(label), Some(data)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) else {
            return;
        };

        clear_label_selection(&label);
        reset_semantic_css(&label);
        label.remove_css_class("memory-row-changed");
        let row = data.borrow::<MemoryRowData>();
        label.add_css_class(memory_kind_css(row.kind));

        if row.changed {
            label.add_css_class("memory-row-changed");
        }

        let width = usize::try_from(row.pointer_bits / 4)
            .unwrap_or(16)
            .clamp(8, 16);

        let text = match column {
            MemoryRowColumn::Address => format!("0x{:0width$x}", row.address),
            MemoryRowColumn::Offset => format!("+0x{:04x}", row.offset),
            MemoryRowColumn::Value => row.value.clone(),
            MemoryRowColumn::Decoded => row.decoded.clone(),
            MemoryRowColumn::Interpretation => row.interpretation.clone(),
        };

        label.set_text(&text);
        label.set_tooltip_text(Some(&text));
    });

    let view_column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    view_column.set_fixed_width(width);
    view_column.set_resizable(true);
    view_column.set_expand(expand);

    view_column
}

fn format_memory_rows(
    begin: u64,
    bytes: &[u8],
    format: MemoryWatchFormat,
    context: &MemoryRenderContext<'_>,
) -> Vec<MemoryRowData> {
    let chunk_size = match format {
        MemoryWatchFormat::Bytes => 16,
        MemoryWatchFormat::U16 => 2,
        MemoryWatchFormat::U32 => 4,
        MemoryWatchFormat::U64 => 8,
        MemoryWatchFormat::F32 => 4,
        MemoryWatchFormat::F64 => 8,
        MemoryWatchFormat::Pointers => usize::try_from(context.pointer_bits / 8)
            .unwrap_or(8)
            .clamp(4, 8),
    };

    bytes
        .chunks(chunk_size)
        .enumerate()
        .map(|(index, chunk)| {
            let offset = index * chunk_size;
            let address = begin.saturating_add(offset as u64);

            let changed = context.previous_begin == Some(begin)
                && context
                    .previous
                    .get(offset..offset.saturating_add(chunk.len()))
                    .is_some_and(|old| old != chunk);

            let pointer = (format == MemoryWatchFormat::Pointers)
                .then(|| decode_memory_integer(chunk, context.endian))
                .flatten();

            let value = format_memory_value(chunk, format, context.endian);
            let decoded = decode_memory_ascii(chunk);

            let kind = memory_region_for_address(context.regions, address)
                .map_or(MemoryKind::None, |region| region.kind);

            let interpretation = memory_interpretation(
                chunk,
                format,
                context.endian,
                pointer,
                changed,
                address,
                context.regions,
            );

            MemoryRowData {
                address,
                offset,
                value,
                decoded,
                interpretation,
                pointer,
                changed,
                kind,
                pointer_bits: context.pointer_bits,
            }
        })
        .collect()
}

fn format_memory_value(bytes: &[u8], format: MemoryWatchFormat, endian: TargetEndian) -> String {
    if format == MemoryWatchFormat::Bytes {
        let mut value = String::with_capacity(bytes.len() * 3);
        push_hex_bytes(&mut value, bytes);
        return value;
    }

    decode_memory_integer(bytes, endian).map_or_else(
        || {
            let mut value = String::with_capacity(bytes.len() * 3);
            push_hex_bytes(&mut value, bytes);

            value
        },
        |value| format!("0x{value:0width$x}", width = bytes.len() * 2),
    )
}

fn decode_memory_integer(bytes: &[u8], endian: TargetEndian) -> Option<u64> {
    match bytes.len() {
        2 => <[u8; 2]>::try_from(bytes).ok().map(|bytes| match endian {
            TargetEndian::Little => u16::from_le_bytes(bytes) as u64,
            TargetEndian::Big => u16::from_be_bytes(bytes) as u64,
        }),
        4 => <[u8; 4]>::try_from(bytes)
            .ok()
            .map(|bytes| endian.decode_u32(bytes) as u64),
        8 => <[u8; 8]>::try_from(bytes)
            .ok()
            .map(|bytes| endian.decode_u64(bytes)),
        _ => None,
    }
}

fn decode_memory_ascii(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| {
            if byte.is_ascii_graphic() || *byte == b' ' {
                char::from(*byte)
            } else {
                '·'
            }
        })
        .collect()
}

fn memory_interpretation(
    bytes: &[u8],
    format: MemoryWatchFormat,
    endian: TargetEndian,
    pointer: Option<u64>,
    changed: bool,
    address: u64,
    regions: &[MemoryRegion],
) -> String {
    let mut parts = Vec::new();

    match format {
        MemoryWatchFormat::Bytes => {}
        MemoryWatchFormat::U16 => {
            if let Some(value) = decode_memory_integer(bytes, endian) {
                parts.push(format!("u16 {value} · i16 {}", value as u16 as i16));
            }
        }
        MemoryWatchFormat::U32 => {
            if let Some(value) = decode_memory_integer(bytes, endian) {
                parts.push(format!("u32 {value} · i32 {}", value as u32 as i32));
            }
        }
        MemoryWatchFormat::U64 => {
            if let Some(value) = decode_memory_integer(bytes, endian) {
                parts.push(format!("u64 {value} · i64 {}", value as i64));
            }
        }
        MemoryWatchFormat::F32 => {
            if let Some(value) = decode_memory_integer(bytes, endian) {
                parts.push(format!("f32 {}", f32::from_bits(value as u32)));
            }
        }
        MemoryWatchFormat::F64 => {
            if let Some(value) = decode_memory_integer(bytes, endian) {
                parts.push(format!("f64 {}", f64::from_bits(value)));
            }
        }
        MemoryWatchFormat::Pointers => {
            if let Some(pointer) = pointer {
                let target = memory_region_for_address(regions, pointer)
                    .map(MemoryRegion::description)
                    .unwrap_or_else(|| {
                        if pointer == 0 {
                            String::from("null")
                        } else {
                            String::from("unmapped")
                        }
                    });

                parts.push(format!("→ {target}"));
            }
        }
    }

    if format == MemoryWatchFormat::Bytes
        && let Some(region) = memory_region_for_address(regions, address)
    {
        parts.push(region.description());
    }

    if changed {
        parts.push(String::from("changed"));
    }

    parts.join(" · ")
}

pub(super) fn push_hex_bytes(output: &mut String, bytes: &[u8]) {
    use std::fmt::Write as _;

    for (index, byte) in bytes.iter().enumerate() {
        if index != 0 {
            output.push(' ');
        }

        let _ = write!(output, "{byte:02x}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_conventional_hex_rows_and_typed_values() {
        let bytes = (0_u8..20).collect::<Vec<_>>();

        let rows = format_memory_rows(
            0x1000,
            &bytes,
            MemoryWatchFormat::Bytes,
            &MemoryRenderContext {
                pointer_bits: 64,
                endian: TargetEndian::Little,
                previous_begin: None,
                previous: &[],
                regions: &[],
            },
        );

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].address, 0x1000);
        assert_eq!(rows[0].value.split_whitespace().count(), 16);

        let typed = format_memory_rows(
            0x2000,
            &[0xff, 0xff, 0xff, 0xff],
            MemoryWatchFormat::U32,
            &MemoryRenderContext {
                pointer_bits: 64,
                endian: TargetEndian::Little,
                previous_begin: None,
                previous: &[],
                regions: &[],
            },
        );

        assert!(typed[0].interpretation.contains("u32 4294967295"));
        assert!(typed[0].interpretation.contains("i32 -1"));

        let float = format_memory_rows(
            0x3000,
            &1.5_f32.to_bits().to_le_bytes(),
            MemoryWatchFormat::F32,
            &MemoryRenderContext {
                pointer_bits: 64,
                endian: TargetEndian::Little,
                previous_begin: None,
                previous: &[],
                regions: &[],
            },
        );

        assert_eq!(float[0].interpretation, "f32 1.5");
    }

    #[test]
    fn reports_changes_only_for_the_same_address_range() {
        let current = [1, 2, 9, 4];
        let previous = [1, 2, 3, 4];

        let changed = format_memory_rows(
            0x1000,
            &current,
            MemoryWatchFormat::U16,
            &MemoryRenderContext {
                pointer_bits: 64,
                endian: TargetEndian::Little,
                previous_begin: Some(0x1000),
                previous: &previous,
                regions: &[],
            },
        );

        assert!(!changed[0].changed);
        assert!(changed[1].changed);

        let moved = format_memory_rows(
            0x2000,
            &current,
            MemoryWatchFormat::U16,
            &MemoryRenderContext {
                pointer_bits: 64,
                endian: TargetEndian::Little,
                previous_begin: Some(0x1000),
                previous: &previous,
                regions: &[],
            },
        );

        assert!(moved.iter().all(|row| !row.changed));
    }

    #[test]
    fn builds_bounded_page_expressions() {
        assert_eq!(
            memory_watch_request_expression_at("$rsp", -128),
            "($rsp)-0x80"
        );

        assert_eq!(
            memory_watch_request_expression_at("$rsp", 128),
            "($rsp)+0x80"
        );

        assert_eq!(memory_watch_request_expression_at("$rsp", 0), "$rsp");
    }

    #[test]
    fn filters_mappings_by_address_permissions_register_and_path() {
        let region = MemoryRegion {
            start: 0x7fff_1000,
            end: 0x7fff_2000,
            permissions: String::from("rw-p"),
            path: Some(String::from("/tmp/example.bin")),
            kind: MemoryKind::Writable,
            referenced_by: vec![String::from("$rsp")],
        };

        assert!(memory_region_matches_filter(&region, ""));
        assert!(memory_region_matches_filter(&region, "7fff1000"));
        assert!(memory_region_matches_filter(&region, "rw-p $rsp"));
        assert!(memory_region_matches_filter(&region, "example.bin"));
        assert!(!memory_region_matches_filter(&region, "r-xp"));
    }
}
