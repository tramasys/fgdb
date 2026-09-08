//! Weak dialog ownership follows panels without tying their lifetime to a host.

use super::*;
use crate::ui::lifecycle::SignalSubscription;

#[derive(Clone, Copy)]
enum Origin {
    Application,
    Panel(PanelId),
}

struct Entry {
    window: glib::WeakRef<gtk::Window>,
    origin: Origin,
}

#[derive(Default)]
pub(super) struct Dialogs {
    entries: RefCell<Vec<Entry>>,
    subscription: RefCell<Option<SignalSubscription>>,
    closed: Cell<bool>,
}

impl Dialogs {
    pub(super) fn install(self: &Rc<Self>, main: &gtk::ApplicationWindow) {
        if self.closed.get() || self.subscription.borrow().is_some() {
            return;
        }

        let windows = gtk::Window::toplevels();
        let weak = Rc::downgrade(self);
        let main = main.downgrade();

        let observe = move |model: &gio::ListModel, position, added| {
            for index in position..position + added {
                let Some(window) = model.item(index).and_downcast::<gtk::Window>() else {
                    continue;
                };

                let weak = weak.clone();
                let main = main.clone();

                let register = move |window: &gtk::Window| {
                    let (Some(dialogs), Some(main), Some(mut parent)) =
                        (weak.upgrade(), main.upgrade(), window.transient_for())
                    else {
                        return;
                    };

                    for _ in 0..16 {
                        if &parent == main.upcast_ref::<gtk::Window>()
                            || parent
                                .application()
                                .is_some_and(|app| Some(app) == main.application())
                        {
                            dialogs.register(window, None);
                            break;
                        }

                        let Some(next) = parent.transient_for() else {
                            break;
                        };

                        parent = next;
                    }
                };

                register(&window);
                window.connect_transient_for_notify(register);
            }
        };

        observe(&windows, 0, windows.n_items());

        let subscription = windows.connect_items_changed(move |windows, position, _, added| {
            observe(windows, position, added);
        });

        self.subscription
            .replace(Some(SignalSubscription::new(&windows, subscription)));
    }

    pub(super) fn register(&self, window: &gtk::Window, panel: Option<PanelId>) {
        if self.closed.get() {
            return;
        }

        let mut entries = self.entries.borrow_mut();
        entries.retain(|entry| entry.window.upgrade().is_some());

        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.window.upgrade().as_ref() == Some(window))
        {
            if let Some(panel) = panel {
                entry.origin = Origin::Panel(panel);
            }

            return;
        }

        entries.push(Entry {
            window: window.downgrade(),
            origin: panel.map_or(Origin::Application, Origin::Panel),
        });
    }

    pub(super) fn rehost(&self, panels: &Panels, retired: Option<(&gtk::Window, &gtk::Window)>) {
        let windows = self.snapshot();

        for (window, origin) in windows {
            let parent = match origin {
                Origin::Panel(id) => panels.window(id),
                Origin::Application => retired.and_then(|(old, new)| {
                    (window.transient_for().as_ref() == Some(old)).then(|| new.clone())
                }),
            };

            if let Some(parent) = parent
                && window != parent
                && window.transient_for().as_ref() != Some(&parent)
            {
                window.set_transient_for(Some(&parent));
            }
        }
    }

    pub(super) fn close(&self) {
        self.closed.set(true);
        self.subscription.borrow_mut().take();

        for (window, _) in self.snapshot() {
            window.destroy();
        }

        self.entries.borrow_mut().clear();
    }

    pub(super) fn retire_host(&self, old: &gtk::Window, fallback: &gtk::Window) {
        // An externally destroyed host cannot wait for deferred docking before
        // rescuing destroy-with-parent dialogs. No panel widget moves here.
        for (window, _) in self.snapshot() {
            if window != *fallback && window.transient_for().as_ref() == Some(old) {
                window.set_transient_for(Some(fallback));
            }
        }
    }

    fn snapshot(&self) -> Vec<(gtk::Window, Origin)> {
        let mut entries = self.entries.borrow_mut();
        let mut windows = Vec::with_capacity(entries.len());

        entries.retain(|entry| {
            if let Some(window) = entry.window.upgrade() {
                windows.push((window, entry.origin));
                true
            } else {
                false
            }
        });

        windows
    }
}
