//! Shortcut definitions and validation work before GTK initialization too.

use std::{collections::BTreeMap, sync::OnceLock};

use gtk::gdk::{Key, KeyEvent, KeyMatch, ModifierType};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    Execution,
    Source,
    Workspace,
}

macro_rules! actions {
    ($($action:ident => ($key:literal, $title:literal, $help:literal, $default:literal, $scope:ident)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        pub(crate) enum Action { $($action),+ }

        impl Action {
            pub(crate) const ALL: &[Self] = &[$(Self::$action),+];

            pub(crate) fn config_key(self) -> &'static str {
                match self { $(Self::$action => $key),+ }
            }

            pub(crate) fn title(self) -> &'static str {
                match self { $(Self::$action => $title),+ }
            }

            pub(crate) fn help(self) -> &'static str {
                match self { $(Self::$action => $help),+ }
            }

            pub(crate) fn default_binding(self) -> &'static str {
                match self { $(Self::$action => $default),+ }
            }

            pub(crate) fn scope(self) -> Scope {
                match self { $(Self::$action => Scope::$scope),+ }
            }
        }
    };
}

actions! {
    Run => ("keybind_run", "Run or continue", "Start or continue the inferior", "F5", Execution),
    Pause => ("keybind_pause", "Pause", "Interrupt the inferior", "F6", Execution),
    Next => ("keybind_next", "Next source line", "Step over a source line in the selected direction", "F10", Execution),
    Step => ("keybind_step", "Step into source", "Step into a source line in the selected direction", "F11", Execution),
    NextInstruction => ("keybind_nexti", "Next instruction", "Step one instruction in the selected direction, stepping over calls", "Ctrl+F10", Execution),
    StepInstruction => ("keybind_stepi", "Step into instruction", "Step one instruction in the selected direction, stepping into calls", "Ctrl+F11", Execution),
    Finish => ("keybind_finish", "Finish function", "Finish the current function, or reverse to its entry", "Shift+F11", Execution),
    Resynchronize => ("keybind_refresh", "Refresh debugger state", "Re-read state after terminal commands or external debugger changes", "Ctrl+Shift+R", Workspace),
    Terminal => ("keybind_terminal", "Toggle terminal", "Show or hide the interactive GDB terminal", "Ctrl+grave", Workspace),
    Log => ("keybind_log", "Toggle application log", "Show or hide fgdb application messages", "Disabled", Workspace),
    Settings => ("keybind_settings", "Open Settings", "Change preferences and inspect configuration", "Disabled", Workspace),
    DebugData => ("keybind_debug_data", "Open Debug data", "Inspect symbols, sources, and pretty printers", "Disabled", Workspace),
    SourceBack => ("keybind_source_back", "Previous source location", "Back in source navigation history", "Alt+Left", Source),
    SourceForward => ("keybind_source_forward", "Next source location", "Forward in source navigation history", "Alt+Right", Source),
    QuickOpen => ("keybind_quick_open", "Quick open source", "Find a loaded or project source file", "Ctrl+P", Source),
    OpenFile => ("keybind_open_file", "Open source file", "Open one or more source files from disk in editor tabs", "Ctrl+O", Source),
    Find => ("keybind_find", "Find in source", "Find text in the current source file", "Ctrl+F", Source),
    GoToLine => ("keybind_go_to_line", "Go to source line", "Navigate to a line in the current source file", "Ctrl+G", Source),
    Symbols => ("keybind_symbols", "Find functions and symbols", "Search symbols for source navigation", "Ctrl+Shift+O", Source),
    LoadedSources => ("keybind_loaded_sources", "Search loaded source files", "Search the source files known to the debugger", "Disabled", Source),
    SourceTree => ("keybind_source_tree", "Search source tree", "Search text across the project source tree", "Ctrl+Shift+F", Source),
    ReopenSource => ("keybind_reopen_source", "Reopen closed source tab", "Reopen the most recently closed source tab", "Ctrl+Shift+T", Source),
}

impl Action {
    pub(crate) fn from_config_key(key: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|action| action.config_key() == key)
    }
}

const MODIFIERS: ModifierType = ModifierType::CONTROL_MASK
    .union(ModifierType::SHIFT_MASK)
    .union(ModifierType::ALT_MASK)
    .union(ModifierType::SUPER_MASK);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shortcut {
    key: Key,
    modifiers: ModifierType,
}

