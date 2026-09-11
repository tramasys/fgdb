use super::*;
use crate::ui::layout::{Pane, Persistence};
use std::time::{Duration, Instant};

fn settle() {
    glib::MainContext::default().block_on(glib::timeout_future(Duration::from_millis(40)));
}

fn wait(ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);

    while !ready() && Instant::now() < deadline {
        settle();
    }

    assert!(ready(), "TLS layout did not settle");
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn tls_split_resizes_and_restores_after_page_and_metadata_changes() {
    gtk::init().unwrap();
    Theme::graphite().install();

    let application = gtk::Application::builder()
        .application_id("dev.fgdb.TlsLayoutTest")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    application.register(None::<&gio::Cancellable>).unwrap();
    let temporary = glib::mkdtemp(std::env::temp_dir().join("fgdb-tls-layout-XXXXXX")).unwrap();
    let path = temporary.join("layout.conf");

    let build = || {
        let columns = ColumnLayouts::default();

        let kernel = build_kernel_view(&KernelViewBindings {
            columns: &columns,
            refresh_handler: &Rc::new(RefCell::new(None)),
            remembered_disclosures: &HashMap::new(),
            section_handler: &Rc::new(RefCell::new(None)),
        });

        let split = kernel
            .pages
            .child_by_name("tls")
            .unwrap()
            .downcast::<gtk::Paned>()
            .expect("runtime TLS and ELF metadata must have a draggable divider");

        let runtime = KernelTlsRuntime {
            architecture: TargetArchitecture::X86_64,
            endian: Some(TargetEndian::Little),
            pointer_bits: 64,
            register: Some(String::from("fs_base")),
            base: Some(0x7fff_0000),
            bytes: vec![0; 80],
            ..KernelTlsRuntime::default()
        };

        replace_boxed_store_if_changed(&kernel.tls_runtime_store, tls_runtime_rows(&runtime));
        let pages = kernel.pages;
        let metadata = kernel.tls_metadata;

        let window = gtk::ApplicationWindow::builder()
            .application(&application)
            .default_width(1000)
            .default_height(700)
            .child(&kernel.root)
            .build();

        let persistence = Persistence::install_at(
            &window,
            vec![Pane::with_default_fraction("kernel_tls", &split, 0.5)],
            path.clone(),
            &columns,
        );

        let ready = Rc::new(Cell::new(false));
        let observed = Rc::clone(&ready);
        persistence.on_ready(move || observed.set(true));
        window.present();
        wait(|| ready.get());
        let clock = window.frame_clock().unwrap();
        let mapped_pages = pages.clone();

        // Map between layout frames so an idle callback cannot assume that
        // the newly visible pane already has an allocation.
        let handler =
            clock.connect_after_paint(move |_| mapped_pages.set_visible_child_name("tls"));

        clock.request_phase(gtk::gdk::FrameClockPhase::AFTER_PAINT);
        wait(|| split.is_mapped() && split.height() > 600);
        clock.disconnect(handler);
        settle();

        (window, pages, split, metadata, persistence)
    };

    let (window, pages, split, metadata, persistence) = build();
    assert!(split.is_wide_handle());

    assert!(
        (split.position() - split.height() / 2).abs() <= 2,
        "initial position {} for height {} and range {}..{}",
        split.position(),
        split.height(),
        split.min_position(),
        split.max_position()
    );

    let runtime_page = split.start_child().unwrap();

    let runtime = runtime_page
        .last_child()
        .and_downcast::<gtk::ScrolledWindow>()
        .unwrap();

    assert_eq!(runtime.max_content_height(), -1);
    split.set_position(500);
    wait(|| runtime.height() > 400 && metadata.height() > 100);
    let expanded_height = runtime.height();
    metadata.set_visible_child_name("content");

    let metadata_pages = metadata
        .child_by_name("content")
        .unwrap()
        .last_child()
        .and_downcast::<gtk::Stack>()
        .unwrap();

    for name in ["symbols", "modules"] {
        metadata_pages.set_visible_child_name(name);
        settle();
        assert_eq!(split.position(), 500);
        assert_eq!(runtime.height(), expanded_height);
    }

    let full_height = split.height();
    window.set_default_size(1000, 500);
    wait(|| split.height() < full_height - 100);
    assert!(runtime.height() > 0 && metadata.height() > 0);
    window.set_default_size(1000, 700);
    wait(|| split.height() == full_height);
    split.set_position(180);
    wait(|| runtime.height() < 180 && metadata.height() > 400);
    metadata.set_visible_child_name("empty");
    settle();
    assert_eq!(split.position(), 180);
    pages.set_visible_child_name("overview");
    wait(|| !split.is_mapped());
    pages.set_visible_child_name("tls");
    wait(|| split.is_mapped());
    settle();
    assert_eq!(split.position(), 180);

    pages.set_visible_child_name("overview");
    wait(|| !split.is_mapped());
    pages.set_visible_child_name("tls");
    split.set_position(240);
    settle();

    assert_eq!(
        split.position(),
        240,
        "a pending restore must not override an explicit move"
    );

    split.set_position(180);
    persistence.finish();
    wait(|| persistence.final_save_done());

    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("kernel_tls=180,")
    );

    window.close();
    drop((window, pages, split, metadata, persistence));
    let (window, _, split, _, persistence) = build();
    assert_eq!(split.position(), 180);
    persistence.finish();
    wait(|| persistence.final_save_done());
    window.close();
    std::fs::remove_file(path.with_extension("lock")).unwrap();
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(temporary).unwrap();
}
