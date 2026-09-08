//! Move a panel's existing widget between its fixed home and one window.
//! Only the GTK main loop applies transitions, never a menu or layout signal.

use super::*;

#[derive(Clone, Copy)]
pub(super) enum Action {
    PopOut,
    Restore,
    Dock,
    DockQuietly,
    Show,
}

#[derive(Clone, Copy, Default)]
struct Pending {
    placement: Option<Placement>,
    show: bool,
}

#[derive(Clone, Copy)]
enum Placement {
    Floating { restoring: bool },
    Docked { reveal: bool },
}

impl Pending {
    fn merge(mut self, action: Action) -> Self {
        match action {
            Action::Show => self.show = true,
            Action::PopOut => self.placement = Some(Placement::Floating { restoring: false }),
            Action::Restore => self.placement = Some(Placement::Floating { restoring: true }),
            Action::Dock => self.placement = Some(Placement::Docked { reveal: true }),
            Action::DockQuietly => self.placement = Some(Placement::Docked { reveal: false }),
        }

        self
    }
}

type HostSetup = Rc<dyn Fn(&gtk::ApplicationWindow)>;

struct Floating {
    window: gtk::ApplicationWindow,
    content: gtk::Box,
    placeholder: Option<gtk::Box>,
}

pub(in crate::ui) struct Hosts {
    pub(super) panels: Rc<Panels>,
    main: glib::WeakRef<gtk::ApplicationWindow>,
    floating: [RefCell<Option<Floating>>; PanelId::ALL.len()],
    pending: [Cell<Pending>; PanelId::ALL.len()],
    source: RefCell<Option<glib::SourceId>>,
    closing: Cell<bool>,
    setup: RefCell<Option<HostSetup>>,
    pub(super) layout: RefCell<Option<layout::Persistence>>,
    restore_on_startup: Cell<bool>,
    observers: RefCell<Vec<std::rc::Weak<dyn Fn()>>>,
    pub(super) menu: RefCell<Option<gtk::Popover>>,
}

impl Hosts {
    pub(in crate::ui) fn new(main: &gtk::ApplicationWindow, panels: Rc<Panels>) -> Rc<Self> {
        let hosts = Rc::new(Self {
            panels,
            main: main.downgrade(),
            floating: std::array::from_fn(|_| RefCell::new(None)),
            pending: std::array::from_fn(|_| Cell::default()),
            source: RefCell::new(None),
            closing: Cell::new(false),
            setup: RefCell::new(None),
            layout: RefCell::new(None),
            restore_on_startup: Cell::new(true),
            observers: RefCell::new(Vec::new()),
            menu: RefCell::new(None),
        });

        let weak = Rc::downgrade(&hosts);

        main.connect_close_request(move |_| {
            if let Some(hosts) = weak.upgrade() {
                hosts.begin_shutdown();
            }

            glib::Propagation::Proceed
        });

        let weak = Rc::downgrade(&hosts);

        main.connect_unrealize(move |_| {
            if let Some(hosts) = weak.upgrade() {
                hosts.begin_shutdown();
            }
        });

        hosts
    }

    pub(in crate::ui) fn install(
        self: &Rc<Self>,
        setup: impl Fn(&gtk::ApplicationWindow) + 'static,
    ) {
        if self.setup.borrow().is_some() {
            return;
        }

        self.setup.replace(Some(Rc::new(setup)));
        if let Some(main) = self.main.upgrade() {
            self.panels.dialogs.install(&main);
        }

        self.install_tab_menus();

        // The shared Terminal/Log stack remains the only owner of console state.
        // Its existing toggles also show or hide the floating host.
        if let Some(root) = self.panels.root(PanelId::Console) {
            let weak = Rc::downgrade(self);

            root.connect_visible_notify(move |root| {
                if let Some(hosts) = weak.upgrade() {
                    if let Some(window) = hosts.floating_window(PanelId::Console) {
                        if root.get_visible() {
                            window.present();
                        } else {
                            window.set_visible(false);
                        }
                    }

                    hosts.notify_changed();
                }
            });
        }
    }

    pub(in crate::ui) fn set_restore_on_startup(&self, enabled: bool) {
        self.restore_on_startup.set(enabled);
    }

