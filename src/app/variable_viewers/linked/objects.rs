//! Viewer-owned GDB objects, native ownership links, and page-root retirement.

use super::*;

pub(super) fn prepare_linked_root(traversal: Rc<RefCell<LinkedTraversal>>) {
    if !begin_linked_request(&traversal) {
        return;
    }
    let (varobj, requests) = {
        let traversal = traversal.borrow();
        (traversal.current.varobj.clone(), traversal.requests.clone())
    };
    let Some(varobj) = varobj else {
        finish_linked(
            &traversal,
            Some(String::from("The list root has no inspectable GDB object")),
        );
        return;
    };

    let command = format!(
        "-var-info-path-expression {}",
        crate::debugger::quote(&varobj)
    );
    let guard = Rc::clone(&traversal);
    let response = Rc::clone(&traversal);

    if let Err(error) = requests
        .unscoped(&command)
        .when(move || linked_is_current(&guard))
        .request(move |_, record| {
            if !linked_is_current(&response) || record.class == "superseded" {
                finish_linked(&response, Some(String::from(STALE_VIEWER_MESSAGE)));
                return;
            }

            let Some(path) = crate::debugger::variable_path_expression(&record) else {
                // Synthetic printer children need not have evaluable expressions.
                // Borrow their tree without ever assuming ownership of it.
                request_linked_node(response);
                return;
            };

            create_linked_root(response, path);
        })
    {
        finish_linked(
            &traversal,
            Some(format!("Could not resolve list root: {error}")),
        );
    }
}

fn create_linked_root(traversal: Rc<RefCell<LinkedTraversal>>, expression: String) {
    if !begin_linked_request(&traversal) {
        return;
    }
    let name = next_variable_object_name();
    let requests = traversal.borrow().requests.clone();
    traversal
        .borrow_mut()
        .owned_variable_objects
        .insert(name.clone());
    let command = format!(
        "-var-create {name} * {}",
        crate::debugger::quote(&expression)
    );
    let guard = Rc::clone(&traversal);
    let response = Rc::clone(&traversal);

    if let Err(error) = requests
        .frame(&command)
        .when(move || linked_is_current(&guard))
        .inspect(AUTOMATIC_PRINT_ELEMENTS, move |_, record| {
            if !linked_is_current(&response) || record.class == "superseded" {
                let (ui, client) = {
                    let traversal = response.borrow();
                    (traversal.ui.clone(), Rc::clone(&traversal.client))
                };

                cleanup_viewer_variable_objects(&ui, &client, Some(name));
                finish_linked(&response, Some(String::from(STALE_VIEWER_MESSAGE)));
                return;
            }

            let Some(variable) = record
                .is_done()
                .then(|| crate::debugger::variable_object(&record, &expression))
                .flatten()
            else {
                // A printer's synthetic path may exist but not be evaluable.
                // Its borrowed tree remains usable and is never deleted here.
                request_linked_node(response);
                return;
            };

            let (ui, client, retired) = {
                let mut traversal = response.borrow_mut();
                traversal.current_address = variable.pointer_address();
                traversal.address_source = None;
                traversal.current = variable;
                let retired = traversal
                    .owned_variable_objects
                    .drain()
                    .filter(|root| root != &name)
                    .collect::<Vec<_>>();
                traversal.owned_variable_objects.insert(name);

                (traversal.ui.clone(), Rc::clone(&traversal.client), retired)
            };

            cleanup_viewer_variable_objects(&ui, &client, retired);
            seed_linked_root_address(response);
        })
    {
        finish_linked(
            &traversal,
            Some(format!("Could not prepare list root: {error}")),
        );
    }
}

fn resume_linked_node(traversal: Rc<RefCell<LinkedTraversal>>) {
    let pending = traversal.borrow_mut().pending_node.take();

    if let Some((current, children)) = pending {
        append_linked_node(&traversal, current, children);
    } else {
        request_linked_node(traversal);
    }
}

