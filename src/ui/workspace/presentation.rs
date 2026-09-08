//! Coalesced presentation changes with host subscriptions that follow reparenting.

use super::*;
use crate::ui::lifecycle::SignalSubscription;

pub(super) fn is_presented(root: &gtk::Widget) -> bool {
    root.is_mapped() && host_window(root).is_some_and(|window| !window.is_suspended())
}

struct Presentation {
    root: glib::WeakRef<gtk::Widget>,
    host: glib::WeakRef<gtk::Window>,
    subscription: RefCell<Option<SignalSubscription>>,
    pending: Cell<bool>,
    previous: Cell<bool>,
    changed: Box<dyn Fn(bool)>,
}

impl Presentation {
    fn schedule(self: &Rc<Self>) {
        if self.pending.replace(true) {
            return;
        }

        let weak = Rc::downgrade(self);

        glib::idle_add_local_once(move || {
            if let Some(state) = weak.upgrade() {
                state.pending.set(false);
                state.update();
            }
        });
    }

    fn update(self: &Rc<Self>) {
        let Some(root) = self.root.upgrade() else {
            return;
        };

        let host = host_window(&root);

        if host != self.host.upgrade() {
            self.subscription.borrow_mut().take();
            self.host.set(host.as_ref());

            if let Some(host) = host {
                let weak = Rc::downgrade(self);

                let handler = host.connect_suspended_notify(move |_| {
                    if let Some(state) = weak.upgrade() {
                        state.schedule();
                    }
                });

                self.subscription
                    .replace(Some(SignalSubscription::new(&host, handler)));
            }
        }

        let presented = is_presented(&root);

        if self.previous.replace(presented) != presented {
            (self.changed)(presented);
        }
    }
}

/// A transient unmap/map pair during a host change produces no false refresh.
/// Neither reparenting nor a compositor notification invokes feature code inline.
pub(in crate::ui) fn connect_presentation(
    root: &impl IsA<gtk::Widget>,
    changed: impl Fn(bool) + 'static,
) {
    let state = Rc::new(Presentation {
        root: root.as_ref().downgrade(),
        host: glib::WeakRef::new(),
        subscription: RefCell::new(None),
        pending: Cell::new(false),
        previous: Cell::new(false),
        changed: Box::new(changed),
    });

    let mapped = Rc::clone(&state);
    root.connect_map(move |_| mapped.schedule());
    let unmapped = Rc::clone(&state);
    root.connect_unmap(move |_| unmapped.schedule());
    state.schedule();
}