    pub(super) fn bind_layout(self: &Rc<Self>, layout: &layout::Persistence) {
        self.layout.replace(Some(layout.clone()));
        let weak = Rc::downgrade(self);

        layout.on_ready(move || {
            if let Some(hosts) = weak.upgrade() {
                hosts.restore_saved();
            }
        });
    }

    fn restore_saved(self: &Rc<Self>) {
        let Some(layout) = self.layout.borrow().clone() else {
            return;
        };

        for id in PanelId::ALL {
            let Some(mut placement) = layout.panel(id) else {
                continue;
            };

            if self.pending[id as usize].get().placement.is_some()
                || self.floating_window(id).is_some()
            {
                continue;
            }

            if placement.floating && self.restore_on_startup.get() {
                self.request(id, Action::Restore);
            } else {
                placement.floating = false;
                layout.remember_panel(id, placement);
            }
        }
    }

    pub(super) fn observe(
        self: &Rc<Self>,
        owner: &impl IsA<gtk::Widget>,
        changed: impl Fn(&Self) + 'static,
    ) {
        let weak = Rc::downgrade(self);

        let callback: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(hosts) = weak.upgrade() {
                changed(&hosts);
            }
        });

        self.observers.borrow_mut().push(Rc::downgrade(&callback));
        callback();
        owner.connect_map(move |_| callback());
    }

    fn notify_changed(&self) {
        let callbacks = {
            let mut observers = self.observers.borrow_mut();
            observers.retain(|observer| observer.strong_count() > 0);
            observers
                .iter()
                .filter_map(std::rc::Weak::upgrade)
                .collect::<Vec<_>>()
        };

        for callback in callbacks {
            callback();
        }
    }

    pub(super) fn accepting_actions(&self) -> bool {
        !self.closing.get()
    }

    pub(super) fn dock_all(self: &Rc<Self>) {
        for id in PanelId::ALL {
            self.request(id, Action::DockQuietly);
        }
    }

    pub(super) fn floating_window(&self, id: PanelId) -> Option<gtk::ApplicationWindow> {
        self.floating[id as usize]
            .borrow()
            .as_ref()
            .map(|host| host.window.clone())
    }

    pub(super) fn request(self: &Rc<Self>, id: PanelId, action: Action) {
        if self.closing.get() || self.panels.root(id).is_none() {
            return;
        }

        let pending = &self.pending[id as usize];
        pending.set(pending.get().merge(action));
        self.schedule();
    }

    fn schedule(self: &Rc<Self>) {
        if self.source.borrow().is_some() {
            return;
        }

        let weak = Rc::downgrade(self);

        let source = glib::idle_add_local_once(move || {
            let Some(hosts) = weak.upgrade() else {
                return;
            };

            hosts.source.borrow_mut().take();

            if hosts.closing.get() {
                hosts.shutdown();
                return;
            }

            for id in PanelId::ALL {
                let pending = hosts.pending[id as usize].take();

                match pending.placement {
                    Some(Placement::Floating { restoring }) => hosts.pop_out(id, restoring),
                    Some(Placement::Docked { reveal }) => hosts.dock(id, reveal),
                    None => {}
                }

                if pending.show {
                    hosts.panels.reveal(id);
                }
            }

            hosts.notify_changed();
        });

        self.source.replace(Some(source));
    }

    fn pop_out(self: &Rc<Self>, id: PanelId, restoring: bool) {
        if let Some(window) = self.floating_window(id) {
            window.present();
            return;
        }

        let (Some(main), Some(root), Some(slot)) = (
            self.main.upgrade(),
            self.panels.root(id),
            self.panels.slot(id),
        ) else {
            return;
        };

        // Validate exact ownership before any structural GTK operation.
        if (!root.get_visible() && !restoring) || root.parent().as_ref() != Some(slot.upcast_ref())
        {
            return;
        }

        let Some(application) = main.application() else {
            return;
        };

        let window = gtk::ApplicationWindow::builder()
            .application(&application)
            .title(format!("fgdb - {}", id.title()))
            .default_width(initial_extent(root.width(), 900, 420, 1600))
            .default_height(initial_extent(root.height(), 600, 300, 1200))
            .css_classes(["fgdb-window"])
            .build();

        if let Some(placement) = self
            .layout
            .borrow()
            .as_ref()
            .and_then(|layout| layout.panel(id))
        {
            placement.geometry.apply(&window);
        }

        let titlebar = gtk::HeaderBar::new();
        titlebar.add_css_class("topbar");
        titlebar.set_show_title_buttons(false);
        let icon = gtk::Image::from_icon_name(crate::APPLICATION_ID);
        icon.set_pixel_size(18);
        icon.set_margin_start(components::CONTENT_INSET);
        icon.set_margin_end(components::CONTENT_INSET);
        titlebar.pack_start(&icon);
        let title = gtk::Label::new(Some(id.title()));
        title.add_css_class("app-title");
        titlebar.set_title_widget(Some(&title));
        let dock = gtk::Button::new();
        dock.set_child(Some(&components::icon_label(
            "view-restore-symbolic",
            "Dock",
        )));

        dock.set_tooltip_text(Some("Return this panel to its original position"));
        dock.add_css_class("toolbar-action");
        titlebar.pack_start(&dock);
        titlebar.pack_end(&components::window_controls(&window));
        window.set_titlebar(Some(&titlebar));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.add_css_class("debugger-root");
        content.set_vexpand(true);
        window.set_child(Some(&content));
        let placeholder = (!self.panels.collapses_home(id)).then(|| self.placeholder(id));
        let weak = Rc::downgrade(self);

        dock.connect_clicked(move |_| {
            if let Some(hosts) = weak.upgrade() {
                hosts.request(id, Action::Dock);
            }
        });

        let weak = Rc::downgrade(self);

        window.connect_close_request(move |_| {
            if let Some(hosts) = weak.upgrade() {
                hosts.request(id, Action::Dock);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });

        let weak = Rc::downgrade(self);

        window.connect_unrealize(move |window| {
            if let Some(hosts) = weak.upgrade()
                && hosts.floating_window(id).as_ref() == Some(window)
            {
                if let Some(main) = hosts.main.upgrade() {
                    hosts
                        .panels
                        .dialogs
                        .retire_host(window.upcast_ref(), main.upcast_ref());
                }

                hosts.request(id, Action::DockQuietly);
            }
        });

        let setup = self.setup.borrow().clone();

        if let Some(setup) = setup {
            setup(&window);
        }

        let focus = release_focus(root);
        slot.remove(root);

        if let Some(placeholder) = &placeholder {
            slot.append(placeholder);
        }

        content.append(root);
        self.panels.sync_home(id);

        self.floating[id as usize].replace(Some(Floating {
            window: window.clone(),
            content,
            placeholder,
        }));

        self.panels.dialogs.rehost(&self.panels, None);

        for property in ["default-width", "default-height", "maximized"] {
            let weak = Rc::downgrade(self);

            window.connect_notify_local(Some(property), move |window, _| {
                if let Some(hosts) = weak.upgrade()
                    && hosts.floating_window(id).as_ref() == Some(window)
                {
                    hosts.remember_placement(id, true, Some(window));
                }
            });
        }

        if root.get_visible() {
            window.present();
        }

        self.remember_placement(id, true, Some(&window));
        restore_focus(focus);
    }

    fn remember_placement(
        &self,
        id: PanelId,
        floating: bool,
        window: Option<&gtk::ApplicationWindow>,
    ) {
        if self.closing.get() {
            return;
        }

        let Some(layout) = self.layout.borrow().clone() else {
            return;
        };

        let geometry = window
            .and_then(layout::WindowGeometry::capture)
            .or_else(|| layout.panel(id).map(|placement| placement.geometry));

        if let Some(geometry) = geometry {
            layout.remember_panel(id, layout::PanelPlacement { floating, geometry });
        }
    }

    fn dock(&self, id: PanelId, reveal: bool) {
        let host = self.floating[id as usize].borrow_mut().take();

        let Some(host) = host else {
            self.remember_placement(id, false, None);
            return;
        };

        self.remember_placement(id, false, Some(&host.window));
        let (Some(root), Some(slot)) = (self.panels.root(id), self.panels.slot(id)) else {
            host.window.destroy();
            return;
        };

        let focus = release_focus(root);

        if root.parent().as_ref() == Some(host.content.upcast_ref()) {
            host.content.remove(root);
        }

        if let Some(placeholder) = &host.placeholder
            && placeholder.parent().as_ref() == Some(slot.upcast_ref())
        {
            slot.remove(placeholder);
        }

        if root.parent().is_none() {
            slot.append(root);
        }

        self.panels.sync_home(id);
        let parent =
            host_window(slot).or_else(|| self.main.upgrade().map(|window| window.upcast()));

        if let Some(parent) = parent {
            self.panels
                .dialogs
                .rehost(&self.panels, Some((host.window.upcast_ref(), &parent)));
        }

        host.window.destroy();

        if reveal && !self.closing.get() {
            self.panels.reveal(id);
            restore_focus(focus);
        }
    }

    fn placeholder(self: &Rc<Self>, id: PanelId) -> gtk::Box {
        let placeholder = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
        placeholder.set_halign(gtk::Align::Center);
        placeholder.set_valign(gtk::Align::Center);
        placeholder.set_margin_top(components::CONTENT_INSET);
        placeholder.set_margin_bottom(components::CONTENT_INSET);
        placeholder.set_margin_start(components::CONTENT_INSET);
        placeholder.set_margin_end(components::CONTENT_INSET);
        let message = gtk::Label::new(Some("Open in a separate window"));
        message.add_css_class("muted");
        message.set_wrap(true);
        message.set_justify(gtk::Justification::Center);
        placeholder.append(&message);
        let actions = components::control_row();
        actions.set_halign(gtk::Align::Center);

        for (label, action) in [("Show window", Action::Show), ("Dock here", Action::Dock)] {
            let button = gtk::Button::with_label(label);
            button.add_css_class("inline-action");
            let weak = Rc::downgrade(self);

            button.connect_clicked(move |_| {
                if let Some(hosts) = weak.upgrade() {
                    hosts.request(id, action);
                }
            });

            actions.append(&button);
        }

        placeholder.append(&actions);

        placeholder
    }

    fn begin_shutdown(self: &Rc<Self>) {
        self.closing.set(true);

        if let Some(layout) = self.layout.borrow().clone() {
            layout.finish();
        }

        self.schedule();
    }

    pub(in crate::ui) fn shutdown(&self) {
        self.closing.set(true);

        if let Some(layout) = self.layout.borrow().clone() {
            layout.finish();
        }

        if let Some(source) = self.source.borrow_mut().take() {
            source.remove();
        }

        self.dismiss_menu();
        self.panels.dialogs.close();

        for id in PanelId::ALL {
            self.pending[id as usize].set(Pending::default());
            self.dock(id, false);
        }
    }
}

