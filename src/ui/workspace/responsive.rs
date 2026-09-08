use super::*;
use gtk::subclass::prelude::*;
use std::sync::LazyLock;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ResponsiveBox {
        pub(super) content: gtk::Box,
        pub(super) compact: Cell<bool>,
        pub(super) requested: Cell<bool>,
        pub(super) pending: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ResponsiveBox {
        const NAME: &'static str = "FgdbResponsiveBox";
        type Type = super::ResponsiveBox;
        type ParentType = gtk::Widget;

        fn class_init(class: &mut Self::Class) {
            class.set_css_name("box");
            class.set_accessible_role(gtk::AccessibleRole::Group);
        }
    }

    impl ObjectImpl for ResponsiveBox {
        fn constructed(&self) {
            self.parent_constructed();
            self.content.set_orientation(gtk::Orientation::Vertical);
            self.content.set_parent(&*self.obj());
        }

        fn dispose(&self) {
            if self.content.parent().is_some() {
                self.content.unparent();
            }
        }

        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: LazyLock<Vec<glib::subclass::Signal>> = LazyLock::new(|| {
                vec![
                    glib::subclass::Signal::builder("compact-changed")
                        .param_types([bool::static_type()])
                        .build(),
                ]
            });

            SIGNALS.as_ref()
        }
    }

    impl WidgetImpl for ResponsiveBox {
        fn compute_expand(&self, hexpand: &mut bool, vexpand: &mut bool) {
            *hexpand = self.content.compute_expand(gtk::Orientation::Horizontal);
            *vexpand = self.content.compute_expand(gtk::Orientation::Vertical);
        }

        fn request_mode(&self) -> gtk::SizeRequestMode {
            self.content.request_mode()
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            self.content.measure(orientation, for_size)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().snapshot_child(&self.content, snapshot);
        }

        fn focus(&self, direction: gtk::DirectionType) -> bool {
            self.content.child_focus(direction)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.content.allocate(width, height, baseline, None);

            if width <= 0 {
                return;
            }

            self.requested.set(width < 620);

            if self.compact.get() == self.requested.get() || self.pending.replace(true) {
                return;
            }

            let weak = self.obj().downgrade();

            // Changing child visibility during allocation can invalidate GTK's
            // layout stack. Apply only breakpoint transitions after allocation.
            glib::idle_add_local_once(move || {
                let Some(root) = weak.upgrade() else {
                    return;
                };

                let state = root.imp();
                state.pending.set(false);
                let compact = state.requested.get();

                if state.compact.replace(compact) == compact {
                    return;
                }

                if compact {
                    root.add_css_class("inspector-compact");
                } else {
                    root.remove_css_class("inspector-compact");
                }

                root.emit_by_name::<()>("compact-changed", &[&compact]);
            });
        }
    }
}

glib::wrapper! {
    pub struct ResponsiveBox(ObjectSubclass<imp::ResponsiveBox>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ResponsiveBox {
    pub(in crate::ui) fn new() -> Self {
        glib::Object::new()
    }

    pub(in crate::ui) fn append(&self, child: &impl IsA<gtk::Widget>) {
        self.imp().content.append(child);
    }

    pub(in crate::ui) fn connect_compact_changed(&self, changed: impl Fn(bool) + 'static) {
        self.connect_closure(
            "compact-changed",
            false,
            glib::closure_local!(move |_root: Self, compact: bool| {
                changed(compact);
            }),
        );
    }

    pub(in crate::ui) fn bind_navigation(&self, wide: &gtk::Box, compact: &gtk::Box) {
        let wide = wide.downgrade();
        let compact = compact.downgrade();

        self.connect_compact_changed(move |is_compact| {
            if let (Some(wide), Some(compact)) = (wide.upgrade(), compact.upgrade()) {
                wide.set_visible(!is_compact);
                compact.set_visible(is_compact);
            }
        });
    }
}
