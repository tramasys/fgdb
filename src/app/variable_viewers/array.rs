//! Stop-bound navigation for native arrays and printer-backed sequences.

use super::*;
use crate::debugger::array::{ArrayPage, ArrayShape, BATCH_LIMIT, PAGE_LIMIT};

mod protocol;
use protocol::{parse_batch, parse_error, parse_shape};

pub(super) fn request_array(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    requests: StopRequests,
    session: Rc<VariableViewerSession>,
    variable: Variable,
    limit: usize,
    owned_root: Option<String>,
) {
    Rc::new(ArrayRequest {
        ui,
        client,
        requests,
        session,
        variable,
        limit: limit.min(PAGE_LIMIT),
        owned_root: RefCell::new(owned_root),
    })
    .resolve();
}

struct ArrayRequest {
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    requests: StopRequests,
    session: Rc<VariableViewerSession>,
    variable: Variable,
    limit: usize,
    owned_root: RefCell<Option<String>>,
}

impl Drop for ArrayRequest {
    fn drop(&mut self) {
        cleanup_viewer_variable_objects(&self.ui, &self.client, self.owned_root.get_mut().take());
    }
}

impl ArrayRequest {
    fn resolve(self: Rc<Self>) {
        let Some(varobj) = self.variable.varobj.as_deref() else {
            self.session.fail("GDB did not expose an array object");
            return;
        };

        let fortran = self
            .variable
            .type_name
            .as_deref()
            .is_some_and(crate::language::is_fortran_array);
        let (root, members) = if fortran {
            varobj.split_once('.').unwrap_or((varobj, ""))
        } else {
            (varobj, "")
        };

        let members = members.to_owned();
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
                    request.session.finish(STALE_VIEWER_MESSAGE);
                } else if let Some(expression) = crate::debugger::variable_path_expression(&record)
                {
                    request.describe(expression, members);
                } else {
                    request.fallback();
                }
            })
        {
            self.session
                .fail(&format!("Could not resolve array: {error}"));
        }
    }

    fn describe(self: Rc<Self>, expression: String, members: String) {
        let command = crate::language::python::array_description_command(&expression, &members);
        let request = Rc::clone(&self);
        let session = Rc::clone(&self.session);

        if let Err(error) = self
            .requests
            .frame(&command)
            .when(move || session.is_open())
            .capture(move |_, record, output| {
                if record.class == "superseded"
                    || !viewer_is_current(&request.requests, &request.session)
                {
                    request.session.finish(STALE_VIEWER_MESSAGE);
                    return;
                }

                if let Some(error) = parse_error(&output) {
                    request.session.fail(&error);
                    return;
                }

                let Some(shape) = record
                    .is_done()
                    .then(|| parse_shape(&output))
                    .flatten()
                    .filter(|_| record.field("fgdb-output-truncated").is_none())
                else {
                    request.fallback();
                    return;
                };

                if let Err(error) = request.session.configure_array(shape.clone()) {
                    request.session.fail(error);
                    return;
                }

                let pager = Rc::new(ArrayPager {
                    requests: request.requests.clone(),
                    session: Rc::downgrade(&request.session),
                    expression,
                    members,
                    shape,
                    limit: request.limit,
                });

                request
                    .session
                    .connect_array_query(move |page| Rc::clone(&pager).start(page));
            })
        {
            self.session
                .fail(&format!("Could not read array bounds: {error}"));
        }
    }

    fn fallback(&self) {
        if self
            .variable
            .type_name
            .as_deref()
            .is_some_and(crate::language::has_native_array_bounds)
        {
            self.session.fail("Native array bounds are unavailable. Python-enabled GDB and allocated array storage are required");
            return;
        }

        // Preserve the MI-only viewer for backends without Python and for
        // collection wrappers which do not expose a resolvable expression.
        request_indexed_children(
            self.ui.clone(),
            Rc::clone(&self.client),
            self.requests.clone(),
            Rc::clone(&self.session),
            self.variable.clone(),
            self.limit,
            self.owned_root.borrow_mut().take(),
        );
    }
}

struct ArrayPager {
    requests: StopRequests,
    session: Weak<VariableViewerSession>,
    expression: String,
    members: String,
    shape: ArrayShape,
    limit: usize,
}

impl ArrayPager {
    fn start(self: Rc<Self>, mut page: ArrayPage) {
        let Some(session) = self.session.upgrade().filter(|session| session.is_open()) else {
            return;
        };

        if !self.requests.is_current() {
            session.finish_page(STALE_VIEWER_MESSAGE, true);
            return;
        }

        page.count = page.count.min(self.limit);
        let total = match page.validate(&self.shape) {
            Ok(total) => total,
            Err(error) => {
                session.fail(error);
                return;
            }
        };

        let revision = session.begin_page(&page);

        if total == 0 {
            session.finish_page("Empty slice", true);
            return;
        }

        Rc::new(PageRequest {
            pager: self,
            page,
            revision,
            total,
            loaded: Cell::new(0),
        })
        .batch();
    }
}

struct PageRequest {
    pager: Rc<ArrayPager>,
    page: ArrayPage,
    revision: u64,
    total: u64,
    loaded: Cell<usize>,
}

impl PageRequest {
    fn session(&self) -> Option<Rc<VariableViewerSession>> {
        self.pager
            .session
            .upgrade()
            .filter(|session| session.page_is_current(self.revision))
    }

    fn batch(self: Rc<Self>) {
        let Some(session) = self.session() else {
            return;
        };

        if !self.pager.requests.is_current() {
            session.finish_page(STALE_VIEWER_MESSAGE, true);
            return;
        }

        let offset = self.page.offset + self.loaded.get() as u64;
        let count = BATCH_LIMIT
            .min(self.page.count - self.loaded.get())
            .min((self.total - offset).min(BATCH_LIMIT as u64) as usize);

        let command = crate::language::python::array_inspection_command(
            &self.pager.expression,
            &self.pager.members,
            &self.page,
            offset,
            count,
        );

        let guard = Rc::clone(&self);
        let response = Rc::clone(&self);

        if let Err(error) = self
            .pager
            .requests
            .frame(&command)
            .when(move || guard.session().is_some())
            .capture(move |_, record, output| {
                let Some(session) = response.session() else {
                    return;
                };

                if record.class == "superseded" || !response.pager.requests.is_current() {
                    session.finish_page(STALE_VIEWER_MESSAGE, true);
                    return;
                }

                let batch = record
                    .is_done()
                    .then(|| parse_batch(&output, count))
                    .flatten()
                    .filter(|_| record.field("fgdb-output-truncated").is_none());

                let Some((shape, rows, ended)) = batch else {
                    let error = parse_error(&output);
                    session.fail(error.as_deref().or_else(|| record.error_message()).unwrap_or(
                        "Array page could not be read completely. Previous batches are retained",
                    ));
                    return;
                };

                if shape != response.pager.shape {
                    session.fail("Array bounds or representation changed. Reopen the viewer");
                    return;
                }

                if rows.is_empty() && !ended {
                    session.fail("Array response made no progress within the output budget");
                    return;
                }

                response.loaded.set(response.loaded.get() + rows.len());
                session.append(rows);
                let exhausted =
                    response.page.offset + response.loaded.get() as u64 >= response.total;

                if ended || exhausted || response.loaded.get() >= response.page.count {
                    session.finish_page(
                        &format!(
                            "{} elements loaded · Target memory at this stop",
                            response.loaded.get()
                        ),
                        ended || exhausted,
                    );
                } else {
                    response.batch();
                }
            })
        {
            session.fail(&format!("Could not queue array page: {error}"));
        }
    }
}