fn initial_extent(allocated: i32, fallback: i32, minimum: i32, maximum: i32) -> i32 {
    if allocated > 0 { allocated } else { fallback }.clamp(minimum, maximum)
}

fn release_focus(root: &gtk::Widget) -> Option<glib::WeakRef<gtk::Widget>> {
    let window = host_window(root)?;
    let focus = gtk::prelude::GtkWindowExt::focus(&window)?;

    if focus == *root || focus.is_ancestor(root) {
        let weak = focus.downgrade();
        gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
        Some(weak)
    } else {
        None
    }
}

fn restore_focus(focus: Option<glib::WeakRef<gtk::Widget>>) {
    if let Some(focus) = focus.and_then(|focus| focus.upgrade()) {
        focus.grab_focus();
    }
}

impl Ui {
    pub(crate) fn connect_panel_hosts(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);

        self.panel_hosts.install(move |window| {
            if let Some(ui) = weak.upgrade() {
                ui.connect_keyboard_shortcuts(window);
            }
        });

        self.panel_hosts.bind_layout(&self.layout);
        let weak = Rc::downgrade(self);

        self.layout.on_error(move |message| {
            if let Some(ui) = weak.upgrade() {
                ui.application_log
                    .record(LogLevel::Warning, "Layout", message);
            }
        });
    }

    pub(crate) fn shutdown_panel_hosts(&self) {
        self.panel_hosts.shutdown();
    }
}
