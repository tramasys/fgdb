use super::*;
use crate::theme::Theme;
use gtk::glib;
use std::{cell::Cell, rc::Rc, time::Duration};

fn settle() {
    glib::MainContext::default().block_on(glib::timeout_future(Duration::from_millis(50)));
}

fn columns(view: &gtk::ColumnView) -> Vec<gtk::ColumnViewColumn> {
    view.columns().iter().map(Result::unwrap).collect()
}

fn assert_policy(view: &gtk::ColumnView) {
    let columns = columns(view);
    let last = columns.iter().rposition(|column| column.is_visible());

    for (index, column) in columns.iter().enumerate() {
        assert!(column.is_resizable());
        assert_eq!(
            column.expands(),
            Some(index) == last,
            "{:?}",
            column.title()
        );
    }
}

fn header_bounds(view: &gtk::ColumnView) -> Vec<gtk::graphene::Rect> {
    let header = view.first_child().unwrap();
    assert_eq!(header.css_name(), "header");
    let mut child = header.first_child();
    let mut bounds = Vec::new();

    while let Some(current) = child {
        if current.is_visible() {
            bounds.push(current.compute_bounds(view).unwrap());
        }

        child = current.next_sibling();
    }

    bounds
}

fn assert_dividers_move_both_ways(view: &gtk::ColumnView) {
    assert_policy(view);
    let visible = columns(view)
        .into_iter()
        .filter(|column| column.is_visible())
        .collect::<Vec<_>>();

    for (index, column) in visible
        .iter()
        .enumerate()
        .take(visible.len().saturating_sub(1))
    {
        let before = header_bounds(view);
        let width = column.fixed_width();

        // Header drags change the fixed width of the preceding column.
        for delta in [-24, 40, 0] {
            column.set_fixed_width(width + delta);
            settle();
            let after = header_bounds(view);
            let shift = after[index + 1].x() - before[index + 1].x();
            assert!(
                (shift - delta as f32).abs() <= 1.0,
                "{:?}: {shift} != {delta}",
                column.title()
            );
            let trailing_growth = after.last().unwrap().width() - before.last().unwrap().width();
            assert!((trailing_growth + delta as f32).abs() <= 1.0);
        }
    }
}

