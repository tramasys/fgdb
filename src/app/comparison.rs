//! Explicit snapshot requests, scoped to the original stop and comparison window.

use super::*;
use crate::debugger::comparison::{
    CapturedValue, MAX_BYTES, MAX_ROWS, SnapshotReply, ValueSnapshot,
};

pub(super) fn connect(ui: &Rc<Ui>, client: &Rc<MiClient>) {
    let weak_ui = Rc::downgrade(ui);
    let weak_client = Rc::downgrade(client);

    ui.connect_comparisons(move |variable, current, reply| {
        let requests = weak_ui
            .upgrade()
            .zip(weak_client.upgrade())
            .and_then(|(ui, client)| {
                stop_requests(
                    &weak_ui,
                    &client,
                    ui.model.current_stop_refresh_generation(),
                )
            });

        let Some(requests) = requests else {
            reply(Err("Stopped debugger connection unavailable".into()));
            return;
        };

        let request = Rc::new(Request {
            requests,
            current,
            reply: RefCell::new(Some(reply)),
        });

        request.resolve(variable);
    });
}

struct Request {
    requests: StopRequests,
    current: Rc<dyn Fn() -> bool>,
    reply: RefCell<Option<SnapshotReply>>,
}

impl Request {
    fn is_current(&self) -> bool {
        self.reply.borrow().is_some() && self.requests.is_current() && (self.current)()
    }

    fn finish(&self, result: Result<ValueSnapshot, String>) {
        let reply = self.reply.borrow_mut().take();

        if let Some(reply) = reply {
            reply(result);
        }
    }

    fn resolve(self: &Rc<Self>, variable: Variable) {
        let Some(varobj) = variable
            .varobj
            .as_deref()
            .filter(|_| variable.local_index.is_none())
        else {
            self.capture(&variable.name, "", &variable.name);
            return;
        };

        let (root, members) = value_path::variable_path_root(&variable, varobj);
        let members = members.unwrap_or_default().to_owned();
        let command = format!("-var-info-path-expression {}", crate::debugger::quote(root));
        let guard = Rc::clone(self);
        let response = Rc::clone(self);

        if let Err(error) = self
            .requests
            .unscoped(&command)
            .when(move || guard.is_current())
            .enrich(move |_, record| {
                if record.is_superseded() || !response.is_current() {
                    response.finish(Err(
                        "Snapshot cancelled because the stop or selection changed".into(),
                    ));
                } else if let Some(expression) = crate::debugger::variable_path_expression(&record)
                {
                    response.capture(&expression, &members, &variable.name);
                } else {
                    response.finish(Err(record
                        .error_message()
                        .unwrap_or("GDB did not expose a value path")
                        .into()));
                }
            })
        {
            self.finish(Err(error.to_string()));
        }
    }

    fn capture(self: &Rc<Self>, expression: &str, members: &str, name: &str) {
        if expression.len() > 16_384 || members.len() > 4096 || name.len() > 2048 {
            self.finish(Err("Value path exceeds the snapshot limit".into()));
            return;
        }

        let command = crate::language::python::comparison_command(expression, members, name);
        let guard = Rc::clone(self);
        let response = Rc::clone(self);

        if let Err(error) = self
            .requests
            .frame(&command)
            .when(move || guard.is_current())
            .capture(move |_, record, output| {
                let result = if record.is_superseded() || !response.is_current() {
                    Err("Snapshot cancelled because the stop or selection changed".into())
                } else if record.is_done() && record.field("fgdb-output-truncated").is_none() {
                    parse(&output)
                        .ok_or_else(|| "GDB returned an invalid or incomplete snapshot".into())
                } else {
                    Err(record
                        .error_message()
                        .unwrap_or("Value snapshot unavailable. Python-enabled GDB is required.")
                        .into())
                };

                response.finish(result);
            })
        {
            self.finish(Err(error.to_string()));
        }
    }
}

