use super::*;
use hosting::Action;

impl Hosts {
    pub(in crate::ui) fn menu_button(self: &Rc<Self>) -> gtk::MenuButton {
        let button = gtk::MenuButton::new();
        button.set_child(Some(&components::icon_label(
            "window-new-symbolic",
            "Panels",
        )));

        button.set_tooltip_text(Some("Show, pop out, or dock workspace panels"));
        button.add_css_class("workspace-panel-menu");
        let weak = Rc::downgrade(self);

        button.set_create_popup_func(move |button| {
            if let Some(hosts) = weak.upgrade() {
                button.set_popover(Some(&hosts.overview_menu()));
            }
        });

        button
    }

    fn overview_menu(self: &Rc<Self>) -> gtk::Popover {
        let (popover, menu) = build_context_menu();
        let mut previous_group = None;

        for id in PanelId::ALL {
            let Some(root) = self.panels.root(id) else {
                continue;
            };

            let group = id.spec().group;

            if previous_group != Some(group) {
                menu.append(&components::section_title(group.title()));
                previous_group = Some(group);
            }

            let floating = self.floating_window(id).is_some();
            let detail = if floating { "Show window" } else { "Pop out" };
            let action = if floating {
                Action::Show
            } else {
                Action::PopOut
            };

            let button = self.menu_action(&popover, id, action, id.title(), Some(detail));
            button.set_sensitive(root.get_visible());
            menu.append(&button);
        }

        menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let dock_all = context_menu_action("Dock all panels");
        dock_all.set_sensitive(
            PanelId::ALL
                .into_iter()
                .any(|id| self.floating_window(id).is_some()),
        );

        let weak = Rc::downgrade(self);
        let popup = popover.downgrade();

        dock_all.connect_clicked(move |_| {
            if let Some(popup) = popup.upgrade() {
                popup.popdown();
            }

            if let Some(hosts) = weak.upgrade() {
                hosts.dock_all();
            }
        });

        menu.append(&dock_all);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .overlay_scrolling(false)
            .max_content_height(560)
            .propagate_natural_height(true)
            .build();

        // Replace the parent before placing the menu into its bounded viewport.
        popover.set_child(None::<&gtk::Widget>);
        scroll.set_child(Some(&menu));
        popover.set_child(Some(&scroll));

        popover
    }

    pub(in crate::ui) fn settings_controls(self: &Rc<Self>) -> gtk::Box {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.set_vexpand(true);
        let header = components::control_row();
        header.add_css_class("settings-heading");
        let title = components::section_title("WORKSPACE PANELS");
        title.set_hexpand(true);
        header.append(&title);
        let dock_all = gtk::Button::with_label("Dock all panels");
        dock_all.add_css_class("inline-action");
        header.append(&dock_all);
        content.append(&header);

        if let Some(layout) = self.layout.borrow().as_ref() {
            content.append(&layout.status_note());
        }

        let weak = Rc::downgrade(self);

        dock_all.connect_clicked(move |_| {
            if let Some(hosts) = weak.upgrade() {
                hosts.dock_all();
            }
        });

        let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let states = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        let actions = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        let mut controls = Vec::new();

        for id in std::iter::once(PanelId::RightPane).chain(
            PanelId::ALL
                .into_iter()
                .filter(|id| *id != PanelId::RightPane),
        ) {
            if self.panels.root(id).is_none() {
                continue;
            }

            let row = components::control_row();
            row.add_css_class("settings-row");
            row.add_css_class("workspace-panel-row");
            let name = gtk::Label::new(Some(id.title()));
            name.set_xalign(0.0);
            name.set_hexpand(true);
            row.append(&name);
            let state = gtk::Label::new(None);
            state.add_css_class("muted");
            state.set_xalign(0.0);
            states.add_widget(&state);
            row.append(&state);
            let show = gtk::Button::with_label("Show");
            show.add_css_class("inline-action");
            show.set_tooltip_text(Some("Show this panel in its current window"));
            row.append(&show);
            let toggle = gtk::Button::with_label("Pop out");
            toggle.add_css_class("inline-action");
            actions.add_widget(&toggle);
            row.append(&toggle);
            rows.append(&row);
            let weak = Rc::downgrade(self);

            show.connect_clicked(move |_| {
                if let Some(hosts) = weak.upgrade() {
                    hosts.request(id, Action::Show);
                }
            });

            let weak = Rc::downgrade(self);

            toggle.connect_clicked(move |_| {
                if let Some(hosts) = weak.upgrade() {
                    let action = if hosts.floating_window(id).is_some() {
                        Action::Dock
                    } else {
                        Action::PopOut
                    };

                    hosts.request(id, action);
                }
            });

            controls.push((id, state, show, toggle));
        }

        let scroll = gtk::ScrolledWindow::builder()
            .child(&rows)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .overlay_scrolling(false)
            .build();

        content.append(&scroll);

        self.observe(&content, move |hosts| {
            let mut any_floating = false;

            for (id, state, show, toggle) in &controls {
                let floating = hosts.floating_window(*id).is_some();
                let visible = hosts
                    .panels
                    .root(*id)
                    .is_some_and(|root| root.get_visible());

                let container = hosts
                    .panels
                    .home_container(*id)
                    .filter(|container| hosts.floating_window(*container).is_some())
                    .map(|container| format!("In detached {}", container.title().to_lowercase()));

                let label = if floating {
                    if visible {
                        "Detached"
                    } else {
                        "Detached (hidden)"
                    }
                } else if !visible {
                    "Hidden"
                } else if let Some(container) = &container {
                    container
                } else {
                    "Docked"
                };

                set_label_text(state, label);
                let action = if floating { "Dock" } else { "Pop out" };

                if toggle.label().as_deref() != Some(action) {
                    toggle.set_label(action);
                }

                toggle.set_sensitive(hosts.accepting_actions() && (visible || floating));
                show.set_sensitive(hosts.accepting_actions() && visible);
                any_floating |= floating;
            }

            dock_all.set_sensitive(hosts.accepting_actions() && any_floating);
        });

        content
    }

