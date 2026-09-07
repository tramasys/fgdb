use super::*;
use crate::config::keybindings::{Action, Shortcut};

pub(super) struct ShortcutInput {
    pub root: gtk::Box,
    pub button: gtk::Button,
}

impl ShortcutInput {
    pub(super) fn new(action: Action) -> Self {
        let root = components::control_row();
        root.set_hexpand(false);
        root.set_size_request(260, -1);
        let button = gtk::Button::with_label("Disabled");
        button.set_hexpand(true);
        button.set_tooltip_text(Some("Record a new shortcut"));
        root.append(&button);
        let clear = gtk::Button::from_icon_name("edit-clear-symbolic");
        clear.set_tooltip_text(Some("Disable this shortcut"));
        root.append(&clear);
        let reset = gtk::Button::from_icon_name("edit-undo-symbolic");
        reset.set_tooltip_text(Some(&format!(
            "Restore built-in binding: {}",
            action.default_binding()
        )));
        root.append(&reset);
        let weak = button.downgrade();

        clear.connect_clicked(move |_| {
            if let Some(button) = weak.upgrade() {
                button.set_label("Disabled");
            }
        });

        let weak = button.downgrade();

        reset.connect_clicked(move |_| {
            if let Some(button) = weak.upgrade() {
                button.set_label(action.default_binding());
            }
        });

        button.connect_clicked(move |button| record(button, action));
        Self { root, button }
    }
}

fn record(button: &gtk::Button, action: Action) {
    let Some(parent) = button.root().and_downcast::<gtk::Window>() else {
        return;
    };

    let dialog = gtk::Window::builder()
        .title("fgdb shortcut")
        .transient_for(&parent)
        .destroy_with_parent(true)
        .modal(true)
        .default_width(440)
        .resizable(false)
        .css_classes(["settings-dialog"])
        .build();

    let content = gtk::Box::new(gtk::Orientation::Vertical, components::CONTENT_INSET);
    components::inset(&content, components::DIALOG_INSET);
    let title = gtk::Label::builder()
        .label(action.title())
        .xalign(0.0)
        .wrap(true)
        .css_classes(["title-2"])
        .build();
    content.append(&title);
    let message = gtk::Label::builder()
        .label(
            "Press a shortcut. Use a function key or include Ctrl, Alt, or Super. Escape cancels",
        )
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .max_width_chars(54)
        .css_classes(["muted"])
        .build();

    content.append(&message);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.set_halign(gtk::Align::End);
    let weak = dialog.downgrade();

    cancel.connect_clicked(move |_| {
        if let Some(dialog) = weak.upgrade() {
            dialog.close();
        }
    });

    content.append(&cancel);
    dialog.set_child(Some(&content));
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_dialog = dialog.downgrade();
    let weak_button = button.downgrade();

    keys.connect_key_pressed(move |controller, key, _, modifiers| {
        let Some(dialog) = weak_dialog.upgrade() else {
            return glib::Propagation::Proceed;
        };

        if key == gtk::gdk::Key::Escape
            && (modifiers - gtk::gdk::ModifierType::LOCK_MASK).is_empty()
        {
            dialog.close();
            return glib::Propagation::Stop;
        }

        let Some(event) = controller
            .current_event()
            .and_then(|event| event.downcast::<gtk::gdk::KeyEvent>().ok())
        else {
            return glib::Propagation::Stop;
        };

        if event.is_modifier() {
            return glib::Propagation::Stop;
        }

        match Shortcut::from_event(&event) {
            Ok(shortcut) => {
                if let Some(button) = weak_button.upgrade() {
                    button.set_label(&shortcut.display());
                }

                dialog.close();
            }
            Err(error) => {
                message.set_text(&error);
                message.add_css_class("configuration-error");
            }
        }

        glib::Propagation::Stop
    });

    dialog.add_controller(keys);
    dialog.present();
}
