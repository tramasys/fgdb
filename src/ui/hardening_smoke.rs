use super::*;

#[track_caller]
fn iterate_until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !done() {
        assert!(
            Instant::now() < deadline,
            "GTK background work did not complete"
        );
        glib::MainContext::default().iteration(false);
        std::thread::sleep(Duration::from_millis(1));
    }
}

// Run separately under xvfb-run with --ignored --test-threads=1. Keeping this
// integration check opt-in avoids requiring a display for the unit suite.
#[test]
#[ignore = "requires a GTK display and an isolated XDG_CONFIG_HOME"]
fn gtk_source_loading_filtering_and_forced_metadata_batches() {
    let config = PathBuf::from(
        std::env::var_os("XDG_CONFIG_HOME")
            .expect("Set XDG_CONFIG_HOME to a temporary directory before running this test"),
    );
    assert!(
        config.starts_with(std::env::temp_dir()) && config != std::env::temp_dir(),
        "The GTK check must not overwrite a user's layout"
    );
    gtk::init().unwrap();
    gio::resources_register_include!("fgdb.gresource").unwrap();
    let directory = std::env::temp_dir().join(format!("fgdb-ui-hardening-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let first = directory.join("first.rs");
    let second = directory.join("second.cpp");
    std::fs::write(&first, "fn main() {\n    let value = 42;\n}\n").unwrap();
    std::fs::write(&second, "int main() {\n    return 0;\n}\n").unwrap();
    let app = gtk::Application::builder()
        .application_id("dev.fgdb.HardeningTest")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    let ui = Rc::new(Ui::build(
        &app,
        &LaunchConfig::for_ui_test(directory.clone()),
        &Theme::graphite(),
    ));
    ui.connect_source_loading();
    ui.connect_source_navigation();
    ui.connect_stop_point_search();
    ui.window.present();

    ui.open_source_batch(
        Rc::new(RefCell::new([first.clone(), second.clone()].into())),
        Rc::new(RefCell::new((0, Vec::new()))),
    );
    iterate_until(|| {
        ui.source_documents.borrow().len() == 2 && !ui.source_back_history.borrow().is_empty()
    });
    assert_eq!(ui.source_back_history.borrow().last().unwrap().path, first);
    let history = ui.source_back_history.borrow().clone();
    ui.navigate_to_source(&directory.join("missing.rs"), 1, true);
    iterate_until(|| ui.status_label.text() == "Source unavailable");
    assert_eq!(*ui.source_back_history.borrow(), history);
    ui.source_navigation.back.emit_clicked();
    assert!(ui.source_back_history.borrow().is_empty());
    assert_eq!(
        ui.source_forward_history.borrow().last().unwrap().path,
        second
    );
    ui.source_navigation.forward.emit_clicked();
    assert!(ui.source_forward_history.borrow().is_empty());

    let record = crate::debugger::parse_record(r#"1^done,BreakpointTable={body=[bkpt={number="1",type="breakpoint",enabled="y",addr="0x401000",original-location="first.rs:1"}]}"#).unwrap();
    let template = crate::debugger::breakpoints(&record).pop().unwrap();
    let count = crate::performance::STOP_POINT_WIDGET_BUDGET + 20;
    let breakpoints = (0..count)
        .map(|index| {
            let mut breakpoint = template.clone();
            breakpoint.number = (index + 1).to_string();
            breakpoint.function = Some(if index + 1 == count {
                String::from("tail_function")
            } else {
                format!("function_{index}")
            });
            breakpoint
        })
        .collect();
    ui.show_breakpoints(breakpoints);
    assert_eq!(
        ui.stop_point_filter_rows.borrow().len(),
        crate::performance::STOP_POINT_WIDGET_BUDGET
    );
    ui.stop_point_filter.search.set_text("tail_function");
    iterate_until(|| ui.stop_point_filter_rows.borrow().len() == 1);
    assert_eq!(
        ui.stop_point_filter_rows.borrow()[0].number,
        count.to_string()
    );
    ui.stop_point_filter
        .search
        .set_text("no_matching_stop_point");
    iterate_until(|| ui.stop_point_filter.empty.is_visible());

    let mut multi = vec![template.clone()];
    for index in 0..count {
        let mut child = template.clone();
        child.number = format!("1.{}", index + 1);
        child.parent_number = Some(String::from("1"));
        child.function = Some(if index + 1 == count {
            String::from("child_tail")
        } else {
            format!("child_{index}")
        });
        multi.push(child);
    }
    ui.show_breakpoints(multi);
    ui.stop_point_filter.search.set_text("child_tail");
    iterate_until(|| !ui.stop_point_filter.empty.is_visible());
    let parent = ui.breakpoints_list.first_child().unwrap();
    let child = parent.next_sibling().unwrap();
    assert!(child.has_css_class("breakpoint-location-row"));
    assert_eq!(
        child.next_sibling().unwrap(),
        ui.stop_point_filter.empty.clone().upcast::<gtk::Widget>()
    );

    // Force refresh of cached metadata, including entries beyond the first
    // batch. An uncached-only continuation would leave these sentinel values.
    for index in 0..crate::performance::MODULE_METADATA_FILE_BUDGET + 3 {
        let path = directory.join(format!("module-{index}"));
        std::os::unix::fs::symlink(&first, &path).unwrap();
        let file = std::fs::metadata(&path).unwrap();
        ui.latest_modules.borrow_mut().push(SharedLibrary {
            target_name: path.display().to_string(),
            host_name: None,
            symbols_loaded: true,
            from: None,
            to: None,
        });
        ui.module_debug_metadata.borrow_mut().insert(
            path.clone(),
            ModuleDebugMetadata {
                path,
                build_id: None,
                debuglink: None,
                debuglink_crc: None,
                separate_debug_file: None,
                embedded_debug_info: true,
                suggestion: Some(String::from("not refreshed")),
                error: None,
                file_size: Some(file.len()),
                modified: file.modified().ok(),
            },
        );
    }
    ui.refresh_module_debug_metadata(true);
    iterate_until(|| !ui.module_debug_worker_active.load(Ordering::Relaxed));
    assert!(
        ui.module_debug_metadata
            .borrow()
            .values()
            .all(|metadata| metadata.suggestion.is_none())
    );
    ui.window.destroy();
    drop(ui);
    std::fs::remove_dir_all(directory).unwrap();
}
