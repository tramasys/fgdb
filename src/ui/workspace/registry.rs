//! One static declaration owns identity, persistence keys, labels, and refresh policy.

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Group {
    Inspectors,
    Navigation,
    Workspace,
}

impl Group {
    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Inspectors => "Inspectors",
            Self::Navigation => "Navigation",
            Self::Workspace => "Workspace",
        }
    }
}

pub(super) struct Spec {
    pub key: &'static str,
    pub title: &'static str,
    pub group: Group,
    pub refresh_stop_details: bool,
}

// Enum discriminants and ALL are generated together, so dense array indices
// cannot drift when another panel is added or the menu order changes.
macro_rules! panels {
    ($($id:ident => ($key:literal, $title:literal, $group:ident, $details:literal)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub(crate) enum PanelId { $($id),+ }

        impl PanelId {
            pub(in crate::ui) const ALL: [Self; [$(stringify!($id)),+].len()] = [$(Self::$id),+];

            pub(in crate::ui::workspace) fn spec(self) -> &'static Spec {
                match self {
                    $(Self::$id => &Spec {
                        key: $key,
                        title: $title,
                        group: Group::$group,
                        refresh_stop_details: $details,
                    }),+
                }
            }
        }
    };
}

panels! {
    Context => ("context", "Context", Inspectors, false),
    Watches => ("watches", "Watches", Inspectors, false),
    Registers => ("registers", "Registers", Inspectors, true),
    Stack => ("stack", "Stack", Inspectors, true),
    Memory => ("memory", "Memory", Inspectors, true),
    Breakpoints => ("breakpoints", "Breakpoints", Inspectors, false),
    Signals => ("signals", "Signals", Inspectors, false),
    Kernel => ("kernel", "Kernel", Inspectors, true),
    Misc => ("misc", "Misc", Inspectors, false),
    Inferiors => ("inferiors", "Inferiors", Navigation, false),
    CallStack => ("call-stack", "Call stack", Navigation, false),
    Threads => ("threads", "Threads", Navigation, false),
    Modules => ("modules", "Modules", Navigation, false),
    Sources => ("sources", "Sources", Navigation, false),
    Editor => ("editor", "Source", Workspace, false),
    Console => ("console", "Console", Workspace, false),
    RightPane => ("right-pane", "Right pane", Workspace, false),
}

impl PanelId {
    pub(in crate::ui) fn key(self) -> &'static str {
        self.spec().key
    }

    pub(in crate::ui) fn title(self) -> &'static str {
        self.spec().title
    }

    pub(in crate::ui) fn refresh_stop_details(self) -> bool {
        self.spec().refresh_stop_details
    }

    pub(in crate::ui) fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|panel| panel.key() == key)
    }
}
