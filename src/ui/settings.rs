//! Preference lifecycle is separate from widgets and debugger session authority.

use super::*;
use crate::config::{
    settings::{LiveSettings, Preferences},
    settings_io::{self, Snapshot},
};

mod editor;
mod shortcut;

pub(super) struct Settings {
    path: PathBuf,
    resolver: LiveSettings,
    pub breakpoint_auto_relocate: Cell<bool>,
    preferences: RefCell<Preferences>,
    snapshot: RefCell<Option<Rc<Snapshot>>>,
    editor: RefCell<Option<Rc<editor::Editor>>>,
    ui: RefCell<std::rc::Weak<Ui>>,
    monitor: RefCell<Option<settings_io::Watch>>,
    watched_paths: RefCell<Vec<PathBuf>>,
    monitor_error: RefCell<Option<String>>,
    debounce: RefCell<Option<glib::SourceId>>,
    busy: Cell<bool>,
    reread: Cell<bool>,
    discard_on_reload: RefCell<Option<(std::rc::Weak<editor::Editor>, u64)>>,
    error: RefCell<Option<String>>,
    provider: gtk::CssProvider,
}

impl Settings {
    pub(super) fn new(config: &LaunchConfig) -> Rc<Self> {
        Rc::new(Self {
            path: config.configuration_report().active_path().to_path_buf(),
            resolver: config.live_settings.clone(),
            breakpoint_auto_relocate: Cell::new(config.breakpoint_auto_relocate),
            preferences: RefCell::new(config.preferences.clone()),
            snapshot: RefCell::new(None),
            editor: RefCell::new(None),
            ui: RefCell::new(std::rc::Weak::new()),
            monitor: RefCell::new(None),
            watched_paths: RefCell::new(Vec::new()),
            monitor_error: RefCell::new(None),
            debounce: RefCell::new(None),
            busy: Cell::new(false),
            reread: Cell::new(false),
            discard_on_reload: RefCell::new(None),
            error: RefCell::new(None),
            provider: gtk::CssProvider::new(),
        })
    }

