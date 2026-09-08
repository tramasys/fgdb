//! Panel identity and presentation are independent of notebook order and window ownership.

use super::*;

pub(super) mod console;
mod dialogs;
mod hosting;
mod menus;
mod navigation;
mod presentation;
mod registry;
mod responsive;
pub(super) use hosting::Hosts;
pub(super) use navigation::build_compact_navigation;
pub(super) use presentation::connect_presentation;
pub(crate) use registry::PanelId;
pub(super) use responsive::ResponsiveBox;

#[derive(Default)]
pub(super) struct Panels {
    entries: [Option<Panel>; PanelId::ALL.len()],
    dialogs: Rc<dialogs::Dialogs>,
}

struct Panel {
    root: gtk::Widget,
    slot: gtk::Box,
    collapse_home: bool,
}

impl Panels {
    pub(super) fn track_dialog(&self, id: PanelId, window: &gtk::Window) {
        self.dialogs.register(window, Some(id));
    }

    pub(super) fn containing(&self, widget: &gtk::Widget) -> Option<PanelId> {
        let mut current = Some(widget.clone());

        while let Some(widget) = current {
            if let Some(id) = self.id(&widget) {
                return Some(id);
            }

            current = widget.parent();
        }

        None
    }

    pub(super) fn register(&mut self, id: PanelId, root: &impl IsA<gtk::Widget>) -> gtk::Box {
        self.register_home(id, root, false)
    }

    pub(super) fn register_collapsible(
        &mut self,
        id: PanelId,
        root: &impl IsA<gtk::Widget>,
    ) -> gtk::Box {
        self.register_home(id, root, true)
    }

    fn register_home(
        &mut self,
        id: PanelId,
        root: &impl IsA<gtk::Widget>,
        collapse_home: bool,
    ) -> gtk::Box {
        assert!(
            self.entries[id as usize].is_none(),
            "duplicate panel registration"
        );

        assert!(
            root.parent().is_none(),
            "a panel root must be unparented at registration"
        );
        assert!(
            self.id(root.as_ref()).is_none(),
            "a widget cannot have two panel identities"
        );

        let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        slot.set_hexpand(true);
        slot.set_vexpand(true);
        slot.append(root);
        slot.set_visible(root.get_visible());
        let weak_slot = slot.downgrade();

        root.connect_visible_notify(move |root| {
            if let Some(slot) = weak_slot.upgrade() {
                // Read the panel's own flag, not the visibility of its hidden home.
                sync_home_visibility(&slot, root.as_ref(), collapse_home);
            }
        });

        self.entries[id as usize] = Some(Panel {
            root: root.as_ref().clone(),
            slot: slot.clone(),
            collapse_home,
        });

        slot
    }

    pub(super) fn finish(self) -> Rc<Self> {
        // Construction is the only mutable phase. Runtime hosting receives a
        // complete immutable registry and cannot add a partially wired panel.
        assert!(
            self.entries.iter().all(Option::is_some),
            "missing workspace panel"
        );
        Rc::new(self)
    }

    pub(super) fn append(
        &mut self,
        notebook: &gtk::Notebook,
        id: PanelId,
        root: &impl IsA<gtk::Widget>,
    ) {
        let slot = self.register(id, root);
        notebook.append_page(&slot, None::<&gtk::Widget>);
        notebook.set_tab_label_text(&slot, id.title());
    }

    pub(super) fn root(&self, id: PanelId) -> Option<&gtk::Widget> {
        self.entries[id as usize].as_ref().map(|panel| &panel.root)
    }

    pub(super) fn slot(&self, id: PanelId) -> Option<&gtk::Box> {
        self.entries[id as usize].as_ref().map(|panel| &panel.slot)
    }

    fn sync_home(&self, id: PanelId) {
        if let Some(panel) = &self.entries[id as usize] {
            sync_home_visibility(&panel.slot, &panel.root, panel.collapse_home);
        }
    }