impl Shortcut {
    pub(crate) fn parse(value: &str) -> Result<Option<Self>, String> {
        if value.len() > 128 || value.chars().any(char::is_control) {
            return Err(String::from(
                "A shortcut must be at most 128 characters without control characters",
            ));
        }

        let value = value.trim();

        if value.is_empty()
            || value.eq_ignore_ascii_case("disabled")
            || value.eq_ignore_ascii_case("none")
        {
            return Ok(None);
        }

        let mut parts = value.rsplit('+');
        let name = parts.next().unwrap_or_default().trim();
        let key = Key::from_name(if name == "`" { "grave" } else { name }).ok_or_else(|| {
            format!("Unknown key '{name}'. Use names such as F5, Left, or Ctrl+K")
        })?;
        let mut modifiers = ModifierType::empty();

        for part in parts {
            let flag = match part.trim().to_ascii_lowercase().as_str() {
                "ctrl" | "control" => ModifierType::CONTROL_MASK,
                "shift" => ModifierType::SHIFT_MASK,
                "alt" => ModifierType::ALT_MASK,
                "super" => ModifierType::SUPER_MASK,
                _ => {
                    return Err(format!(
                        "Unknown modifier '{part}'. Use Ctrl, Shift, Alt, or Super"
                    ));
                }
            };

            if modifiers.contains(flag) {
                return Err(String::from("A shortcut cannot repeat a modifier"));
            }

            modifiers.insert(flag);
        }

        Self::new(key, modifiers).map(Some)
    }

    fn new(key: Key, modifiers: ModifierType) -> Result<Self, String> {
        let key = key.to_lower();

        if matches!(
            key,
            Key::Control_L
                | Key::Control_R
                | Key::Shift_L
                | Key::Shift_R
                | Key::Alt_L
                | Key::Alt_R
                | Key::Super_L
                | Key::Super_R
                | Key::Meta_L
                | Key::Meta_R
                | Key::Hyper_L
                | Key::Hyper_R
                | Key::Caps_Lock
                | Key::Shift_Lock
                | Key::Num_Lock
                | Key::ISO_Level3_Shift
                | Key::Mode_switch
                | Key::VoidSymbol
        ) {
            return Err(String::from("Choose a key as well as any modifiers"));
        }

        if !(Key::F1..=Key::F35).contains(&key)
            && !modifiers.intersects(
                ModifierType::CONTROL_MASK | ModifierType::ALT_MASK | ModifierType::SUPER_MASK,
            )
        {
            return Err(String::from(
                "Use a function key or include Ctrl, Alt, or Super. Plain typing and navigation keys remain available to controls",
            ));
        }

        if modifiers.intersection(!ModifierType::SHIFT_MASK) == ModifierType::CONTROL_MASK
            && matches!(
                key,
                Key::a | Key::c | Key::v | Key::x | Key::y | Key::z | Key::Insert
            )
        {
            return Err(String::from(
                "This shortcut is reserved for text editing or the clipboard",
            ));
        }

        Ok(Self { key, modifiers })
    }

    pub(crate) fn from_event(event: &KeyEvent) -> Result<Self, String> {
        let (key, modifiers) = event
            .match_()
            .ok_or_else(|| String::from("This key cannot be used as a shortcut"))?;

        if !(modifiers - MODIFIERS - ModifierType::LOCK_MASK).is_empty() {
            return Err(String::from("Use Ctrl, Shift, Alt, or Super modifiers"));
        }

        Self::new(key, modifiers & MODIFIERS)
    }

    pub(crate) fn is_function_key(self) -> bool {
        (Key::F1..=Key::F35).contains(&self.key)
    }

    pub(crate) fn display(self) -> String {
        let mut value = String::new();

        for (flag, name) in [
            (ModifierType::CONTROL_MASK, "Ctrl+"),
            (ModifierType::ALT_MASK, "Alt+"),
            (ModifierType::SHIFT_MASK, "Shift+"),
            (ModifierType::SUPER_MASK, "Super+"),
        ] {
            if self.modifiers.contains(flag) {
                value.push_str(name);
            }
        }

        let key = if self.key.is_lower() {
            self.key.to_upper()
        } else {
            self.key
        };
        value.push_str(key.name().as_deref().unwrap_or("Unknown"));
        value
    }
}