    pub(super) fn bind(self: &Rc<Self>, ui: &Rc<Ui>) {
        self.ui.replace(Rc::downgrade(ui));

        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &self.provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
            );
        }

        self.apply(ui, &self.preferences.borrow(), true);
        self.watch(std::slice::from_ref(&self.path));
        self.reload();
    }

    fn watch(self: &Rc<Self>, paths: &[PathBuf]) {
        if *self.watched_paths.borrow() == paths && self.monitor_error.borrow().is_none() {
            return;
        }

        let weak = Rc::downgrade(self);

        match settings_io::Watch::new(paths, move || {
            if let Some(settings) = weak.upgrade() {
                settings.schedule_reload();
            }
        }) {
            Ok(watch) => {
                self.monitor.replace(Some(watch));
                self.watched_paths.replace(paths.to_vec());
                self.monitor_error.borrow_mut().take();
            }
            Err(error) => {
                self.monitor_error.replace(Some(error));
            }
        }
    }

    pub(super) fn present(self: &Rc<Self>, ui: &Ui, diagnostics: bool) {
        let existing = self.editor.borrow().clone();

        if let Some(editor) = existing {
            editor.present(diagnostics);
            return;
        }

        let editor = editor::Editor::new(self, ui);
        self.editor.replace(Some(Rc::clone(&editor)));

        if let Some(snapshot) = self.snapshot.borrow().as_ref() {
            editor.populate(snapshot, ui.configuration_report.selected_profile());
            editor.update_issues(&snapshot.document);
        }

        if let Some(error) = self.error.borrow().as_deref() {
            editor.status(error, true);
        }

        editor.present(diagnostics);
        self.reload();
    }

    fn schedule_reload(self: &Rc<Self>) {
        if let Some(source) = self.debounce.borrow_mut().take() {
            source.remove();
        }

        let weak = Rc::downgrade(self);

        let source = glib::timeout_add_local_once(Duration::from_millis(200), move || {
            if let Some(settings) = weak.upgrade() {
                settings.debounce.borrow_mut().take();
                settings.reload();
            }
        });

        self.debounce.replace(Some(source));
    }

    fn reload(self: &Rc<Self>) {
        if self.busy.replace(true) {
            self.reread.set(true);
            return;
        }

        let path = self.path.clone();
        let weak = Rc::downgrade(self);

        glib::spawn_future_local(async move {
            let result = settings_io::read(&path).await;
            let Some(settings) = weak.upgrade() else {
                return;
            };

            match result {
                Ok(snapshot) => settings.accept(Rc::new(snapshot)),
                Err(error) => {
                    settings.discard_on_reload.borrow_mut().take();
                    settings.fail(&error);
                }
            }

            settings.finish_io();
        });
    }

    fn accept(self: &Rc<Self>, snapshot: Rc<Snapshot>) {
        self.watch(&snapshot.watch_paths);
        let file_changed = self
            .snapshot
            .borrow()
            .as_ref()
            .is_none_or(|old| old.document.text != snapshot.document.text);

        let reload_request = self.discard_on_reload.borrow_mut().take();
        let mut live_error = None;

        if let Some(ui) = self.ui.borrow().upgrade() {
            match self.resolver.resolve(&snapshot.document) {
                Ok((preferences, relocate)) => {
                    self.breakpoint_auto_relocate.set(relocate);

                    if *self.preferences.borrow() != preferences {
                        self.apply(&ui, &preferences, false);
                        self.preferences.replace(preferences);
                    }
                }
                Err(error) => live_error = Some(error),
            }
        }

        let error = live_error.or_else(|| self.monitor_error.borrow().clone());
        let recovered = error.is_none() && self.error.borrow().is_some();

        if let Some(editor) = self.editor.borrow().as_ref() {
            if file_changed {
                editor.update_issues(&snapshot.document);
            }

            // Reload may only discard the draft from the editor that requested it.
            // Edits made during I/O, or in a reopened window, must survive the response.
            let requested_revision = reload_request
                .and_then(|(owner, revision)| owner.upgrade().map(|owner| (owner, revision)))
                .filter(|(owner, _)| Rc::ptr_eq(owner, editor))
                .map(|(_, revision)| revision);

            let discard = requested_revision == Some(editor.revision());
            let changed = !editor.is_current(&snapshot.document);

            if editor.is_dirty() && !discard {
                if changed {
                    editor.status("The file changed outside this editor. Reload to discard your draft before saving", true);
                } else if requested_revision.is_some() {
                    editor.status("Reload finished. Edits made while loading were kept", false);
                }
            } else if changed || discard || recovered {
                let profile = editor.profile();
                editor.populate(&snapshot, profile.as_deref());
            }
        }

        self.snapshot.replace(Some(snapshot));

        if let Some(error) = error.as_deref() {
            self.fail(error);
        } else {
            self.error.borrow_mut().take();

            if let Some(ui) = self.ui.borrow().upgrade() {
                ui.configuration_button
                    .remove_css_class("configuration-warning");
            }
        }
    }

    fn save(self: &Rc<Self>, editor: &Rc<editor::Editor>) {
        if self.busy.get() {
            editor.status(
                "Configuration I/O is in progress. Try Apply again when it completes",
                false,
            );
            return;
        }

        let Some(snapshot) = self.snapshot.borrow().clone() else {
            return;
        };

        let text = match editor.patch(&snapshot.document) {
            Ok(Some(text)) => text,
            Ok(None) => return,
            Err(error) => {
                editor.status(&error, true);
                return;
            }
        };

        self.busy.set(true);
        editor.set_saving(true);
        let weak = Rc::downgrade(self);
        let weak_editor = Rc::downgrade(editor);
        let path = self.path.clone();

        glib::spawn_future_local(async move {
            let result = settings_io::write(&path, &snapshot, text).await;
            let refreshed = settings_io::read(&path).await;
            let Some(settings) = weak.upgrade() else {
                return;
            };

            if let Some(editor) = weak_editor.upgrade() {
                if result.is_ok() {
                    editor.mark_saved();
                }

                match refreshed {
                    Ok(snapshot) => settings.accept(Rc::new(snapshot)),
                    Err(error) => settings.fail(&error),
                }

                editor.set_saving(false);
            }

            if let Err(error) = result {
                settings.fail(&error);
            }

            settings.finish_io();
        });
    }

    fn finish_io(self: &Rc<Self>) {
        self.busy.set(false);

        if self.reread.replace(false) {
            self.reload();
        }
    }

    fn fail(&self, message: &str) {
        if self.error.borrow().as_deref() != Some(message) {
            if let Some(ui) = self.ui.borrow().upgrade() {
                ui.configuration_button
                    .add_css_class("configuration-warning");

                ui.application_log
                    .record(LogLevel::Warning, "Settings", message);
            }

            self.error.replace(Some(message.into()));
        }

        if let Some(editor) = self.editor.borrow().as_ref() {
            editor.status(message, true);
        }
    }

    fn apply(&self, ui: &Ui, preferences: &Preferences, initial: bool) {
        use crate::config::settings::{CursorBlink, CursorShape};

        let previous = self.preferences.borrow();
        let font_changed = initial || previous.source_font != preferences.source_font;

        ui.variable_presentation
            .set_format(preferences.integer_display);
        ui.application_log
            .apply_preferences(&previous, preferences, initial);
        ui.apply_instruction_preferences(&previous, preferences, initial);

        if initial || previous.keybindings != preferences.keybindings {
            ui.update_keybinding_hints(&preferences.keybindings);
        }

        if font_changed {
            self.provider
                .load_from_string(&source_font_css(&preferences.source_font));
        }

        if initial || previous.terminal_font != preferences.terminal_font {
            ui.terminal
                .set_font(Some(&pango::FontDescription::from_string(
                    &preferences.terminal_font,
                )));
        }

        if initial || previous.terminal_scrollback != preferences.terminal_scrollback {
            ui.terminal
                .set_scrollback_lines(i64::from(preferences.terminal_scrollback));
        }

        if initial || previous.terminal_cursor_shape != preferences.terminal_cursor_shape {
            ui.terminal
                .set_cursor_shape(match preferences.terminal_cursor_shape {
                    CursorShape::Block => vte4::CursorShape::Block,
                    CursorShape::Ibeam => vte4::CursorShape::Ibeam,
                    CursorShape::Underline => vte4::CursorShape::Underline,
                });
        }

        if initial || previous.terminal_cursor_blink != preferences.terminal_cursor_blink {
            ui.terminal
                .set_cursor_blink_mode(match preferences.terminal_cursor_blink {
                    CursorBlink::System => vte4::CursorBlinkMode::System,
                    CursorBlink::On => vte4::CursorBlinkMode::On,
                    CursorBlink::Off => vte4::CursorBlinkMode::Off,
                });
        }

        if initial || previous.terminal_scroll_output != preferences.terminal_scroll_output {
            ui.terminal
                .set_scroll_on_output(preferences.terminal_scroll_output);
        }

        if initial || previous.terminal_scroll_typing != preferences.terminal_scroll_typing {
            ui.terminal
                .set_scroll_on_keystroke(preferences.terminal_scroll_typing);
        }

        if font_changed
            || previous.source_tab_width != preferences.source_tab_width
            || previous.source_wrap != preferences.source_wrap
            || previous.source_highlight_line != preferences.source_highlight_line
        {
            for document in ui.source_documents.borrow().iter() {
                apply_source(&document.view, preferences);

                if font_changed {
                    document.breakpoint_renderer.queue_resize();
                }
            }
        }
    }

    pub(super) fn apply_source(&self, view: &sourceview5::View) {
        apply_source(view, &self.preferences.borrow());
    }

    pub(super) fn assembly_syntax(&self) -> crate::config::settings::AssemblySyntax {
        self.preferences.borrow().assembly_syntax
    }

    pub(super) fn shortcut_action(
        &self,
        event: &gtk::gdk::KeyEvent,
    ) -> crate::config::keybindings::ShortcutMatch {
        self.preferences.borrow().keybindings.matching(event)
    }
}

