//! Opt-in GTK refresh measurements, including the first rendered frame.

use super::*;
use crate::ui::{ColumnLayouts, InstructionRowData, TableId, views};
use gtk::{gio, glib};
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

fn rows(start: u64, pc: u64) -> Vec<InstructionRowData> {
    (start..start + 440)
        .map(|index| InstructionRowData {
            instruction: crate::debugger::Instruction {
                address: format!("0x{:016x}", 0x1000 + index * 4),
                function: format!("rust_fixture::checkpoint_{}", start / 1000),
                offset: (index - start).to_string(),
                opcodes: Some(String::from("48 8b 84 24 b0 08 00 00")),
                text: format!("mov rax,QWORD PTR [rsp+0x{:x}]", index * 8),
                source: None,
            },
            current: index == pc,
            pointer_bits: 64,
            source_text: None,
        })
        .collect()
}

fn update(store: &gio::ListStore, rows: Vec<InstructionRowData>) {
    super::super::replace_snapshot_store(store, rows);
}

#[test]
#[ignore = "GTK timing, requires a display and an otherwise idle system"]
fn benchmark_instruction_refresh() {
    gtk::init().unwrap();
    crate::theme::Theme::graphite().install();
    measure_instruction_refresh(ScrollMode::Native);
}

#[test]
#[ignore = "GTK timing, requires a display and an otherwise idle system"]
fn benchmark_centered_instruction_refresh() {
    gtk::init().unwrap();
    crate::theme::Theme::graphite().install();

    for mode in [ScrollMode::LegacyCentered, ScrollMode::Centered] {
        println!("BENCH centering: {mode:?}");
        measure_instruction_refresh(mode);
    }
}

#[derive(Clone, Copy, Debug)]
enum ScrollMode {
    Native,
    LegacyCentered,
    Centered,
}

fn measure_instruction_refresh(mode: ScrollMode) {
    let (view, store, selection, _) =
        views::build_instruction_view(&ColumnLayouts::default().table(TableId::Instructions));

    let setups = Rc::new(Cell::new(0));
    let binds = Rc::new(Cell::new(0));

    for column in view.columns().iter::<gtk::ColumnViewColumn>() {
        let factory = column
            .unwrap()
            .factory()
            .and_downcast::<gtk::SignalListItemFactory>()
            .unwrap();
        let setups = Rc::clone(&setups);
        factory.connect_setup(move |_, _| setups.set(setups.get() + 1));
        let binds = Rc::clone(&binds);
        factory.connect_bind(move |_, _| binds.set(binds.get() + 1));
    }

    update(&store, rows(0, 0));
    let scroll = gtk::ScrolledWindow::builder()
        .child(&view)
        .overlay_scrolling(false)
        .build();
    let centering = Rc::new(CenteredScroll::default());
    let window = gtk::Window::builder()
        .default_width(1200)
        .default_height(650)
        .child(&scroll)
        .build();
    window.present();
    let main = glib::MainContext::default();
    main.block_on(glib::timeout_future(Duration::from_millis(200)));
    let painted = Rc::new(Cell::new(None::<Instant>));
    let painted_signal = Rc::clone(&painted);
    let clock = view.frame_clock().unwrap();
    let handler = clock.connect_after_paint(move |_| painted_signal.set(Some(Instant::now())));

    for round in 0..4 {
        for (name, start, pc) in [
            ("function", 1000 + round * 1000, 1200 + round * 1000),
            ("step", 1000 + round * 1000, 1201 + round * 1000),
        ] {
            let rows = rows(start, pc);
            setups.set(0);
            binds.set(0);
            painted.set(None);
            let started = Instant::now();
            update(&store, rows);
            let updated = Instant::now();
            selection.set_selected((pc - start) as u32);
            let selected = Instant::now();
            let position = (pc - start) as u32;

            if matches!(mode, ScrollMode::Centered) {
                centering.request(&view, &scroll, position);
            } else {
                view.scroll_to(position, None, gtk::ListScrollFlags::FOCUS, None);

                if matches!(mode, ScrollMode::LegacyCentered) {
                    // The former production path, retained only for comparison.
                    center_scroll_adjustment(&scroll, position, store.n_items());
                    let scroll = scroll.clone();
                    let count = store.n_items();

                    glib::timeout_add_local_once(Duration::from_millis(16), move || {
                        center_scroll_adjustment(&scroll, position, count);
                    });
                }
            }
            let sync = started.elapsed();
            clock.request_phase(gtk::gdk::FrameClockPhase::AFTER_PAINT);

            while painted.get().is_none() {
                assert!(started.elapsed() < Duration::from_secs(5));
                main.block_on(glib::timeout_future(Duration::from_millis(1)));
            }

            println!(
                "BENCH instruction {name}: sync={sync:?} update={:?} selection={:?} scroll={:?} frame={:?} setups={} binds={}",
                updated.duration_since(started),
                selected.duration_since(updated),
                sync.saturating_sub(selected.duration_since(started)),
                painted.get().unwrap().duration_since(started),
                setups.get(),
                binds.get()
            );
            main.block_on(glib::timeout_future(Duration::from_millis(40)));
        }
    }

    clock.disconnect(handler);
    window.close();
}