pub(crate) type Overrides = BTreeMap<Action, Option<Shortcut>>;

pub(crate) enum ShortcutMatch {
    Unbound,
    Bound(Action, Shortcut),
    Ambiguous(Action, Action),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Bindings([Option<Shortcut>; Action::ALL.len()]);

impl Default for Bindings {
    fn default() -> Self {
        static DEFAULTS: OnceLock<Bindings> = OnceLock::new();

        DEFAULTS
            .get_or_init(|| {
                Self(std::array::from_fn(|index| {
                    Shortcut::parse(Action::ALL[index].default_binding())
                        .expect("built-in keybindings must be valid")
                }))
            })
            .clone()
    }
}

impl Bindings {
    pub(crate) fn from_overrides(overrides: &Overrides) -> Self {
        let mut bindings = Self::default();

        for (&action, &shortcut) in overrides {
            bindings.0[action as usize] = shortcut;
        }

        bindings
    }

    pub(crate) fn conflict(&self) -> Option<(Action, Action)> {
        for (index, &action) in Action::ALL.iter().enumerate() {
            if let Some(shortcut) = self.0[index]
                && let Some(&other) = Action::ALL[index + 1..]
                    .iter()
                    .find(|&&other| self.0[other as usize] == Some(shortcut))
            {
                return Some((action, other));
            }
        }

        None
    }

    pub(crate) fn value(&self, action: Action) -> String {
        self.0[action as usize].map_or_else(|| "Disabled".into(), Shortcut::display)
    }

    pub(crate) fn matching(&self, event: &KeyEvent) -> ShortcutMatch {
        let mut found = None;

        for &action in Action::ALL {
            if let Some(shortcut) = self.0[action as usize]
                && event.matches(shortcut.key, shortcut.modifiers) == KeyMatch::Exact
            {
                // Keyboard layouts can introduce aliases beyond normalized key names.
                // An ambiguous event must never choose an execution action arbitrarily.
                if let Some((other, _)) = found {
                    return ShortcutMatch::Ambiguous(other, action);
                }

                found = Some((action, shortcut));
            }
        }

        found.map_or(ShortcutMatch::Unbound, |(action, shortcut)| {
            ShortcutMatch::Bound(action, shortcut)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_round_trip_without_gtk_and_reject_unsafe_or_ambiguous_bindings() {
        let defaults = Bindings::default();
        assert_eq!(defaults.conflict(), None);

        for &action in Action::ALL {
            assert_eq!(Action::from_config_key(action.config_key()), Some(action));
            assert_eq!(
                Shortcut::parse(&defaults.value(action)).unwrap(),
                defaults.0[action as usize]
            );
        }

        assert_eq!(
            Shortcut::parse("Control+shift+k").unwrap(),
            Shortcut::parse("Ctrl+Shift+K").unwrap()
        );
        assert_eq!(
            Shortcut::parse("Ctrl+`").unwrap(),
            Shortcut::parse("Ctrl+grave").unwrap()
        );
        assert_eq!(Shortcut::parse("").unwrap(), None);
        assert_eq!(Shortcut::parse("disabled").unwrap(), None);

        for invalid in [
            "K",
            "Shift+K",
            "Escape",
            "Tab",
            "Return",
            "Super_L",
            "Ctrl+Shift_L",
            "Ctrl+Ctrl+F1",
            "Ctrl+C",
            "Ctrl+Shift+V",
            "Ctrl+Insert",
            "Meta+F1",
            "Bogus",
            "Ctrl+F1\0",
            &"x".repeat(129),
        ] {
            assert!(Shortcut::parse(invalid).is_err(), "{invalid:?}");
        }

        let mut overrides = Overrides::from([(Action::Next, Shortcut::parse("F5").unwrap())]);
        assert_eq!(
            Bindings::from_overrides(&overrides).conflict(),
            Some((Action::Run, Action::Next))
        );
        overrides.insert(Action::Run, None);
        assert_eq!(Bindings::from_overrides(&overrides).conflict(), None);
    }
}
