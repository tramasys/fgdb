use super::*;
use crate::debugger::{Register, TargetArchitecture, TargetEndian};
use crate::ui::{
    ColumnLayouts, RegisterColumn, RegisterRowData, TableId, VectorDisplay, components,
    formatting::{register_primary_value, with_register_row},
    views,
};
use std::cell::RefCell;

fn row(value: &str) -> RegisterRowData {
    RegisterRowData {
        register: Register {
            name: "rax".into(),
            value: value.into(),
            pointer_chain: Vec::new(),
        },
        changed: value != "0x0",
        ring: None,
        architecture: TargetArchitecture::X86_64,
        endian: Some(TargetEndian::Little),
        pointer_bits: 64,
        vector_display: VectorDisplay::default(),
    }
}

#[test]
fn register_edit_payload_reads_table_and_simd_representations() {
    let value = row("0x1234");
    for object in [
        components::SnapshotRow::new(value.clone()).upcast::<glib::Object>(),
        glib::BoxedAnyObject::new(value.clone()).upcast(),
    ] {
        assert_eq!(
            with_register_row(&object, |row| row.register.clone()),
            Some(value.register.clone())
        );
    }
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn register_snapshots_update_text_styles_and_release_cells() {
    gtk::init().unwrap();
    Theme::graphite().install();
    let (view, store) = views::build_register_group_table(
        &ColumnLayouts::default().table(TableId::GeneralRegisters),
    );
    let items = Rc::new(RefCell::new(Vec::<glib::WeakRef<gtk::ListItem>>::new()));

    for column in columns(&view) {
        let items = Rc::clone(&items);
        column
            .factory()
            .and_downcast::<gtk::SignalListItemFactory>()
            .unwrap()
            .connect_setup(move |_, item| {
                items
                    .borrow_mut()
                    .push(item.downcast_ref::<gtk::ListItem>().unwrap().downgrade());
            });
    }

    components::replace_snapshot_store(&store, [row("0x0")]);
    let (window, scroll) = present(&view, 1000);
    let original = store.item(0).unwrap();
    let weak_view = view.downgrade();

    for value in ["0x1234", "0x0", "0x5678"] {
        let row = row(value);
        components::replace_snapshot_store(&store, [row.clone()]);
        assert_eq!(store.item(0).as_ref(), Some(&original));
        settle();
        let mut checked = 0;

        for item in items.borrow().iter().filter_map(glib::WeakRef::upgrade) {
            let Some(label) = item.child().and_downcast::<gtk::Label>() else {
                continue;
            };
            if item.item().is_none() {
                continue;
            }
            if label.has_css_class(views::register_column_css(RegisterColumn::Name)) {
                assert_eq!(label.text(), "$rax");
                assert_eq!(label.has_css_class("modified-register"), row.changed);
                checked += 1;
            } else if label.has_css_class(views::register_column_css(RegisterColumn::Value)) {
                assert_eq!(
                    label.text(),
                    register_primary_value(&row.register, row.architecture)
                );
                assert_eq!(label.has_css_class("register-zero"), value == "0x0");
                label.select_region(0, -1);
                label.emit_by_name::<()>("copy-clipboard", &[]);
                assert_eq!(
                    glib::MainContext::default()
                        .block_on(label.clipboard().read_text_future())
                        .unwrap()
                        .as_deref(),
                    Some(label.text().as_str())
                );
                checked += 1;
            }
        }

        assert_eq!(checked, 2);
    }

    // A retained old payload must not retain its former cell or view.
    scroll.set_child(gtk::Widget::NONE);
    window.close();
    drop(view);
    drop(scroll);
    drop(window);
    settle();
    assert!(weak_view.upgrade().is_none());
    assert!(items.borrow().iter().all(|item| item.upgrade().is_none()));
}
