use super::*;

#[cfg(test)]
mod tests;

pub(in crate::app::variable_viewers) struct LinkedListSettings {
    pub next_members: Vec<String>,
    pub limit: usize,
    pub owned_root: Option<String>,
}

struct LinkedPager {
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    requests: StopRequests,
    session: Weak<VariableViewerSession>,
    root: Variable,
    settings: LinkedListSettings,
    active: RefCell<Option<Rc<RefCell<LinkedTraversal>>>>,
}

impl Drop for LinkedPager {
    fn drop(&mut self) {
        cleanup_viewer_variable_objects(&self.ui, &self.client, self.settings.owned_root.take());
    }
}

pub(in crate::app::variable_viewers) fn start_linked_list(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    requests: StopRequests,
    session: Rc<VariableViewerSession>,
    variable: Variable,
    settings: LinkedListSettings,
) {
    let pager = Rc::new(LinkedPager {
        ui,
        client,
        requests,
        session: Rc::downgrade(&session),
        root: variable,
        settings,
        active: RefCell::new(None),
    });

    session.connect_linked(move |action| pager.handle(action));
}

impl LinkedPager {
    fn handle(&self, action: LinkedListAction) {
        let active = self.active.borrow().clone();

        if let LinkedListAction::Restart(query) = action {
            if active.as_ref().is_some_and(|active| {
                let active = active.borrow();
                !active.finished && !active.paused
            }) {
                return;
            }

            self.restart(query);
            return;
        }

        let Some(active) = active else { return };

        if matches!(action, LinkedListAction::Cancel) {
            finish_linked(
                &active,
                Some(String::from(
                    "Cancelled · Cached nodes retained · Restart to traverse again",
                )),
            );
            return;
        }

        let resume = {
            let mut traversal = active.borrow_mut();

            if !traversal.finished && !traversal.paused {
                return;
            }

            let offset = match action {
                LinkedListAction::First => 0,
                LinkedListAction::Previous => traversal.offset.saturating_sub(traversal.page_size),
                LinkedListAction::Next => {
                    let next = traversal
                        .offset
                        .saturating_add(traversal.page_size)
                        .min(traversal.rows.len());

                    if next == traversal.rows.len() && traversal.finished {
                        return;
                    }

                    next
                }
                _ => return,
            };

            traversal.offset = offset;
            let resume = !traversal.finished && offset + traversal.page_size > traversal.rows.len();

            if resume {
                traversal.paused = false;
                traversal.message = String::from("Following bounded linked-list page…");
            }

            resume
        };

        render_linked(&active, true);

        if resume {
            // Re-root each new page to retire the previous page's GDB child tree.
            // Cached pages keep plain values, not growing chains of variable objects.
            prepare_linked_root(active);
        }
    }

    fn restart(&self, query: LinkedListQuery) {
        let Some(session) = self.session.upgrade().filter(|session| session.is_open()) else {
            return;
        };

        if let Err(error) = query.validate(self.settings.limit) {
            session.fail(error);
            return;
        }

        if !self.requests.is_current() {
            let active = self.active.borrow().clone();

            if let Some(active) = active {
                finish_linked(&active, Some(String::from(STALE_VIEWER_MESSAGE)));
                active.borrow_mut().message = String::from(STALE_VIEWER_MESSAGE);
                render_linked(&active, false);
            } else {
                session.show_linked_page([], LinkedListProgress::default(), STALE_VIEWER_MESSAGE);
            }

            return;
        }

        let old = self.active.borrow_mut().take();

        if let Some(old) = old {
            finish_linked(&old, None);
        }

        let revision = session.begin_linked();
        let traversal = Rc::new(RefCell::new(LinkedTraversal {
            ui: self.ui.clone(),
            client: Rc::clone(&self.client),
            requests: self.requests.clone(),
            session: self.session.clone(),
            revision,
            current: self.root.clone(),
            current_address: self.root.pointer_address(),
            pending_node: None,
            address_source: None,
            member: query.member,
            next_members: self
                .settings
                .next_members
                .iter()
                .map(|member| normalize_member_name(member))
                .collect(),
            seen_addresses: HashSet::new(),
            seen_objects: HashSet::new(),
            owned_variable_objects: HashSet::new(),
            wrapper_depth: 0,
            page_size: query.page_size,
            offset: 0,
            rows: Vec::new(),
            message: String::from("Preparing linked-list traversal…"),
            paused: false,
            finished: false,
            fields_read: 0,
            requests_sent: 0,
            fields_truncated: false,
        }));

        self.active.replace(Some(Rc::clone(&traversal)));
        prepare_linked_root(traversal);
    }
}

pub(super) fn render_linked(traversal: &Rc<RefCell<LinkedTraversal>>, replace_rows: bool) {
    let (session, rows, progress, message) = {
        let traversal = traversal.borrow();
        let Some(session) = traversal
            .session
            .upgrade()
            .filter(|session| session.page_is_current(traversal.revision))
        else {
            return;
        };
        let end = traversal
            .offset
            .saturating_add(traversal.page_size)
            .min(traversal.rows.len());
        let rows = replace_rows.then(|| traversal.rows[traversal.offset..end].to_vec());
        let progress = LinkedListProgress {
            offset: traversal.offset,
            shown: end - traversal.offset,
            cached: traversal.rows.len(),
            busy: !traversal.finished && !traversal.paused,
            can_continue: !traversal.finished && traversal.rows.len() < NODE_LIMIT,
        };

        (session, rows, progress, traversal.message.clone())
    };

    // Completion updates controls without rebuilding rows or losing selection.
    if let Some(rows) = rows {
        session.show_linked_page(rows, progress, &message);
    } else {
        session.update_linked_progress(progress, &message);
    }
}