fn seed_linked_root_address(traversal: Rc<RefCell<LinkedTraversal>>) {
    let (varobj, should_query, requests) = {
        let traversal = traversal.borrow();

        (
            traversal.current.varobj.clone(),
            traversal.current_address.is_none() && !traversal.current.is_pointer(),
            traversal.requests.clone(),
        )
    };

    let Some(varobj) = varobj.filter(|_| should_query) else {
        resume_linked_node(traversal);
        return;
    };

    if !begin_linked_request(&traversal) {
        return;
    }

    let command = format!(
        "-var-info-path-expression {}",
        crate::debugger::quote(&varobj)
    );

    let traversal_for_guard = Rc::clone(&traversal);
    let traversal_for_response = Rc::clone(&traversal);

    if requests
        .unscoped(&command)
        .when(move || linked_is_current(&traversal_for_guard))
        .request(move |_, record| {
            if record.class == "superseded" || !linked_is_current(&traversal_for_response) {
                finish_linked(
                    &traversal_for_response,
                    Some(String::from(STALE_VIEWER_MESSAGE)),
                );

                return;
            }

            let Some(path) = crate::debugger::variable_path_expression(&record) else {
                resume_linked_node(traversal_for_response);
                return;
            };

            let command = format!(
                "-data-evaluate-expression {}",
                crate::debugger::quote(&format!("&({path})"))
            );

            let requests = traversal_for_response.borrow().requests.clone();

            let traversal_for_guard = Rc::clone(&traversal_for_response);
            let traversal_for_address = Rc::clone(&traversal_for_response);

            if !begin_linked_request(&traversal_for_response) {
                return;
            }

            if requests
                .frame(&command)
                .when(move || linked_is_current(&traversal_for_guard))
                .inspect(AUTOMATIC_PRINT_ELEMENTS, move |_, record| {
                    if record.class == "superseded" || !linked_is_current(&traversal_for_address) {
                        finish_linked(
                            &traversal_for_address,
                            Some(String::from(STALE_VIEWER_MESSAGE)),
                        );

                        return;
                    }

                    if let Some(address) = crate::debugger::evaluated_value(&record)
                        .as_deref()
                        .and_then(pointer_address)
                        .filter(|address| *address != 0)
                    {
                        let mut traversal = traversal_for_address.borrow_mut();
                        traversal.current_address = Some(address);
                        traversal.address_source =
                            traversal
                                .current
                                .type_name
                                .as_ref()
                                .map(|type_name| NodeAddress {
                                    base: address,
                                    type_name: type_name.clone(),
                                    members: Vec::new(),
                                });
                    }

                    resume_linked_node(traversal_for_address);
                })
                .is_err()
            {
                resume_linked_node(traversal_for_response);
            }
        })
        .is_err()
    {
        resume_linked_node(traversal);
    }
}

pub(super) fn resolve_linked_node_address(traversal: Rc<RefCell<LinkedTraversal>>) {
    let command = {
        let traversal = traversal.borrow();
        traversal.address_source.as_ref().map(|source| {
            crate::language::python::member_address_command(
                source.base,
                &source.type_name,
                &source.members,
            )
        })
    };

    let Some(command) = command else {
        seed_linked_root_address(traversal);
        return;
    };

    if !begin_linked_request(&traversal) {
        return;
    }

    let requests = traversal.borrow().requests.clone();
    let guard = Rc::clone(&traversal);
    let response = Rc::clone(&traversal);

    if let Err(error) = requests
        .frame(&command)
        .when(move || linked_is_current(&guard))
        .capture(move |_, record, output| {
            if record.class == "superseded" || !linked_is_current(&response) {
                finish_linked(&response, Some(String::from(STALE_VIEWER_MESSAGE)));
                return;
            }

            let address = record
                .is_done()
                .then(|| {
                    if record.field("fgdb-output-truncated").is_some() {
                        return None;
                    }

                    let mut addresses = output
                        .lines()
                        .filter_map(|line| line.strip_prefix("FGDB_MEMBER_ADDRESS:0x"));
                    let address = u64::from_str_radix(addresses.next()?, 16).ok()?;
                    (addresses.next().is_none() && address != 0).then_some(address)
                })
                .flatten();

            if let Some(address) = address {
                response.borrow_mut().current_address = Some(address);
                resume_linked_node(response);
            } else {
                finish_linked(
                    &response,
                    Some(String::from(
                        "Could not verify wrapped node identity · Cached nodes retained",
                    )),
                );
            }
        })
    {
        finish_linked(
            &traversal,
            Some(format!("Could not resolve wrapped node: {error}")),
        );
    }
}

