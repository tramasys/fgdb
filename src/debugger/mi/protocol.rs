#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MiEvent {
    Ready(GdbCapabilities),
    CapabilitiesChanged(GdbCapabilities),
    InferiorsChanged,
    InferiorStarted {
        id: String,
        pid: Option<u32>,
    },
    InferiorExited {
        id: String,
        exit_code: Option<String>,
    },
    Running {
        thread_id: Option<String>,
    },
    Stopped {
        reason: Option<String>,
        signal_name: Option<String>,
        signal_meaning: Option<String>,
        address: Option<String>,
        thread_id: Option<String>,
        group_id: Option<String>,
        frame_level: Option<u32>,
        fork_pid: Option<u32>,
        all_stopped: bool,
    },
    BreakpointsChanged,
    ThreadsChanged {
        id: Option<String>,
        group_id: Option<String>,
    },
    ThreadExited {
        id: String,
        group_id: Option<String>,
    },
    ThreadExitPrompt,
    DebuggerUnusable(String),
    LibrariesChanged {
        group_id: Option<String>,
    },
    SelectionChanged {
        thread_id: Option<String>,
        group_id: Option<String>,
        frame_level: Option<u32>,
    },
    CommandParameterChanged {
        parameter: String,
        value: Option<String>,
    },
    Error(String),
    Disconnected,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GdbCapabilities {
    pub version: Option<String>,
    pub features_known: bool,
    pub features: Vec<String>,
    pub mi_async: bool,
    pub pretty_printing: bool,
    pub rust_pretty_printing: bool,
}

impl GdbCapabilities {
    pub fn supports(&self, feature: &str) -> bool {
        !self.features_known || self.features.iter().any(|available| available == feature)
    }

    pub fn compatibility_summary(&self) -> String {
        let mut available = Vec::with_capacity(4);
        if self.mi_async {
            available.push("MI async");
        }
        if self.pretty_printing {
            available.push("pretty printers");
        }
        if self.rust_pretty_printing {
            available.push("Rust printers");
        }
        if self.features_known {
            available.push("feature list");
        }
        let support = if available.is_empty() {
            String::from("compatibility mode")
        } else {
            available.join(" · ")
        };
        if let Some(version) = self.version.as_ref() {
            format!("GDB {version} · {support}")
        } else {
            support
        }
    }

    pub(super) fn set_version_component(&mut self, component: &str, minor: bool) {
        if component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit()) {
            return;
        }
        if minor {
            if let Some(version) = self.version.as_mut() {
                version.push('.');
                version.push_str(component);
            }
        } else {
            self.version = Some(component.to_owned());
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MiRecord {
    pub token: Option<u64>,
    pub kind: char,
    pub class: String,
    pub results: Vec<MiResult>,
}

impl MiRecord {
    pub fn field(&self, name: &str) -> Option<&MiValue> {
        result_field(&self.results, name)
    }

    pub fn is_done(&self) -> bool {
        self.class == "done"
    }

    pub fn is_success(&self) -> bool {
        self.kind == '^' && matches!(self.class.as_str(), "done" | "running" | "connected")
    }

    pub fn error_message(&self) -> Option<&str> {
        self.field("msg").and_then(MiValue::as_const)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MiResult {
    pub name: String,
    pub value: MiValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MiValue {
    Const(String),
    Tuple(Vec<MiResult>),
    List(Vec<MiListItem>),
}

impl MiValue {
    pub fn as_const(&self) -> Option<&str> {
        match self {
            Self::Const(value) => Some(value),
            Self::Tuple(_) | Self::List(_) => None,
        }
    }

    pub fn as_tuple(&self) -> Option<&[MiResult]> {
        match self {
            Self::Tuple(results) => Some(results),
            Self::Const(_) | Self::List(_) => None,
        }
    }

    pub fn as_list(&self) -> Option<&[MiListItem]> {
        match self {
            Self::List(items) => Some(items),
            Self::Const(_) | Self::Tuple(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MiListItem {
    Value(MiValue),
    Result(MiResult),
}

pub fn result_field<'a>(results: &'a [MiResult], name: &str) -> Option<&'a MiValue> {
    results
        .iter()
        .find(|result| result.name == name)
        .map(|result| &result.value)
}
