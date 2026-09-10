//! Pure wait-chain traversal and selected-lock presentation.

use super::*;

fn set_label_class(label: &gtk::Label, class: &str, enabled: bool) {
    if enabled {
        label.add_css_class(class);
    } else {
        label.remove_css_class(class);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ChainRow {
    pub edge: LockDependency,
    pub cycle: bool,
}

impl ChainRow {
    pub(super) fn key(&self) -> WaitKey {
        WaitKey {
            tid: self.edge.waiter_tid,
            address: Some(self.edge.address),
        }
    }
}

pub(super) fn chain_rows(snapshot: &LockSnapshot, start: u32) -> Vec<ChainRow> {
    let edges = snapshot
        .dependencies
        .iter()
        .map(|edge| (edge.waiter_tid, edge))
        .collect::<HashMap<_, _>>();

    let mut positions = HashMap::new();
    let mut rows: Vec<ChainRow> = Vec::new();
    let mut tid = start;

    while let Some(edge) = edges.get(&tid) {
        if let Some(&position) = positions.get(&tid) {
            for row in &mut rows[position..] {
                row.cycle = true;
            }

            break;
        }

        positions.insert(tid, rows.len());

        rows.push(ChainRow {
            edge: (*edge).clone(),
            cycle: false,
        });

        tid = edge.owner_tid;
    }

    rows
}

pub(super) fn flags_text(wait: &LockWait) -> String {
    let Some(flags) = wait.operation_flags else {
        return "Operation flags unavailable".into();
    };

    if wait.operation == "FUTEX_WAITV" {
        return format!("Wait-vector flags 0x{flags:x}");
    }

    let sharing = if flags & 0x80 != 0 {
        "private"
    } else {
        "process-shared"
    };

    let clock = if flags & 0x100 != 0 {
        " · realtime clock"
    } else {
        ""
    };

    format!("Flags 0x{flags:x} · {sharing}{clock}")
}

pub(super) fn relation_column() -> gtk::ColumnViewColumn {
    selection::marked_column::<ChainRow>(
        "RELATION",
        150,
        |row| {
            if row.cycle {
                "Cycle · waits for".into()
            } else {
                "waits for".into()
            }
        },
        |row| row.cycle,
    )
}

pub(super) fn owner_column() -> gtk::ColumnViewColumn {
    selection::marked_column::<LockWait>(
        "OWNER",
        145,
        |row| {
            let owner = row
                .observation
                .ownership
                .owner()
                .map_or_else(|| "unknown".into(), |tid| tid.to_string());

            if row.observation.cycle {
                format!("Cycle · {owner}")
            } else {
                owner
            }
        },
        |row| row.observation.cycle,
    )
}

impl LocksView {
    pub(super) fn render_chain(&self) {
        let selected = self.selected_key();
        let snapshot = self.snapshot.borrow();

        let mut rows = self
            .chain_root
            .get()
            .zip(snapshot.as_ref())
            .map(|(root, snapshot)| chain_rows(snapshot, root.tid))
            .unwrap_or_default();

        // A refreshed edge may no longer lead to the inspected waiter. Keep
        // that waiter selected and start its current chain instead.
        if let Some(key) = selected
            && self.chain_root.get() != Some(key)
            && !rows.iter().any(|row| row.key() == key)
        {
            self.chain_root.set(Some(key));

            rows = snapshot
                .as_ref()
                .map(|snapshot| chain_rows(snapshot, key.tid))
                .unwrap_or_default();
        }

        drop(snapshot);
        let cycles = rows.iter().filter(|row| row.cycle).count();
        let updating = self.updating.replace(true);
        self.graph_empty.set_visible(rows.is_empty());

        self.graph_empty
            .set_text(if self.selected_wait().is_some() {
                "No owner edge for this waiter. See the ownership evidence above"
            } else {
                "Select a waiter to follow its inferred dependencies"
            });

        let origin = self
            .chain_root
            .get()
            .map_or_else(String::new, |key| format!("From waiter {} · ", key.tid));

        self.graph_summary.set_text(&if cycles > 0 {
            format!("{origin}{} inferred edges · {cycles} edges form a potential wait cycle", rows.len())
        } else {
            format!("{origin}{} inferred edges · no cycle in the observed chain. Unknown owners can hide dependencies", rows.len())
        });

        set_label_class(&self.graph_summary, "status-error", cycles > 0);

        let position = rows
            .iter()
            .position(|row| Some(row.key()) == selected)
            .and_then(|position| u32::try_from(position).ok())
            .unwrap_or(gtk::INVALID_LIST_POSITION);

        replace_boxed_store_if_changed(&self.dependency_store, rows);
        self.dependency_selection.set_selected(position);
        self.updating.set(updating);
        self.render_details();
    }

    pub(super) fn render_details(&self) {
        let Some(wait) = self.selected_wait() else {
            self.detail
                .set_text("Select a waiter to inspect its lock and dependencies");

            for label in [&self.word, &self.mapping, &self.symbol, &self.evidence] {
                label.set_text("");
            }

            return;
        };

        let snapshot = self.snapshot.borrow();

        let peers = snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .waits
                    .iter()
                    .filter(|other| {
                        wait.address.is_some() && other.address == wait.address
                            || other.tid == wait.tid
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let names = peers
            .iter()
            .take(16)
            .map(|peer| format!("{} {}", peer.tid, peer.thread))
            .collect::<Vec<_>>()
            .join(", ");

        let more = if peers.len() > 16 {
            format!(" · {} more in the waiter table", peers.len() - 16)
        } else {
            String::new()
        };

        let address = wait.address.map_or_else(
            || "Address unavailable".into(),
            |address| format!("0x{address:016x}"),
        );

        self.detail.set_text(&format!(
            "{address} · {} waiter(s)\nThreads  {names}{more}",
            peers.len()
        ));

        let current = wait
            .observation
            .word
            .map_or_else(|| "unavailable".into(), |word| format!("0x{word:08x}"));

        let expected = wait.expected.map_or_else(
            || "not available / applicable".into(),
            |value| format!("0x{value:x}"),
        );

        let expected_label = if wait.operation == "FUTEX_WAITV" {
            "Vector count"
        } else {
            "Expected"
        };

        self.word.set_text(&format!(
            "Selected waiter  {} {} · {}\nWord  {current} · {expected_label}  {expected} · {}\n{}",
            wait.tid,
            wait.thread,
            wait.operation,
            flags_text(&wait),
            wait.details,
        ));

        self.mapping
            .set_text(&wait.observation.mapping.as_ref().map_or_else(
                || "Mapping unavailable in the captured snapshot".into(),
                |mapping| {
                    format!(
                        "Mapping  0x{:016x}–0x{:016x} · {} · {}",
                        mapping.start,
                        mapping.end,
                        mapping.permissions,
                        if mapping.path.is_empty() {
                            "anonymous"
                        } else {
                            &mapping.path
                        },
                    )
                },
            ));

        let symbol = wait
            .address
            .and_then(|address| self.symbols.borrow().get(&address).cloned())
            .unwrap_or_else(|| "Select a current wait address to resolve its symbol".into());

        self.symbol.set_text(&format!("Symbol  {symbol}"));

        let label = if matches!(
            wait.observation.ownership,
            crate::misc::LockOwnership::RobustCandidate(_)
        ) {
            "Inferred ownership"
        } else {
            "Ownership evidence"
        };

        self.evidence.set_text(&format!(
            "{label}  {}",
            wait.observation.ownership.explanation()
        ));
    }
}