pub(super) fn request_linked_raw_wrapper(
    traversal: Rc<RefCell<LinkedTraversal>>,
    current: Variable,
) {
    if !begin_linked_request(&traversal) {
        return;
    }

    let Some(name) = current.varobj.as_deref() else {
        return;
    };
    let command = format!("-var-set-visualizer {} None", crate::debugger::quote(name));
    let requests = traversal.borrow().requests.clone();
    let guard = Rc::clone(&traversal);
    let response = Rc::clone(&traversal);

    // Read ownership pointers from the native layout to preserve cycle identity.
    // Only viewer-owned wrappers are changed. Payload printers and locals stay intact.
    if let Err(error) = requests
        .unscoped(&command)
        .when(move || linked_is_current(&guard))
        .request(move |_, record| {
            if record.class == "superseded" || !linked_is_current(&response) {
                finish_linked(&response, Some(String::from(STALE_VIEWER_MESSAGE)));
                return;
            }

            // If a backend cannot disable this visualizer, follow its printer fields.
            request_linked_children(response, current);
        })
    {
        finish_linked(
            &traversal,
            Some(format!("Could not inspect ownership wrapper: {error}")),
        );
    }
}

pub(super) fn request_linked_dereference(
    traversal: Rc<RefCell<LinkedTraversal>>,
    current: Variable,
) {
    if !begin_linked_request(&traversal) {
        return;
    }
    let Some(varobj) = current.varobj.as_deref() else {
        finish_linked(
            &traversal,
            Some(String::from(
                "The next pointer has no inspectable GDB object",
            )),
        );

        return;
    };

    let requests = traversal.borrow().requests.clone();

    let command = format!(
        "-var-info-path-expression {}",
        crate::debugger::quote(varobj)
    );

    let traversal_for_guard = Rc::clone(&traversal);
    let traversal_for_response = Rc::clone(&traversal);

    if let Err(error) = requests
        .unscoped(&command)
        .when(move || linked_is_current(&traversal_for_guard))
        .request(move |_client, record| {
            if record.class == "superseded" || !linked_is_current(&traversal_for_response) {
                finish_linked(
                    &traversal_for_response,
                    Some(String::from(STALE_VIEWER_MESSAGE)),
                );

                return;
            }

            let Some(path) = crate::debugger::variable_path_expression(&record) else {
                finish_linked(
                    &traversal_for_response,
                    Some(String::from("GDB could not dereference the next pointer")),
                );

                return;
            };

            let dereference_varobj = next_variable_object_name();

            traversal_for_response
                .borrow_mut()
                .owned_variable_objects
                .insert(dereference_varobj.clone());

            let command = format!(
                "-var-create {dereference_varobj} * {}",
                crate::debugger::quote(&format!("*({path})"))
            );

            let requests = traversal_for_response.borrow().requests.clone();

            let traversal_for_guard = Rc::clone(&traversal_for_response);
            let traversal_for_dereference = Rc::clone(&traversal_for_response);

            if !begin_linked_request(&traversal_for_response) {
                return;
            }

            if let Err(error) = requests
                .frame(&command)
                .when(move || linked_is_current(&traversal_for_guard))
                .inspect(AUTOMATIC_PRINT_ELEMENTS, move |_, record| {
                    if record.class == "superseded"
                        || !linked_is_current(&traversal_for_dereference)
                    {
                        let (ui, client) = {
                            let traversal = traversal_for_dereference.borrow();
                            (traversal.ui.clone(), Rc::clone(&traversal.client))
                        };

                        cleanup_viewer_variable_objects(&ui, &client, Some(dereference_varobj));

                        finish_linked(
                            &traversal_for_dereference,
                            Some(String::from(STALE_VIEWER_MESSAGE)),
                        );

                        return;
                    }

                    let name = traversal_for_dereference.borrow().current.name.clone();

                    let Some(child) = record
                        .is_done()
                        .then(|| crate::debugger::variable_object(&record, &format!("*{name}")))
                        .flatten()
                    else {
                        finish_linked(
                            &traversal_for_dereference,
                            Some(String::from("GDB could not inspect the pointed-to node")),
                        );

                        return;
                    };

                    {
                        let mut traversal = traversal_for_dereference.borrow_mut();

                        if let Some(varobj) = child.varobj.clone() {
                            traversal.owned_variable_objects.insert(varobj);
                        }

                        traversal.current = child;
                    }

                    request_linked_node(traversal_for_dereference);
                })
            {
                finish_linked(
                    &traversal_for_response,
                    Some(format!("Could not queue pointer inspection: {error}")),
                );
            }
        })
    {
        finish_linked(
            &traversal,
            Some(format!("Could not queue pointer resolution: {error}")),
        );
    }
}
