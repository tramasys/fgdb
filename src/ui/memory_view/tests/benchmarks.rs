use super::*;

#[test]
#[ignore = "GTK timing, requires a display and an otherwise idle system"]
fn benchmark_memory_refresh() {
    gtk::init().unwrap();
    Theme::graphite().install();
    let (view, store, _) =
        build_memory_watch_table(&ColumnLayouts::default().table(TableId::MemoryBytes));
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

    for round in 0..5 {
        let bytes = vec![0x41 + round; 2048];
        let context = MemoryRenderContext {
            pointer_bits: 64,
            endian: TargetEndian::Little,
            previous_begin: None,
            previous: &[],
            regions: &[],
        };
        let rows = format_memory_rows(0x1000, &bytes, MemoryWatchFormat::Bytes, &context);
        let started = Instant::now();
        components::replace_snapshot_store(&store, rows);
        println!("BENCH memory refresh: {:?}", started.elapsed());
        main.block_on(glib::timeout_future(Duration::from_millis(50)));
    }

    window.close();
}