fn parse(output: &str) -> Option<ValueSnapshot> {
    if output.len() > MAX_BYTES * 3 {
        return None;
    }

    let mut values = std::collections::BTreeMap::new();
    let mut end = None;
    let mut bytes = 0;

    for line in output.lines() {
        if let Some(line) = line.strip_prefix("FGDB_COMPARE_END ") {
            let fields = line.split('\t').collect::<Vec<_>>();

            if end.is_some()
                || fields.len() != 3
                || fields[0] != "1"
                || fields[1].parse::<usize>().ok()? != values.len()
            {
                return None;
            }

            end = Some(match fields[2] {
                "1" => true,
                "0" => false,
                _ => return None,
            });
        } else if let Some(line) = line.strip_prefix("FGDB_COMPARE ") {
            let fields = line.split('\t').collect::<Vec<_>>();

            if end.is_some() || values.len() >= MAX_ROWS || fields.len() != 4 || fields[0] != "1" {
                return None;
            }

            let complete = match fields[1] {
                "1" => true,
                "0" => false,
                _ => return None,
            };

            let name = String::from_utf8(type_metadata::decode_hex(fields[2])?).ok()?;
            let text = String::from_utf8(type_metadata::decode_hex(fields[3])?).ok()?;
            bytes += name.len() + text.len();

            if name.is_empty()
                || name.contains('\0')
                || text.contains('\0')
                || bytes > MAX_BYTES
                || values
                    .insert(name, CapturedValue { text, complete })
                    .is_some()
            {
                return None;
            }
        }
    }

    let complete = end?;

    if complete && (values.is_empty() || values.values().any(|value| !value.complete)) {
        return None;
    }

    Some(ValueSnapshot { values, complete })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::{open_debugger_with_printers, request as mi, wait_until};

    #[test]
    fn snapshots_reject_ambiguous_truncated_or_inconsistent_protocols() {
        let row = "FGDB_COMPARE 1\t1\t78\t3432\n";
        let end = "FGDB_COMPARE_END 1\t1\t1\n";
        let snapshot = parse(&format!("{row}{end}")).unwrap();
        assert_eq!(snapshot.values["x"].text, "42");
        assert!(snapshot.complete);

        for invalid in [
            row.to_owned(),
            format!("{row}{row}{end}"),
            format!("{row}{end}{end}"),
            format!("{end}{row}"),
            format!("{}{end}", row.replace("\t1\t78", "\t0\t78")),
            format!("{}{end}", row.replace("3432", "gg")),
            format!("{row}{}", end.replace("\t1\t1", "\t2\t1")),
        ] {
            assert!(parse(&invalid).is_none(), "{invalid}");
        }

        let partial =
            parse("FGDB_COMPARE 1\t0\t78\t3c756e617661696c61626c653e\nFGDB_COMPARE_END 1\t1\t0\n")
                .unwrap();

        assert!(!partial.complete);
        assert!(!partial.values["x"].complete);
    }

    fn capture(
        client: &MiClient,
        expression: &str,
        current: Rc<dyn Fn() -> bool>,
    ) -> Result<ValueSnapshot, String> {
        let frame = crate::debugger::current_frame(&mi(client, "-stack-info-frame")).unwrap();

        let context = crate::debugger::StopContext::new(
            client.transport_epoch(),
            1,
            Some("i1".into()),
            "1".into(),
            frame.level,
        )
        .unwrap();

        let result = Rc::new(RefCell::new(None));
        let response = Rc::clone(&result);

        let request = Rc::new(Request {
            requests: client.bind_stop_requests(context, |_| true),
            current,
            reply: RefCell::new(Some(Box::new(move |snapshot| {
                assert!(response.replace(Some(snapshot)).is_none());
            }))),
        });

        let record =
            crate::debugger::parse_record(r#"^done,name="fake",numchild="0",value="{...}""#)
                .unwrap();

        let mut variable = crate::debugger::variable_object(&record, expression).unwrap();
        variable.local_index = Some(0);
        variable.varobj = None;
        let before = mi(client, "-gdb-show language").token.unwrap();
        request.resolve(variable);
        wait_until(|| result.borrow().is_some());
        let after = mi(client, "-gdb-show language").token.unwrap();

        assert_eq!(
            after - before,
            2,
            "one snapshot command per root, not per field"
        );

        result.take().unwrap()
    }

    #[test]
    #[ignore = "requires Python-enabled GDB and built comparison/language fixtures"]
    fn live_comparisons_capture_unexpanded_aggregates_and_cancel_safely() {
        for (fixture, checkpoint, caller, expression, expected) in [
            (
                "c-aggregate-return-target",
                "return_mixed",
                true,
                "wide",
                " / x",
            ),
            (
                "cpp-variable-viewer-target",
                "variable_viewer_checkpoint",
                false,
                "words",
                "two words",
            ),
            (
                "rust-variable-viewer-target",
                "rust_types_ready",
                true,
                "local_primitives",
                "unsigned",
            ),
            (
                "d-variable-viewer-target",
                "d_values_ready",
                true,
                "particle",
                "position",
            ),
            (
                "ada-variable-viewer-target",
                "ada_values_ready",
                true,
                "sample",
                "position",
            ),
            (
                "c3-variable-viewer-target",
                "c3_variable_viewer_target.c3_values_ready",
                true,
                "particle",
                "position",
            ),
            (
                "fortran-variable-viewer-target",
                "fortran_variable_viewer_target.f90:30",
                false,
                "sample",
                "position",
            ),
            (
                "zig-variable-viewer-target",
                "zig_variable_viewer_target.zig:18",
                false,
                "particle",
                "position",
            ),
            (
                "odin-variable-viewer-target",
                "odin_variable_viewer_target.odin:27",
                false,
                "particle",
                "position",
            ),
        ] {
            let (_debugger, client) =
                open_debugger_with_printers(fixture, checkpoint, fixture.starts_with("rust"));

            if caller {
                assert!(mi(&client, "-stack-select-frame 1").is_done());
            }

            let before = capture(&client, expression, Rc::new(|| true)).unwrap();
            assert!(before.values.len() > 1, "{fixture}: {before:?}");
            assert!(
                format!("{before:?}").contains(expected),
                "{fixture}: {before:?}"
            );

            if fixture.starts_with("cpp-") {
                assert!(before.complete, "{before:?}");
                let change = "python p=gdb.default_visualizer(gdb.parse_and_eval('words')); s=gdb.default_visualizer(list(p.children())[2][1]).to_string(); gdb.selected_inferior().write_memory(s.address, b'T')";
                assert!(mi(&client, &crate::debugger::console_command(change)).is_done());
                let after = capture(&client, expression, Rc::new(|| true)).unwrap();
                assert!(after.complete, "{after:?}");
                assert_ne!(before, after);

                assert!(
                    after
                        .values
                        .values()
                        .any(|value| value.text.contains("Two words"))
                );
            }

            if fixture.starts_with("c-") {
                assert!(before.complete, "{before:?}");
                assert_eq!(before.values.len(), 2);
                assert!(mi(&client, "-data-evaluate-expression \"wide.x = 42\"").is_done());
                let after = capture(&client, expression, Rc::new(|| true)).unwrap();
                assert!(after.complete);
                assert_ne!(before, after);

                assert!(
                    after
                        .values
                        .iter()
                        .any(|(name, value)| name.contains(" / x") && value.text == "42")
                );
            }

            assert!(capture(&client, expression, Rc::new(|| false)).is_err());
            assert!(mi(&client, &crate::debugger::console_command("python assert all(gdb.parameter(p) for p in ('may-call-functions', 'may-write-memory', 'may-write-registers'))")).is_done());
        }

        let (_debugger, client) =
            open_debugger_with_printers("c-variable-location-target", "location_checkpoint", false);

        assert!(capture(&client, "location_probe()", Rc::new(|| true)).is_err());
        assert!(capture(&client, "location_global = 999", Rc::new(|| true)).is_err());
        let calls = capture(&client, "location_probe_calls", Rc::new(|| true)).unwrap();
        assert_eq!(calls.values.values().next().unwrap().text, "0");
        let unreadable = capture(&client, "*(struct LocationRecord *)0", Rc::new(|| true)).unwrap();
        assert!(!unreadable.complete);
        assert!(unreadable.values.values().any(|value| !value.complete));
        let pointer = capture(&client, "record", Rc::new(|| true)).unwrap();
        assert!(pointer.complete);
        assert_eq!(pointer.values.len(), 1);
        let bits = capture(&client, "*record", Rc::new(|| true)).unwrap();
        assert!(bits.complete, "{bits:?}");
        assert!(bits.values.keys().any(|name| name.contains("flags")));

        let (_debugger, client) =
            open_debugger_with_printers("c-array-viewer-target", "c_arrays_ready", false);

        assert!(mi(&client, "-stack-select-frame 1").is_done());
        let partial = capture(&client, "large", Rc::new(|| true)).unwrap();
        assert!(!partial.complete);
        assert_eq!(partial.values.len(), MAX_ROWS);
    }
}