fn source_font_css(description: &str) -> String {
    use glib::translate::IntoGlib;

    let font = pango::FontDescription::from_string(description);
    let family = font.family().unwrap_or_else(|| "Monospace".into());
    let family = family
        .split(',')
        .map(|name| {
            format!(
                "\"{}\"",
                name.trim().replace('\\', "\\\\").replace('"', "\\\"")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");

    let size = f64::from(font.size()) / f64::from(pango::SCALE);
    let unit = if font.is_size_absolute() { "px" } else { "pt" };
    let style = match font.style() {
        pango::Style::Italic => "italic",
        pango::Style::Oblique => "oblique",
        _ => "normal",
    };

    let stretch = match font.stretch() {
        pango::Stretch::UltraCondensed => "ultra-condensed",
        pango::Stretch::ExtraCondensed => "extra-condensed",
        pango::Stretch::Condensed => "condensed",
        pango::Stretch::SemiCondensed => "semi-condensed",
        pango::Stretch::SemiExpanded => "semi-expanded",
        pango::Stretch::Expanded => "expanded",
        pango::Stretch::ExtraExpanded => "extra-expanded",
        pango::Stretch::UltraExpanded => "ultra-expanded",
        _ => "normal",
    };

    format!(
        ".source-preferences, .source-preferences * {{ font-family: {family}; font-size: {size}{unit}; font-weight: {}; font-style: {style}; font-stretch: {stretch}; }}",
        font.weight().into_glib(),
    )
}

impl Drop for Settings {
    fn drop(&mut self) {
        if let Some(source) = self.debounce.get_mut().take() {
            source.remove();
        }

        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_remove_provider_for_display(&display, &self.provider);
        }
    }
}

fn apply_source(view: &sourceview5::View, preferences: &Preferences) {
    if view.tab_width() != preferences.source_tab_width {
        view.set_tab_width(preferences.source_tab_width);
    }

    if view.is_highlight_current_line() != preferences.source_highlight_line {
        view.set_highlight_current_line(preferences.source_highlight_line);
    }

    let wrap = if preferences.source_wrap {
        gtk::WrapMode::WordChar
    } else {
        gtk::WrapMode::None
    };

    if view.wrap_mode() != wrap {
        view.set_wrap_mode(wrap);
    }
}