    fn menu_action(
        self: &Rc<Self>,
        popover: &gtk::Popover,
        id: PanelId,
        action: Action,
        label: &str,
        detail: Option<&str>,
    ) -> gtk::Button {
        let button = components::menu_action(label, detail);
        button.add_css_class("context-menu-action");
        let weak = Rc::downgrade(self);
        let popup = popover.downgrade();

        button.connect_clicked(move |_| {
            if let Some(popup) = popup.upgrade() {
                popup.popdown();
            }

            if let Some(hosts) = weak.upgrade() {
                hosts.request(id, action);
            }
        });

        button
    }

    pub(super) fn install_tab_menus(self: &Rc<Self>) {
        for id in PanelId::ALL {
            let Some(slot) = self.panels.slot(id) else {
                continue;
            };

            let Some(notebook) = slot
                .ancestor(gtk::Notebook::static_type())
                .and_downcast::<gtk::Notebook>()
            else {
                continue;
            };

            let Some(label) = notebook.tab_label(slot) else {
                continue;
            };

            let gesture = gtk::GestureClick::new();
            gesture.set_button(3);
            let weak = Rc::downgrade(self);

            gesture.connect_pressed(move |gesture, _, x, y| {
                if let (Some(hosts), Some(widget)) = (weak.upgrade(), gesture.widget()) {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    hosts.show_tab_menu(&widget, id, x, y);
                }
            });

            label.add_controller(gesture);
        }
    }

    fn show_tab_menu(self: &Rc<Self>, widget: &gtk::Widget, id: PanelId, x: f64, y: f64) {
        self.dismiss_menu();
        let (popover, menu) = build_context_menu();

        if self.floating_window(id).is_some() {
            menu.append(&self.menu_action(&popover, id, Action::Show, "Show window", None));
            menu.append(&self.menu_action(&popover, id, Action::Dock, "Dock here", None));
        } else {
            menu.append(&self.menu_action(&popover, id, Action::PopOut, "Pop out panel", None));
        }

        if let Some(container) = self.panels.home_container(id) {
            menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            let floating = self.floating_window(container).is_some();
            let (action, label) = if floating {
                (
                    Action::Dock,
                    format!("Dock {}", container.title().to_lowercase()),
                )
            } else {
                (
                    Action::PopOut,
                    format!("Pop out {}", container.title().to_lowercase()),
                )
            };

            menu.append(&self.menu_action(&popover, container, action, &label, None));
        }

        popover.set_parent(widget);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let weak = Rc::downgrade(self);

        popover.connect_closed(move |popover| {
            if let Some(hosts) = weak.upgrade() {
                let current = hosts.menu.borrow().as_ref() == Some(popover);

                if current {
                    hosts.menu.borrow_mut().take();
                }
            }

            if let Some(parent) = popover.parent() {
                let popover = popover.clone();

                glib::idle_add_local_once(move || {
                    if popover.parent().is_some() {
                        popover.unparent();
                    }

                    drop(parent);
                });
            }
        });

        self.menu.replace(Some(popover.clone()));
        popover.popup();
    }

    pub(super) fn dismiss_menu(&self) {
        let popup = self.menu.borrow_mut().take();

        if let Some(popup) = popup {
            popup.popdown();
        }
    }
}
