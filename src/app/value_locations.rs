//! Bounded, cancellable storage queries using the existing stopped MI transport.

use super::*;
use crate::debugger::location::{BATCH_LIMIT, LocationReply, ValueLocation, parse_locations};

pub(super) fn request(
    requests: StopRequests,
    variables: Vec<Variable>,
    current: Rc<dyn Fn() -> bool>,
    reply: LocationReply,
) {
    if variables.is_empty() || variables.len() > BATCH_LIMIT {
        reply(None);
        return;
    }

    let batch = Rc::new(Batch {
        requests,
        current,
        remaining: Cell::new(variables.len()),
        paths: RefCell::new(vec![(String::new(), None); variables.len()]),
        reply: RefCell::new(Some(reply)),
    });

    for (index, variable) in variables.into_iter().enumerate() {
        if !batch.is_current() {
            batch.finish(None);
            break;
        }

        // Local roots already carry the expression from the current catalog.
        // Their temporary MI objects add no path information; only descendants
        // and watches need GDB to resolve an object path first.
        if variable.local_index.is_some() {
            batch.resolved(index, variable.name, None);
            continue;
        }

        let Some(varobj) = variable.varobj.as_deref() else {
            batch.resolved(index, variable.name, None);
            continue;
        };

        let (root, members) = super::value_path::variable_path_root(&variable, varobj);
        let members = members.map(str::to_owned);
        let command = format!("-var-info-path-expression {}", crate::debugger::quote(root));
        let response = Rc::clone(&batch);
        let guard = Rc::clone(&batch);

        if batch
            .requests
            .unscoped(&command)
            .when(move || guard.is_current())
            .enrich(move |_, record| {
                if record.class == "superseded" || !response.is_current() {
                    response.finish(None);
                    return;
                }

                let expression =
                    crate::debugger::variable_path_expression(&record).unwrap_or_default();
                response.resolved(index, expression, members);
            })
            .is_err()
        {
            batch.resolved(index, String::new(), None);
        }
    }
}

struct Batch {
    requests: StopRequests,
    current: Rc<dyn Fn() -> bool>,
    remaining: Cell<usize>,
    paths: RefCell<Vec<(String, Option<String>)>>,
    reply: RefCell<Option<LocationReply>>,
}

impl Batch {
    fn is_current(&self) -> bool {
        let pending = self.reply.borrow().is_some();
        pending && self.requests.is_current() && (self.current)()
    }

    fn finish(&self, result: Option<Vec<ValueLocation>>) {
        let reply = self.reply.borrow_mut().take();

        if let Some(reply) = reply {
            reply(result);
        }
    }

    fn resolved(self: &Rc<Self>, index: usize, expression: String, members: Option<String>) {
        if !self.is_current() {
            self.finish(None);
            return;
        }

        if expression.len() <= 16_384
            && members
                .as_ref()
                .is_none_or(|members| members.len() <= 16_384)
        {
            self.paths.borrow_mut()[index] = (expression, members);
        }

        self.remaining.set(self.remaining.get() - 1);

        if self.remaining.get() != 0 {
            return;
        }

        let command = crate::language::python::value_locations_command(&self.paths.borrow());
        let guard = Rc::clone(self);
        let response = Rc::clone(self);

        if let Err(error) = self
            .requests
            .frame(&command)
            .when(move || guard.is_current())
            .capture(move |_, record, output| {
                if record.class == "superseded" || !response.is_current() {
                    response.finish(None);
                    return;
                }

                let result = (record.is_done() && record.field("fgdb-output-truncated").is_none())
                    .then(|| parse_locations(&output, response.paths.borrow().len()))
                    .flatten();

                if let Some(result) = result {
                    response.finish(Some(result));
                } else {
                    response.failed(
                        record
                            .error_message()
                            .unwrap_or("GDB returned incomplete location metadata"),
                    );
                }
            })
        {
            self.failed(&error.to_string());
        }
    }

    fn failed(&self, message: &str) {
        let count = self.paths.borrow().len();
        self.finish(Some(vec![
            ValueLocation::Unknown(
                message.chars().take(512).collect()
            );
            count
        ]));
    }
}

#[cfg(test)]
mod tests;