fn present(view: &gtk::ColumnView, width: i32) -> (gtk::Window, gtk::ScrolledWindow) {
    view.add_css_class("debug-table");
    let scroll = gtk::ScrolledWindow::builder()
        .child(view)
        .overlay_scrolling(false)
        .build();

    let window = gtk::Window::builder()
        .default_width(width)
        .default_height(240)
        .child(&scroll)
        .build();

    window.present();
    settle();

    (window, scroll)
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn column_layout_tracks_visibility_order_and_overflow_without_retaining_views() {
    gtk::init().unwrap();
    Theme::graphite().install();
    let rows = gtk::StringList::new(&["row"]);
    let view = column_view(gtk::NoSelection::new(Some(rows.clone())));
    let changes = Rc::new(Cell::new(0));
    let captured = Rc::clone(&changes);
    rows.connect_items_changed(move |_, _, _, _| captured.set(captured.get() + 1));

    for (title, width) in [("INDEX", 140), ("VALUE", 260), ("TYPE", 200)] {
        let factory = gtk::SignalListItemFactory::new();

        factory.connect_setup(|_, object| {
            let item = object.downcast_ref::<gtk::ListItem>().unwrap();
            item.set_child(Some(&gtk::Label::new(Some("value"))));
        });

        view.append_column(&table_column(title, width, factory));
    }

    let original = columns(&view);
    let widths = original
        .iter()
        .map(|column| column.fixed_width())
        .collect::<Vec<_>>();
    let (window, scroll) = present(&view, 1100);
    assert_dividers_move_both_ways(&view);
    original[2].set_visible(false);
    settle();
    assert_dividers_move_both_ways(&view);
    original[2].set_visible(true);
    view.insert_column(0, &original[2]);
    settle();
    assert_dividers_move_both_ways(&view);
    view.remove_column(&original[1]);
    settle();
    assert_dividers_move_both_ways(&view);
    view.append_column(&original[1]);
    settle();
    assert_dividers_move_both_ways(&view);

    for column in &original {
        column.set_visible(false);
        assert_policy(&view);
    }

    for column in &original {
        column.set_visible(true);
        assert_policy(&view);
    }

    assert_eq!(
        original
            .iter()
            .map(|column| column.fixed_width())
            .collect::<Vec<_>>(),
        widths
    );
    original[0].set_fixed_width(1600);
    settle();
    assert!(scroll.hadjustment().upper() > scroll.hadjustment().page_size());
    original[0].set_fixed_width(widths[0]);
    settle();
    assert!(scroll.hadjustment().upper() <= scroll.hadjustment().page_size());
    assert_eq!(changes.get(), 0);
    original[0].set_fixed_width(widths[0] + 40);
    rows.append("another row");
    settle();
    assert_eq!(original[0].fixed_width(), widths[0] + 40);
    assert_policy(&view);
    let weak = view.downgrade();
    let retained_columns = view.columns();
    scroll.set_child(gtk::Widget::NONE);
    window.close();
    drop(window);
    drop(scroll);
    drop(view);
    settle();
    assert!(weak.upgrade().is_none());

    for column in original {
        column.set_visible(false);
        assert!(column.column_view().is_none());
    }

    drop(retained_columns);
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn debugger_table_dividers_resize_with_optional_instruction_columns() {
    use crate::config::settings::IntegerDisplay;
    use crate::ui::{VariableViewerRegistry, variable_presentation::VariablePresentation, views};
    use std::cell::RefCell;

    gtk::init().unwrap();
    Theme::graphite().install();
    let pointer_bits = Rc::new(Cell::new(64));
    let presentation =
        VariablePresentation::new(IntegerDisplay::Automatic, Rc::clone(&pointer_bits));
    let locations =
        crate::ui::variables::locations::Locations::new(false, Rc::clone(&pointer_bits));

    let (locals, _, _) = views::build_locals_view(
        &crate::ui::ColumnLayouts::default().table(crate::ui::TableId::Locals),
        &Rc::new(RefCell::new(None)),
        &Rc::new(RefCell::new(None)),
        &Rc::new(VariableViewerRegistry::with_builtins()),
        &presentation,
        &locations,
        None,
    );

    let (instructions, _, _, optional) = views::build_instruction_view(
        &crate::ui::ColumnLayouts::default().table(crate::ui::TableId::Instructions),
    );

    let (stack, _, _) = views::build_stack_view(
        &crate::ui::ColumnLayouts::default().table(crate::ui::TableId::Stack),
    );

    let (registers, _) = views::build_register_group_table(
        &crate::ui::ColumnLayouts::default().table(crate::ui::TableId::GeneralRegisters),
    );

    let search = gtk::SearchEntry::new();
    let (mappings, _) = views::build_memory_region_view(
        &crate::ui::ColumnLayouts::default().table(crate::ui::TableId::MemoryMappings),
        &pointer_bits,
        &search,
    );

    for view in [&locals, &instructions, &stack, &registers, &mappings] {
        let (window, _) = present(view, 1800);
        assert_dividers_move_both_ways(view);
        let last = columns(view)
            .into_iter()
            .rev()
            .find(|column| column.is_visible())
            .unwrap();
        view.insert_column(0, &last);
        settle();
        assert_dividers_move_both_ways(view);
        window.set_child(gtk::Widget::NONE);
        window.close();
    }

    let (window, _) = present(&locals, 1800);

    for visible in [true, false] {
        locations.set_enabled(visible);
        settle();
        assert_dividers_move_both_ways(&locals);
    }

    window.set_child(gtk::Widget::NONE);
    window.close();
    let (window, _) = present(&instructions, 1800);

    for visibility in 0..8 {
        optional.bytes.set_visible(visibility & 1 != 0);
        optional.symbols.set_visible(visibility & 2 != 0);
        optional.source.set_visible(visibility & 4 != 0);
        settle();
        assert_dividers_move_both_ways(&instructions);
    }

    window.close();
}