    fn collapses_home(&self, id: PanelId) -> bool {
        self.entries[id as usize]
            .as_ref()
            .is_some_and(|panel| panel.collapse_home)
    }

    fn home_container(&self, id: PanelId) -> Option<PanelId> {
        let mut parent = self.slot(id)?.parent();

        while let Some(widget) = parent {
            if let Some(id) = self.id(&widget) {
                return Some(id);
            }

            parent = widget.parent();
        }

        None
    }

    pub(super) fn id(&self, root: &gtk::Widget) -> Option<PanelId> {
        PanelId::ALL.into_iter().find(|id| {
            self.entries[*id as usize].as_ref().is_some_and(|panel| {
                panel.root == *root || panel.slot.upcast_ref::<gtk::Widget>() == root
            })
        })
    }

    pub(super) fn presented(&self, id: PanelId) -> bool {
        self.root(id).is_some_and(presentation::is_presented)
    }

    pub(super) fn window(&self, id: PanelId) -> Option<gtk::Window> {
        self.root(id).and_then(host_window)
    }

    fn action_window(
        &self,
        mut active: Option<gtk::Window>,
        fallback: &gtk::Window,
    ) -> gtk::Window {
        // Transient dialogs may be the active application window. Never make
        // a retained dialog its own parent or parent one workspace to another.
        for _ in 0..16 {
            let Some(window) = active else {
                break;
            };

            if window == *fallback
                || self
                    .entries
                    .iter()
                    .flatten()
                    .any(|panel| host_window(&panel.root).as_ref() == Some(&window))
            {
                return window;
            }

            active = window.transient_for();
        }

        fallback.clone()
    }

    pub(super) fn reveal(&self, id: PanelId) {
        let Some(root) = self.root(id) else {
            return;
        };

        if let Some(slot) = self.slot(id)
            && root.parent().as_ref() == Some(slot.upcast_ref())
            && let Some(notebook) = slot
                .ancestor(gtk::Notebook::static_type())
                .and_downcast::<gtk::Notebook>()
            && let Some(page) = notebook.page_num(slot)
        {
            notebook.set_current_page(Some(page));
        }

        if let Some(window) = host_window(root) {
            window.present();
        }
    }
}

fn sync_home_visibility(slot: &gtk::Box, root: &gtk::Widget, collapse_home: bool) {
    slot.set_visible(
        root.get_visible() && (!collapse_home || root.parent().as_ref() == Some(slot.upcast_ref())),
    );
}

pub(super) fn host_window(widget: &impl IsA<gtk::Widget>) -> Option<gtk::Window> {
    widget.as_ref().root().and_downcast::<gtk::Window>()
}

impl Ui {
    // Global actions can be invoked by a shortcut in any workspace window.
    pub(super) fn action_window(&self) -> gtk::Window {
        self.panels.action_window(
            self.window
                .application()
                .and_then(|app| app.active_window()),
            self.window.upcast_ref(),
        )
    }

