use super::*;

pub(super) fn request_array(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    requests: StopRequests,
    session: Rc<VariableViewerSession>,
    variable: Variable,
    limit: usize,
    owned_root: Option<String>,
) {
    let Some(varobj) = variable.varobj.as_deref() else {
        session.fail("GDB did not expose an array object");
        cleanup_viewer_variable_objects(&ui, &client, owned_root);
        return;
    };

    Rc::new(ArrayRequest {
        ui,
        client,
        requests,
        session,
        limit,
        owned_root,
    })
    .resolve(varobj);
}

struct ArrayRequest {
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    requests: StopRequests,
    session: Rc<VariableViewerSession>,
    limit: usize,
    owned_root: Option<String>,
}

impl ArrayRequest {
    fn resolve(self: Rc<Self>, varobj: &str) {
        // GDB refuses path expressions below dynamic varobjs. Resolve the
        // owned root, then let the Fortran adapter walk typed fields and native
        // coordinates. Never feed pretty-printed child names to GDB's parser.
        let (root, member_path) = varobj.split_once('.').unwrap_or((varobj, ""));
        let member_path = member_path.to_owned();

        let command = format!("-var-info-path-expression {}", crate::debugger::quote(root));

        let request = Rc::clone(&self);
        let session = Rc::clone(&self.session);

        if let Err(error) = self
            .requests
            .unscoped(&command)
            .when(move || session.is_open())
            .request(move |_, record| {
                if !viewer_is_current(&request.requests, &request.session)
                    || record.class == "superseded"
                {
                    request.finish(STALE_VIEWER_MESSAGE, false);
                } else if let Some(expression) = crate::debugger::variable_path_expression(&record)
                {
                    request.inspect(&expression, &member_path);
                } else {
                    request.finish("GDB could not resolve the array expression", true);
                }
            })
        {
            self.finish(&format!("Could not resolve array: {error}"), true);
        }
    }

    fn inspect(self: Rc<Self>, expression: &str, member_path: &str) {
        let command =
            crate::language::python::array_inspection_command(expression, member_path, self.limit);

        let request = Rc::clone(&self);
        let session = Rc::clone(&self.session);

        if let Err(error) = self
            .requests
            .frame(&command)
            .when(move || session.is_open())
            .capture(move |_, record, output| {
                if record.class == "superseded" {
                    request.finish(STALE_VIEWER_MESSAGE, false);
                } else if record.is_done() && record.field("fgdb-output-truncated").is_none()
                    && let Some((rows, summary)) = parse_array(&output, request.limit)
                {
                    if request.session.is_open() {
                        request.session.append(rows);
                    }

                    request.finish(&summary, false);
                } else {
                    let message = record.error_message().unwrap_or(
                        "GDB could not inspect this array - Python and valid allocated array bounds are required",
                    );

                    request.finish(message, true);
                }
            })
        {
            self.finish(&format!("Could not queue array inspection: {error}"), true);
        }
    }

    fn finish(&self, message: &str, failed: bool) {
        if self.session.is_open() {
            if failed {
                self.session.fail(message);
            } else {
                self.session.finish(message);
            }
        }

        cleanup_viewer_variable_objects(&self.ui, &self.client, self.owned_root.clone());
    }
}

fn parse_array(output: &str, limit: usize) -> Option<(Vec<VariableViewerRow>, String)> {
    if output.len() > 256 * 1024 {
        return None;
    }

    let decode = |field: &str| String::from_utf8(type_metadata::decode_hex(field)?).ok();
    let mut rows = Vec::new();
    let mut summary = None;

    for line in output.lines() {
        if let Some(fields) = line.strip_prefix("FGDB_ARRAY_ROW:") {
            if rows.len() >= limit || summary.is_some() {
                return None;
            }

            let mut fields = fields.split('\t');
            let ordinal = decode(fields.next()?)?;
            let value = decode(fields.next()?)?;
            let type_name = decode(fields.next()?)?;

            if fields.next().is_some() {
                return None;
            }

            rows.push(VariableViewerRow {
                ordinal,
                name: String::new(),
                value,
                type_name,
                details: String::new(),
            });
        } else if let Some(field) = line.strip_prefix("FGDB_ARRAY_SUMMARY:") {
            if summary.is_some() {
                return None;
            }

            summary = Some(decode(field)?);
        }
    }

    Some((rows, summary?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_responses_preserve_coordinates_and_reject_partial_or_over_budget_data() {
        let row = "FGDB_ARRAY_ROW:282d312c3429\t3432\t696e7465676572\n";
        let summary = "FGDB_ARRAY_SUMMARY:646f6e65\n";
        let response = format!("{row}{summary}");
        let (rows, summary_text) = parse_array(&response, 1).unwrap();
        assert_eq!(rows[0].ordinal, "(-1,4)");
        assert_eq!(rows[0].value, "42");
        assert_eq!(summary_text, "done");
        assert!(parse_array(row, 1).is_none());
        assert!(parse_array(&response, 0).is_none());
        assert!(parse_array(&format!("{response}{row}"), 2).is_none());
        assert!(parse_array(&format!("{response}{summary}"), 1).is_none());
        assert!(parse_array(&response.replace("3432", "zz"), 1).is_none());
    }
}