#[test]
#[ignore = "GTK timing, requires a display and an otherwise idle system"]
fn benchmark_stack_refresh() {
    gtk::init().unwrap();
    crate::theme::Theme::graphite().install();
    let (view, store, _) = views::build_stack_view(&ColumnLayouts::default().table(TableId::Stack));
    let scroll = gtk::ScrolledWindow::builder()
        .child(&view)
        .overlay_scrolling(false)
        .build();
    let window = gtk::Window::builder()
        .default_width(1200)
        .default_height(650)
        .child(&scroll)
        .build();
    window.present();
    let main = glib::MainContext::default();
    main.block_on(glib::timeout_future(Duration::from_millis(100)));

    for round in 0..10 {
        if round == 5 {
            window.set_visible(false);
        }

        let visibility = if round < 5 { "" } else { " hidden" };
        let memory = crate::debugger::MemoryBlock {
            begin: 0x7000 + round * 8,
            bytes: vec![0x41; 128 * 8],
        };
        let entries = crate::debugger::context::build_stack_entries(
            &memory,
            8,
            crate::debugger::TargetEndian::Little,
            crate::debugger::TargetArchitecture::X86_64,
            &[],
            &[],
            &[],
        );
        let started = Instant::now();
        super::super::replace_snapshot_store(&store, entries.clone());
        println!("BENCH stack{visibility} base: {:?}", started.elapsed());
        main.block_on(glib::timeout_future(Duration::from_millis(50)));
        let started = Instant::now();

        for (index, mut entry) in entries.into_iter().enumerate().take(16) {
            entry.pointer_chain.push("0x1234 <target>".into());
            store
                .item(index as u32)
                .and_downcast::<super::super::SnapshotRow>()
                .unwrap()
                .replace(entry);
        }

        println!("BENCH stack{visibility} details: {:?}", started.elapsed());
        main.block_on(glib::timeout_future(Duration::from_millis(50)));
    }

    window.close();
}

#[test]
#[ignore = "GTK timing, requires a display and an otherwise idle system"]
fn benchmark_register_refresh() {
    use crate::debugger::{Register, TargetArchitecture, TargetEndian};
    use crate::ui::{
        RegisterGroupKind, RegisterGroupView, RegisterGroupWidget, RegisterRowData, VectorDisplay,
        formatting,
    };

    gtk::init().unwrap();
    crate::theme::Theme::graphite().install();
    let (view, store) = views::build_register_group_table(
        &ColumnLayouts::default().table(TableId::GeneralRegisters),
    );
    let group = RegisterGroupView {
        kind: RegisterGroupKind::General,
        store,
        view: RegisterGroupWidget::Table(view.clone()),
        panel: gtk::Box::new(gtk::Orientation::Vertical, 0),
        vector_controls: None,
    };
    let scroll = gtk::ScrolledWindow::builder().child(&view).build();
    let window = gtk::Window::builder()
        .default_width(1200)
        .default_height(650)
        .child(&scroll)
        .build();
    window.present();
    let main = glib::MainContext::default();
    main.block_on(glib::timeout_future(Duration::from_millis(100)));

    for round in 0..5 {
        let rows = [
            "rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rsp", "rbp", "r8", "r9", "r10", "r11",
            "r12", "r13", "r14", "r15", "rip",
        ]
        .map(|name| RegisterRowData {
            register: Register {
                name: name.into(),
                value: format!("0x{:x}", 0x1000 + round * 8),
                pointer_chain: vec![format!("0x{:x} <checkpoint>", 0x1000 + round * 8)],
            },
            changed: true,
            ring: None,
            architecture: TargetArchitecture::X86_64,
            endian: Some(TargetEndian::Little),
            pointer_bits: 64,
            vector_display: VectorDisplay::default(),
        });
        let started = Instant::now();
        formatting::populate_register_group(&group, rows, false);
        println!("BENCH register refresh: {:?}", started.elapsed());
        main.block_on(glib::timeout_future(Duration::from_millis(50)));
    }

    window.close();
}