    pub(super) fn panel_window(&self, id: PanelId) -> gtk::Window {
        self.panels
            .window(id)
            .unwrap_or_else(|| self.action_window())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hosting::Action;

    #[test]
    #[ignore = "requires a GTK display, run separately from other GTK tests"]
    fn detached_layout_restores_nested_hosts_hidden_console_and_final_geometry() {
        gtk::init().unwrap();
        Theme::graphite().install();

        let application = gtk::Application::builder()
            .application_id("dev.fgdb.LayoutTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();

        application.register(None::<&gio::Cancellable>).unwrap();
        let main = glib::MainContext::default();
        let wait = |ready: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(3);

            while !ready() && Instant::now() < deadline {
                main.block_on(glib::timeout_future(Duration::from_millis(10)));
            }

            assert!(ready(), "Saved workspace did not settle");
        };

        struct Fixture {
            window: gtk::ApplicationWindow,
            hosts: Rc<Hosts>,
            panels: Rc<Panels>,
            layout: layout::Persistence,
            notebook: gtk::Notebook,
            split: gtk::Paned,
            details: gtk::Stack,
            console: gtk::Stack,
            log_toggle: gtk::ToggleButton,
        }

        let temporary =
            glib::mkdtemp(std::env::temp_dir().join("fgdb-workspace-layout-XXXXXX")).unwrap();

        let path = temporary.join("layout.conf");
        let build = |restore| {
            let mut panels = Panels::default();
            let notebook = gtk::Notebook::new();
            let split = gtk::Paned::new(gtk::Orientation::Vertical);
            split.set_start_child(Some(&gtk::Entry::new()));
            split.set_end_child(Some(&gtk::Entry::new()));
            panels.append(&notebook, PanelId::Registers, &split);
            let details = gtk::Stack::new();
            details.add_named(&gtk::Label::new(Some("First")), Some("first"));
            details.add_named(&gtk::Label::new(Some("Second")), Some("second"));
            panels.append(&notebook, PanelId::Stack, &details);
            let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
            root.append(&panels.register_collapsible(PanelId::RightPane, &notebook));
            let console = gtk::Stack::new();
            let terminal = gtk::Entry::new();
            let log = gtk::Label::new(Some("Log"));
            console.add_named(&terminal, Some("terminal"));
            console.add_named(&log, Some("log"));
            root.append(&panels.register(PanelId::Console, &console));
            let terminal_toggle = gtk::ToggleButton::with_label("Terminal");
            let log_toggle = gtk::ToggleButton::with_label("Log");
            root.append(&terminal_toggle);
            root.append(&log_toggle);

            let window = gtk::ApplicationWindow::builder()
                .application(&application)
                .default_width(600)
                .default_height(440)
                .child(&root)
                .build();

            let panels = Rc::new(panels);
            let hosts = Hosts::new(&window, Rc::clone(&panels));
            hosts.install(|_| {});
            hosts.set_restore_on_startup(restore);
            let layout = layout::Persistence::install_at(
                &window,
                vec![layout::Pane::new("test_split", &split)],
                path.clone(),
            );
            layout.bind_notebook("inspector", &notebook, &panels);
            layout.bind_stack("details", &details);
            console::install(
                &console,
                &terminal_toggle,
                &log_toggle,
                terminal.upcast_ref(),
                log.upcast_ref(),
                layout.console_view(),
                layout.console_handler(),
            );

            hosts.bind_layout(&layout);
            window.present();

            Fixture {
                window,
                hosts,
                panels,
                layout,
                notebook,
                split,
                details,
                console,
                log_toggle,
            }
        };

        let first = build(true);
        wait(&|| first.window.is_mapped());
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        first.split.set_position(first.split.max_position() / 3);
        first.notebook.set_current_page(Some(1));
        main.block_on(glib::timeout_future(Duration::from_millis(20)));
        first.notebook.set_current_page(Some(0));
        main.block_on(glib::timeout_future(Duration::from_millis(20)));
        let edited_position = first.split.max_position() * 2 / 3;
        first.split.set_position(edited_position);
        main.block_on(glib::timeout_future(Duration::from_millis(140)));
        assert_eq!(first.split.position(), edited_position);

        for id in [PanelId::Registers, PanelId::RightPane, PanelId::Console] {
            first.hosts.request(id, Action::PopOut);
        }

        wait(&|| first.hosts.floating_window(PanelId::RightPane).is_some());
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        let right = first.hosts.floating_window(PanelId::RightPane).unwrap();
        right.set_default_size(560, 400);
        first
            .hosts
            .floating_window(PanelId::Registers)
            .unwrap()
            .set_default_size(480, 360);

        first.notebook.set_current_page(Some(1));
        first.details.set_visible_child_name("second");
        first.log_toggle.set_active(true);
        first.log_toggle.set_active(false);
        assert!(!first.console.get_visible());
        first.layout.save();
        right.set_default_size(580, 410);
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        let saved_right = first.layout.panel(PanelId::RightPane).unwrap();
        let saved_registers = first.layout.panel(PanelId::Registers).unwrap();
        let saved_main = layout::WindowGeometry::capture(&first.window).unwrap();
        first.window.close();
        wait(&|| first.hosts.floating_window(PanelId::RightPane).is_none());
        wait(&|| first.layout.final_save_done());
        let saved = std::fs::read_to_string(&path).unwrap();
        first.layout.save();
        main.block_on(glib::timeout_future(Duration::from_millis(400)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
        assert!(saved.contains("panel.right-pane=1,"));
        assert!(saved.contains("panel.registers=1,"));
        assert!(saved.contains("panel.console=1,"));

        let restored = build(true);
        wait(&|| restored.hosts.floating_window(PanelId::RightPane).is_some());
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        assert_eq!(restored.layout.panel(PanelId::RightPane), Some(saved_right));
        assert_eq!(
            restored.layout.panel(PanelId::Registers),
            Some(saved_registers)
        );

        assert_eq!(
            layout::WindowGeometry::capture(&restored.window),
            Some(saved_main)
        );

        assert_eq!(restored.notebook.current_page(), Some(1));
        assert_eq!(
            restored.details.visible_child_name().as_deref(),
            Some("second")
        );

        assert!(!restored.console.get_visible());
        let hidden_console = restored.hosts.floating_window(PanelId::Console).unwrap();
        assert!(!hidden_console.is_visible());
        restored.log_toggle.set_active(true);
        wait(&|| hidden_console.is_mapped());
        assert_eq!(
            restored.console.visible_child_name().as_deref(),
            Some("log")
        );

        assert_eq!(
            restored.panels.window(PanelId::Stack),
            restored.panels.window(PanelId::RightPane)
        );

        assert_ne!(
            restored.panels.window(PanelId::Registers),
            restored.panels.window(PanelId::RightPane)
        );

        let controls = restored.hosts.settings_controls();
        let weak_controls = controls.downgrade();
        drop(controls);
        assert!(weak_controls.upgrade().is_none());
        restored.window.close();
        wait(&|| restored.hosts.floating_window(PanelId::RightPane).is_none());
        wait(&|| restored.layout.final_save_done());

        let docked = build(false);
        wait(&|| docked.window.is_mapped());
        main.block_on(glib::timeout_future(Duration::from_millis(100)));

        for id in [PanelId::Registers, PanelId::RightPane, PanelId::Console] {
            assert!(docked.hosts.floating_window(id).is_none());
            assert!(!docked.layout.panel(id).unwrap().floating);
        }

        docked.hosts.set_restore_on_startup(true);
        assert!(docked.hosts.floating_window(PanelId::RightPane).is_none());
        docked.hosts.request(PanelId::Registers, Action::PopOut);
        wait(&|| docked.hosts.floating_window(PanelId::Registers).is_some());
        assert_eq!(
            docked.layout.panel(PanelId::Registers),
            Some(saved_registers)
        );

        docked.hosts.dock_all();
        wait(&|| docked.hosts.floating_window(PanelId::Registers).is_none());
        assert!(!docked.layout.panel(PanelId::Registers).unwrap().floating);
        docked.window.close();
        wait(&|| docked.layout.final_save_done());
        std::fs::remove_file(path.with_extension("lock")).unwrap();
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(temporary).unwrap();
    }

    #[test]
    #[ignore = "requires a GTK display, run separately from other GTK tests"]
    fn panels_follow_identity_presentation_and_host_allocation() {
        gtk::init().unwrap();
        Theme::graphite().install();
        gtk::Settings::default()
            .unwrap()
            .set_gtk_decoration_layout(Some("icon:minimize,maximize,close"));

        let application = gtk::Application::builder()
            .application_id("dev.fgdb.PanelTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();

        application.register(None::<&gio::Cancellable>).unwrap();
        let main = glib::MainContext::default();
        let wait = |ready: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(3);

            while !ready() && Instant::now() < deadline {
                main.block_on(glib::timeout_future(Duration::from_millis(10)));
            }

            assert!(ready(), "GTK presentation did not settle");
        };

        let notebook = gtk::Notebook::new();
        let mut panels = Panels::default();
        let registers = ResponsiveBox::new();
        let draft = gtk::Entry::new();
        draft.set_text("preserve this draft");
        registers.append(&draft);
        let stack = gtk::Box::new(gtk::Orientation::Vertical, 0);
        stack.append(&gtk::Label::new(Some("Stack contents")));
        let misc = build_misc_view(&Theme::graphite());
        panels.append(&notebook, PanelId::Registers, &registers);
        panels.append(&notebook, PanelId::Stack, &stack);
        panels.append(&notebook, PanelId::Misc, &misc.root);
        let navigation = build_compact_navigation(&notebook);
        navigation.set_visible(true);
        let right_pane = ResponsiveBox::new();
        right_pane.append(&navigation);
        right_pane.append(&notebook);
        let right_home = panels.register_collapsible(PanelId::RightPane, &right_pane);
        let workspace = gtk::Paned::new(gtk::Orientation::Horizontal);
        let source = gtk::Box::new(gtk::Orientation::Vertical, 0);
        workspace.set_start_child(Some(&source));
        workspace.set_end_child(Some(&right_home));
        workspace.set_position(8);
        workspace.set_resize_start_child(false);
        workspace.set_resize_end_child(true);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&workspace);
        let console = gtk::Stack::new();
        console.add_named(&gtk::Entry::new(), Some("terminal"));
        console.add_named(&gtk::Label::new(Some("Log messages")), Some("log"));
        content.append(&panels.register(PanelId::Console, &console));
        console.set_visible(false);

        let dock = gtk::ApplicationWindow::builder()
            .application(&application)
            .default_width(900)
            .default_height(400)
            .child(&content)
            .build();

        let panels = Rc::new(panels);
        let hosts = Hosts::new(&dock, Rc::clone(&panels));
        let installed = Rc::new(Cell::new(0));
        let observed_installs = Rc::clone(&installed);
        hosts.install(move |_| observed_installs.set(observed_installs.get() + 1));
        let menu = hosts.menu_button();
        content.append(&menu);
        let events = Rc::new(RefCell::new(Vec::new()));
        let observed = Rc::clone(&events);
        connect_presentation(&registers, move |visible| {
            observed.borrow_mut().push(visible)
        });

        dock.present();
        wait(&|| !events.borrow().is_empty());
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        assert!(panels.presented(PanelId::Registers));
        assert!(!panels.presented(PanelId::Stack));
        assert_eq!(*events.borrow(), vec![true]);
        assert!(!registers.has_css_class("inspector-compact"));
        assert_eq!(
            navigation.compute_bounds(&content).unwrap().height(),
            notebook
                .first_child()
                .unwrap()
                .compute_bounds(&content)
                .unwrap()
                .height()
        );

        let home = panels.slot(PanelId::Registers).unwrap();
        notebook.reorder_child(home, Some(1));
        panels.reveal(PanelId::Registers);
        main.block_on(glib::timeout_future(Duration::from_millis(50)));
        assert_eq!(notebook.current_page(), Some(1));

        let selector = navigation
            .first_child()
            .unwrap()
            .next_sibling()
            .unwrap()
            .downcast::<gtk::DropDown>()
            .unwrap();

        wait(&|| selector.selected() == 1);

        assert_eq!(
            selector
                .selected_item()
                .and_downcast::<gtk::StringObject>()
                .unwrap()
                .string(),
            "Registers"
        );

        selector.set_selected(0);
        main.block_on(glib::timeout_future(Duration::from_millis(50)));
        assert!(panels.presented(PanelId::Stack));
        assert!(!panels.presented(PanelId::Registers));
        panels.reveal(PanelId::Registers);
        main.block_on(glib::timeout_future(Duration::from_millis(50)));
        let events_before_move = events.borrow().len();
        hosts.request(PanelId::Registers, Action::PopOut);
        hosts.request(PanelId::Registers, Action::PopOut);
        assert!(hosts.floating_window(PanelId::Registers).is_none());
        wait(&|| hosts.floating_window(PanelId::Registers).is_some());
        let floating = hosts.floating_window(PanelId::Registers).unwrap();
        let titlebar = floating
            .titlebar()
            .and_downcast::<gtk::HeaderBar>()
            .unwrap();

        assert!(!titlebar.shows_title_buttons());
        let mut pending = vec![titlebar.upcast::<gtk::Widget>()];
        let mut controls = Vec::new();
        let mut icons = 0;

        while let Some(widget) = pending.pop() {
            if let Some(image) = widget.downcast_ref::<gtk::Image>()
                && image.icon_name().as_deref() == Some(crate::APPLICATION_ID)
            {
                icons += 1;
            }

            if let Some(button) = widget.downcast_ref::<gtk::Button>()
                && button.has_css_class("window-control")
            {
                controls.push(button.clone());
            }

            let mut child = widget.first_child();

            while let Some(widget) = child {
                child = widget.next_sibling();
                pending.push(widget);
            }
        }

        assert_eq!(icons, 1);
        assert_eq!(controls.len(), 3);

        for (class, label) in [("minimize", "−"), ("maximize", "□"), ("close", "×")] {
            let button = controls
                .iter()
                .find(|button| button.has_css_class(class))
                .unwrap();

            assert_eq!(button.label().as_deref(), Some(label));
            assert!(button.is_visible());
        }

        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        floating.set_default_size(420, 300);
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        wait(&|| registers.has_css_class("inspector-compact"));
        assert_eq!(
            panels.window(PanelId::Registers),
            Some(floating.clone().upcast())
        );
        assert!(panels.presented(PanelId::Registers));
        assert_eq!(notebook.n_pages(), 3);
        assert_eq!(notebook.page_num(home), Some(1));
        assert_eq!(installed.get(), 1);
        assert_eq!(events.borrow().len(), events_before_move);
        assert!(registers.has_css_class("inspector-compact"));
        panels.reveal(PanelId::Stack);
        wait(&|| panels.presented(PanelId::Stack));
        assert!(panels.presented(PanelId::Registers));
        let dialog = gtk::Window::builder()
            .transient_for(&floating)
            .destroy_with_parent(true)
            .child(&gtk::Entry::new())
            .build();

        dialog.present();
        assert_eq!(
            panels.action_window(Some(dialog.clone()), dock.upcast_ref()),
            floating
        );
        floating.set_default_size(900, 300);
        wait(&|| !registers.has_css_class("inspector-compact"));
        controls
            .iter()
            .find(|button| button.has_css_class("close"))
            .unwrap()
            .emit_clicked();

        wait(&|| hosts.floating_window(PanelId::Registers).is_none());
        assert_eq!(
            panels.window(PanelId::Registers),
            Some(dock.clone().upcast())
        );
        assert_eq!(dialog.transient_for(), Some(dock.clone().upcast()));
        assert!(dialog.is_visible());
        assert_eq!(draft.text(), "preserve this draft");
        assert_eq!(panels.id(home.upcast_ref()), Some(PanelId::Registers));
        dialog.close();

        // A queued reversal does not create an intermediate host or rebuild the panel.
        hosts.request(PanelId::Registers, Action::PopOut);
        hosts.request(PanelId::Registers, Action::Dock);
        main.block_on(glib::timeout_future(Duration::from_millis(50)));
        assert!(hosts.floating_window(PanelId::Registers).is_none());
        assert_eq!(installed.get(), 1);

        let viewer = gtk::Window::builder()
            .transient_for(&dock)
            .destroy_with_parent(true)
            .build();

        panels.track_dialog(PanelId::Registers, &viewer);
        viewer.present();

        for _ in 0..3 {
            hosts.request(PanelId::Registers, Action::PopOut);
            hosts.request(PanelId::Registers, Action::Show);
            wait(&|| hosts.floating_window(PanelId::Registers).is_some());
            assert_eq!(viewer.transient_for(), panels.window(PanelId::Registers));
            hosts.request(PanelId::Registers, Action::Dock);
            hosts.request(PanelId::Registers, Action::Show);
            wait(&|| hosts.floating_window(PanelId::Registers).is_none());
            assert_eq!(draft.text(), "preserve this draft");
            assert_eq!(viewer.transient_for(), Some(dock.clone().upcast()));
        }

        viewer.close();

        // The container moves without reclaiming an independently floating tab.
        assert_eq!(
            panels.home_container(PanelId::Registers),
            Some(PanelId::RightPane)
        );

        let docked_position = workspace.position();
        hosts.request(PanelId::Stack, Action::PopOut);
        hosts.request(PanelId::RightPane, Action::PopOut);
        wait(&|| hosts.floating_window(PanelId::RightPane).is_some());
        let right_window = hosts.floating_window(PanelId::RightPane).unwrap();
        let stack_window = hosts.floating_window(PanelId::Stack).unwrap();
        wait(&|| source.width() >= workspace.width() - 2);
        assert!(!right_home.get_visible());
        assert!(right_home.first_child().is_none());
        assert_eq!(notebook.n_pages(), 3);
        assert_eq!(notebook.current_page(), Some(1));
        assert_eq!(
            panels.window(PanelId::Registers),
            Some(right_window.clone().upcast())
        );

        assert_eq!(
            panels.window(PanelId::Stack),
            Some(stack_window.clone().upcast())
        );

        assert_eq!(draft.text(), "preserve this draft");
        right_pane.set_visible(false);
        right_pane.set_visible(true);
        assert!(!right_home.get_visible());

        let stack_dialog = gtk::Window::builder()
            .transient_for(&stack_window)
            .destroy_with_parent(true)
            .child(&gtk::Entry::new())
            .build();

        stack_dialog.present();
        hosts.request(PanelId::Stack, Action::Dock);
        wait(&|| hosts.floating_window(PanelId::Stack).is_none());
        assert_eq!(
            stack_dialog.transient_for(),
            Some(right_window.clone().upcast())
        );

        assert!(stack_dialog.is_visible());
        assert_eq!(notebook.current_page(), Some(0));
        assert_eq!(
            panels.home_container(PanelId::Stack),
            Some(PanelId::RightPane)
        );

        hosts.request(PanelId::Registers, Action::PopOut);
        wait(&|| hosts.floating_window(PanelId::Registers).is_some());
        let registers_window = hosts.floating_window(PanelId::Registers).unwrap();

        let registers_dialog = gtk::Window::builder()
            .transient_for(&registers_window)
            .destroy_with_parent(true)
            .child(&gtk::Entry::new())
            .build();

        registers_dialog.present();
        right_window.close();
        wait(&|| hosts.floating_window(PanelId::RightPane).is_none());
        wait(&|| right_home.is_mapped() && source.width() < workspace.width() - 2);
        assert_eq!(workspace.position(), docked_position);
        assert_eq!(notebook.current_page(), Some(0));
        assert_eq!(panels.window(PanelId::Stack), Some(dock.clone().upcast()));
        assert_eq!(stack_dialog.transient_for(), Some(dock.clone().upcast()));
        assert!(stack_dialog.is_visible());
        assert_eq!(
            panels.window(PanelId::Registers),
            Some(registers_window.clone().upcast())
        );

        assert_eq!(
            registers_dialog.transient_for(),
            Some(registers_window.upcast())
        );

        hosts.request(PanelId::Registers, Action::Dock);
        wait(&|| hosts.floating_window(PanelId::Registers).is_none());
        assert_eq!(
            registers_dialog.transient_for(),
            Some(dock.clone().upcast())
        );

        assert!(registers_dialog.is_visible());

        // Dock-all can retire a child host before its containing host.
        hosts.request(PanelId::RightPane, Action::PopOut);
        hosts.request(PanelId::Registers, Action::PopOut);
        wait(&|| hosts.floating_window(PanelId::RightPane).is_some());

        for id in PanelId::ALL {
            hosts.request(id, Action::DockQuietly);
        }

        wait(&|| hosts.floating_window(PanelId::RightPane).is_none());
        assert!(hosts.floating_window(PanelId::Registers).is_none());
        assert_eq!(draft.text(), "preserve this draft");
        stack_dialog.close();
        registers_dialog.close();

        console.set_visible(true);
        hosts.request(PanelId::Console, Action::PopOut);
        wait(&|| hosts.floating_window(PanelId::Console).is_some());
        let console_window = hosts.floating_window(PanelId::Console).unwrap();
        console.set_visible_child_name("log");
        console.set_visible(false);
        assert!(!console_window.is_visible());
        console.set_visible(true);
        assert!(console_window.is_visible());
        assert_eq!(console.visible_child_name().as_deref(), Some("log"));

        hosts.request(PanelId::Misc, Action::PopOut);
        wait(&|| hosts.floating_window(PanelId::Misc).is_some());
        let misc_window = hosts.floating_window(PanelId::Misc).unwrap();
        wait(&|| misc.root.height() > 200 && misc.root.height() >= misc_window.height() - 80);
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        let wide_navigation = misc.root.first_child().unwrap().first_child().unwrap();
        let compact_navigation = wide_navigation.next_sibling().unwrap();
        let navigation_height = wide_navigation.compute_bounds(&misc.root).unwrap().height();
        misc_window.set_default_size(420, 400);
        wait(&|| misc.root.has_css_class("inspector-compact"));
        wait(&|| {
            compact_navigation.is_mapped()
                && compact_navigation
                    .compute_bounds(&misc.root)
                    .unwrap()
                    .height()
                    == navigation_height
        });

        let misc_dialog = gtk::Window::builder()
            .transient_for(&misc_window)
            .destroy_with_parent(true)
            .child(&gtk::Entry::new())
            .build();

        panels.track_dialog(PanelId::Misc, &misc_dialog);
        misc_dialog.present();
        misc_window.destroy();
        wait(&|| hosts.floating_window(PanelId::Misc).is_none());
        assert!(misc_dialog.is_visible());
        assert_eq!(misc_dialog.transient_for(), Some(dock.clone().upcast()));
        misc_dialog.destroy();
        assert_eq!(
            misc.root.parent().as_ref(),
            Some(panels.slot(PanelId::Misc).unwrap().upcast_ref())
        );

        menu.popup();
        main.block_on(glib::timeout_future(Duration::from_millis(50)));
        menu.popdown();
        hosts.request(PanelId::Registers, Action::PopOut);
        hosts.request(PanelId::RightPane, Action::PopOut);
        wait(&|| hosts.floating_window(PanelId::RightPane).is_some());
        let floating = hosts.floating_window(PanelId::Registers).unwrap();
        let right_window = hosts.floating_window(PanelId::RightPane).unwrap();

        // Closing the application retires every host and ignores queued pop-outs.
        dock.close();
        hosts.request(PanelId::Stack, Action::PopOut);
        wait(&|| hosts.floating_window(PanelId::Registers).is_none());
        assert!(hosts.floating_window(PanelId::Console).is_none());
        assert!(hosts.floating_window(PanelId::Stack).is_none());
        assert!(hosts.floating_window(PanelId::RightPane).is_none());
        assert!(!floating.is_visible());
        assert!(!console_window.is_visible());
        assert!(!right_window.is_visible());
    }
}
