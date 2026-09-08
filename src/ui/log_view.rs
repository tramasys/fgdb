use super::*;
use std::collections::VecDeque;

mod history;
pub(super) use history::LogLevel;
use history::{LogEntry, LogFilter, LogHistory};

#[derive(Clone)]
pub(super) struct ApplicationLog(Rc<LogState>);

struct LogState {
    root: gtk::Box,
    view: gtk::TextView,
    buffer: gtk::TextBuffer,
    time_tag: gtk::TextTag,
    level_tags: [gtk::TextTag; LogLevel::ALL.len()],
    end_mark: gtk::TextMark,
    count: gtk::Label,
    follow: gtk::ToggleButton,
    filters: [(LogLevel, gtk::ToggleButton); LogLevel::ALL.len()],
    scroll: gtk::ScrolledWindow,
    clear: gtk::Button,
    adjustment: gtk::Adjustment,
    history: RefCell<LogHistory>,
    filter: Cell<LogFilter>,
    applying_filters: Cell<bool>,
    projection: RefCell<LogProjection>,
    dirty: Cell<bool>,
    refilter_pending: Cell<bool>,
    rendering: Cell<bool>,
    render_source: RefCell<Option<glib::SourceId>>,
    follow_pending: Cell<bool>,
    layout_subscription: RefCell<Option<lifecycle::SignalSubscription>>,
}

#[derive(Default)]
struct LogProjection {
    next_id: u64,
    // Retain only identities and character counts, not evicted message text.
    entries: VecDeque<(u64, i32)>,
}

impl Drop for LogState {
    fn drop(&mut self) {
        if let Some(source) = self.render_source.get_mut().take() {
            source.remove();
        }
    }
}

impl ApplicationLog {
    pub(super) fn new(theme: &Theme) -> Self {
        let root = components::panel();
        let header = components::control_row();
        header.add_css_class("panel-header");
        header.add_css_class("application-log-header");
        let title = section_title("APPLICATION LOG");
        header.append(&title);
        let count = gtk::Label::new(Some("No messages"));
        count.add_css_class("muted");
        count.set_xalign(0.0);
        count.set_hexpand(true);
        count.set_ellipsize(pango::EllipsizeMode::End);
        count.set_tooltip_text(Some(
            "In-memory fgdb messages, oldest first. Retains up to 1,000 entries or 1 MiB. Individual messages are limited to 8 KiB",
        ));

        let filters = [
            (LogLevel::Info, "Info", "Show informational messages"),
            (LogLevel::Warning, "Warnings", "Show warnings"),
            (LogLevel::Error, "Errors", "Show errors"),
        ]
        .map(|(level, label, tooltip)| {
            let button = gtk::ToggleButton::with_label(label);
            button.set_active(true);
            button.set_tooltip_text(Some(tooltip));
            header.append(&button);
            (level, button)
        });

        header.append(&count);
        let follow = gtk::ToggleButton::with_label("Follow");
        follow.set_active(true);
        follow.set_tooltip_text(Some(
            "Follow new messages. Turn off or select text to read earlier entries",
        ));

        header.append(&follow);
        let clear = gtk::Button::with_label("Clear");
        clear.set_sensitive(false);
        clear.set_tooltip_text(Some("Clear all log entries, including hidden messages"));
        header.append(&clear);
        root.append(&header);

        let buffer = gtk::TextBuffer::new(None::<&gtk::TextTagTable>);
        // Evicted entries must not survive in an undo history.
        buffer.set_enable_undo(false);
        let time_tag = buffer
            .create_tag(Some("time"), &[("foreground", &theme.colors.muted)])
            .expect("unique log timestamp tag");

        let level_tags = LogLevel::ALL.map(|level| {
            let color = match level {
                LogLevel::Info => theme.colors.accent,
                LogLevel::Warning => theme.colors.warning,
                LogLevel::Error => theme.colors.danger,
            };

            buffer
                .create_tag(Some(level.label()), &[("foreground", &color)])
                .expect("unique log severity tag")
        });

        let view = gtk::TextView::builder()
            .buffer(&buffer)
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(components::CONTENT_INSET)
            .bottom_margin(components::CONTENT_INSET)
            .left_margin(components::CONTENT_INSET)
            .right_margin(components::CONTENT_INSET)
            .build();

        let scroll = gtk::ScrolledWindow::builder()
            .child(&view)
            .hexpand(true)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();

        root.append(&scroll);

        let state = Rc::new(LogState {
            root,
            view,
            // Right gravity keeps the destination at the end across appends.
            end_mark: buffer.create_mark(None, &buffer.end_iter(), false),
            buffer,
            time_tag,
            level_tags,
            count,
            follow,
            filters,
            scroll: scroll.clone(),
            clear,
            adjustment: scroll.vadjustment(),
            history: RefCell::new(LogHistory::default()),
            filter: Cell::new(LogFilter::ALL),
            applying_filters: Cell::new(false),
            projection: RefCell::new(LogProjection::default()),
            dirty: Cell::new(false),
            refilter_pending: Cell::new(false),
            rendering: Cell::new(false),
            render_source: RefCell::new(None),
            follow_pending: Cell::new(false),
            layout_subscription: RefCell::new(None),
        });

        for (level, button) in &state.filters {
            let weak = Rc::downgrade(&state);
            let level = *level;

            button.connect_toggled(move |button| {
                if let Some(state) = weak.upgrade() {
                    state
                        .filter
                        .set(state.filter.get().with(level, button.is_active()));

                    if !state.applying_filters.get() {
                        state.refilter();
                    }
                }
            });
        }

        let weak = Rc::downgrade(&state);
        state.clear.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                state.history.borrow_mut().clear();
                state.queue_render();
                state.render();
            }
        });

        let weak = Rc::downgrade(&state);
        state.follow.connect_toggled(move |button| {
            if let Some(state) = weak.upgrade() {
                if button.is_active() && state.buffer.has_selection() {
                    state.buffer.place_cursor(&state.buffer.end_iter());
                }

                if button.is_active() {
                    state.follow_latest();
                } else {
                    state.pause_follow();
                }
            }
        });

        let weak = Rc::downgrade(&state);
        state.buffer.connect_has_selection_notify(move |buffer| {
            if buffer.has_selection()
                && let Some(state) = weak.upgrade()
            {
                // Do not display Follow as active while selection pauses it.
                state.follow.set_active(false);
            }
        });

        let weak = Rc::downgrade(&state);
        state.view.connect_map(move |_| {
            if let Some(state) = weak.upgrade() {
                if let Some(clock) = state.view.frame_clock() {
                    let weak = Rc::downgrade(&state);
                    let handler = clock.connect_closure(
                        "layout",
                        true,
                        glib::closure_local!(move |_clock: gtk::gdk::FrameClock| {
                            if let Some(state) = weak.upgrade() {
                                state.finish_follow();
                            }
                        }),
                    );

                    state
                        .layout_subscription
                        .replace(Some(lifecycle::SignalSubscription::new(&clock, handler)));
                }

                state.render();
                state.follow_latest();
            }
        });

        let weak = Rc::downgrade(&state);
        state.view.connect_unmap(move |_| {
            if let Some(state) = weak.upgrade() {
                let source = state.render_source.borrow_mut().take();

                if let Some(source) = source {
                    source.remove();
                }

                state.follow_pending.set(false);
                let subscription = state.layout_subscription.borrow_mut().take();
                drop(subscription);
            }
        });

        let weak = Rc::downgrade(&state);
        state.adjustment.connect_changed(move |_| {
            if let Some(state) = weak.upgrade() {
                state.queue_follow();
            }
        });

        Self(state)
    }

    pub(super) fn root(&self) -> &gtk::Box {
        &self.0.root
    }

    pub(super) fn view(&self) -> &gtk::TextView {
        &self.0.view
    }

    pub(super) fn apply_preferences(
        &self,
        previous: &crate::config::settings::Preferences,
        preferences: &crate::config::settings::Preferences,
        initial: bool,
    ) {
        let previous_filter = self.0.filter.get();
        self.0.applying_filters.set(true);

        for (level, button) in &self.0.filters {
            let (before, after) = match level {
                LogLevel::Info => (previous.log_info, preferences.log_info),
                LogLevel::Warning => (previous.log_warnings, preferences.log_warnings),
                LogLevel::Error => (previous.log_errors, preferences.log_errors),
            };

            if initial || before != after {
                button.set_active(after);
            }
        }

        self.0.applying_filters.set(false);

        if self.0.filter.get() != previous_filter {
            self.0.refilter();
        }

        if initial || previous.log_follow != preferences.log_follow {
            self.0.follow.set_active(preferences.log_follow);
        }

        if initial || previous.log_wrap != preferences.log_wrap {
            self.0.view.set_wrap_mode(if preferences.log_wrap {
                gtk::WrapMode::WordChar
            } else {
                gtk::WrapMode::None
            });

            self.0
                .scroll
                .set_hscrollbar_policy(if preferences.log_wrap {
                    gtk::PolicyType::Never
                } else {
                    gtk::PolicyType::Automatic
                });

            self.0.follow_latest();
        }
    }

    pub(super) fn record(&self, level: LogLevel, title: &str, detail: &str) {
        if title.trim().is_empty() && detail.trim().is_empty() {
            return;
        }

        let time = glib::DateTime::now_local()
            .and_then(|time| time.format("%H:%M:%S"))
            .map(|time| format!("[{time}] "))
            .unwrap_or_else(|_| String::from("[--:--:--] "));

        self.0
            .history
            .borrow_mut()
            .record(level, time, title, detail);

        self.0.queue_render();
    }
}

impl LogState {
    fn queue_render(self: &Rc<Self>) {
        self.dirty.set(true);

        if !self.view.is_mapped() || self.render_source.borrow().is_some() {
            return;
        }

        let weak = Rc::downgrade(self);
        // Coalesce bursts before GTK's text validation and layout idles. The
        // bounded history is the queue, so hidden tabs need no pending copies.
        let source = glib::idle_add_local_full(glib::Priority::HIGH_IDLE, move || {
            if let Some(state) = weak.upgrade() {
                state.render_source.borrow_mut().take();
                state.render();
            }

            glib::ControlFlow::Break
        });

        self.render_source.replace(Some(source));
    }

    fn render(self: &Rc<Self>) {
        if !self.view.is_mapped() || self.rendering.get() || !self.dirty.replace(false) {
            return;
        }

        let source = self.render_source.borrow_mut().take();

        if let Some(source) = source {
            source.remove();
        }

        self.rendering.set(true);
        let reset = self.refilter_pending.replace(false);
        let since = (!reset).then(|| self.projection.borrow().next_id);
        let snapshot = self.history.borrow().snapshot(since, self.filter.get());
        let remove_chars = {
            let mut projection = self.projection.borrow_mut();
            let mut remove_chars = 0;

            while projection
                .entries
                .front()
                .is_some_and(|(id, _)| reset || !snapshot.retains(*id))
            {
                if let Some((_, chars)) = projection.entries.pop_front() {
                    remove_chars += chars;
                }
            }

            projection.next_id = snapshot.next_id;
            projection
                .entries
                .extend(snapshot.entries.iter().map(|entry| (entry.id, entry.chars)));

            remove_chars
        };

        // Release every model borrow before GTK emits synchronous signals.
        // A callback may log again, clear history, or change the filter. Those
        // operations update the model and schedule a subsequent render.
        if remove_chars > 0 {
            self.buffer.delete(
                &mut self.buffer.start_iter(),
                &mut self.buffer.iter_at_offset(remove_chars),
            );
        }

        self.append_visible(&snapshot.entries);

        if self.count.text().as_str() != snapshot.summary {
            self.count.set_text(&snapshot.summary);
        }

        let has_messages = snapshot.retained > 0;

        if self.clear.is_sensitive() != has_messages {
            self.clear.set_sensitive(has_messages);
        }

        if reset || remove_chars > 0 || !snapshot.entries.is_empty() {
            self.follow_latest();
        }

        self.rendering.set(false);

        if self.dirty.get() {
            self.queue_render();
        }
    }

    fn pause_follow(self: &Rc<Self>) {
        self.follow_pending.set(false);
        self.adjustment.set_value(self.adjustment.value());
        let weak = Rc::downgrade(self);

        // Retire any queued mark scroll once GTK has validated the buffer.
        // Otherwise a later append can execute it with Follow already off.
        glib::idle_add_local_full(
            glib::Priority::from(gtk::TEXT_VIEW_PRIORITY_VALIDATE as i32 + 1),
            move || {
                if let Some(state) = weak.upgrade()
                    && !state.follow.is_active()
                    && state.view.is_mapped()
                {
                    let visible = state.view.visible_rect();

                    let (iter, _) = state.view.line_at_y(visible.y() + visible.height() / 2);
                    let mark = state.buffer.create_mark(None, &iter, true);
                    state.view.scroll_to_mark(&mark, 0.0, false, 0.0, 0.0);
                    state.buffer.delete_mark(&mark);

                    state.adjustment.set_value(state.adjustment.value());
                }

                glib::ControlFlow::Break
            },
        );
    }

    fn append_visible(&self, entries: &[Rc<LogEntry>]) {
        if entries.is_empty() {
            return;
        }

        let mut text = String::with_capacity(entries.iter().map(|entry| entry.bytes).sum());

        for entry in entries {
            entry.append_text(&mut text);
        }

        let mut end = self.buffer.end_iter();
        let mut offset = end.offset();
        self.buffer.insert(&mut end, &text);
        self.buffer
            .remove_all_tags(&self.buffer.iter_at_offset(offset), &end);

        for entry in entries {
            let start = self.buffer.iter_at_offset(offset);
            let heading = self.buffer.iter_at_offset(offset + entry.time_chars);
            let body = self
                .buffer
                .iter_at_offset(offset + entry.time_chars + entry.heading_chars);

            self.buffer.apply_tag(&self.time_tag, &start, &heading);
            self.buffer
                .apply_tag(&self.level_tags[entry.level.index()], &heading, &body);

            offset += entry.chars;
        }
    }

    fn refilter(self: &Rc<Self>) {
        // Hidden messages stay in history, not the copyable TextBuffer.
        self.refilter_pending.set(true);
        self.queue_render();
        // Explicit filter changes must also affect an immediate Copy action.
        self.render();
    }

    fn follow_latest(&self) {
        if self.follow.is_active()
            && self.view.is_mapped()
            && self.buffer.selection_bounds().is_none()
        {
            // An adjustment upper bound can still contain estimated heights.
            // A text mark asks GTK to validate the actual scroll destination.
            self.view
                .scroll_to_mark(&self.end_mark, 0.0, true, 0.0, 1.0);

            // Flush text validation during layout, not lazily during snapshot.
            self.view.queue_allocate();
            self.queue_follow();
        }
    }

    fn queue_follow(&self) {
        if self.follow.is_active()
            && self.view.is_mapped()
            && let Some(clock) = self.view.frame_clock()
            && !self.follow_pending.replace(true)
        {
            clock.request_phase(gtk::gdk::FrameClockPhase::LAYOUT);
        }
    }

    fn finish_follow(&self) {
        if !self.follow_pending.replace(false) {
            return;
        }

        if self.follow.is_active()
            && self.view.is_mapped()
            && self.buffer.selection_bounds().is_none()
        {
            // GTK can restore its paragraph anchor after adjustment signals.
            // Commit scrolling after layout, before this frame is painted.
            self.adjustment
                .set_value(self.adjustment.upper() - self.adjustment.page_size());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use history::MAX_LOG_ENTRIES;

    #[test]
    #[ignore = "requires a GTK display"]
    fn follow_paints_the_complete_tail_and_resumes_after_selection() {
        gtk::init().unwrap();
        Theme::graphite().install();
        let log = ApplicationLog::new(&Theme::graphite());
        let window = gtk::Window::builder()
            .default_width(800)
            .default_height(300)
            .child(log.root())
            .build();

        window.present();

        let failures = Rc::new(RefCell::new(Vec::new()));
        let observed = Rc::clone(&failures);
        let state = Rc::downgrade(&log.0);
        let clock = window.frame_clock().unwrap();
        let handler = clock.connect_after_paint(move |_| {
            let Some(state) = state.upgrade() else {
                return;
            };

            if !state.follow.is_active() || !state.view.is_mapped() {
                return;
            }

            let gap =
                state.adjustment.upper() - state.adjustment.page_size() - state.adjustment.value();

            let tail = state.view.iter_location(&state.buffer.end_iter());
            let visible = state.view.visible_rect();
            let hidden_tail = tail.y() + tail.height() + state.view.bottom_margin()
                - visible.y()
                - visible.height();

            let gap = gap.max(f64::from(hidden_tail));

            if gap > 1.0 {
                observed.borrow_mut().push(gap);
            }
        });

        let subscription = lifecycle::SignalSubscription::new(&clock, handler);
        let settle = || {
            let deadline = std::time::Instant::now() + Duration::from_millis(100);
            let context = glib::MainContext::default();

            while std::time::Instant::now() < deadline {
                context.iteration(false);
                std::thread::sleep(Duration::from_millis(1));
            }
        };

        settle();

        let changes = Rc::new(Cell::new(0));
        let observed = Rc::clone(&changes);
        let changed_handler = log.0.buffer.connect_changed(move |_| {
            observed.set(observed.get() + 1);
        });

        let changed_subscription =
            lifecycle::SignalSubscription::new(&log.0.buffer, changed_handler);

        let assert_projection = || {
            let snapshot = log.0.history.borrow().snapshot(None, log.0.filter.get());
            let mut expected = String::new();

            for entry in &snapshot.entries {
                entry.append_text(&mut expected);
            }

            assert_eq!(
                log.0
                    .buffer
                    .text(&log.0.buffer.start_iter(), &log.0.buffer.end_iter(), true),
                expected,
            );

            assert_eq!(log.0.count.text(), snapshot.summary);
            assert_eq!(log.0.clear.is_sensitive(), snapshot.retained > 0);
        };

        for _ in 0..80 {
            log.record(LogLevel::Info, "Paused", "End stepping range");
        }

        assert_eq!(
            changes.get(),
            0,
            "Recording must not synchronously mutate GTK"
        );

        settle();
        assert_eq!(changes.get(), 1, "A burst needs one buffer insertion");
        assert_projection();

        for _ in 0..3 {
            let update = log.clone();
            log.view().add_tick_callback(move |_, _| {
                update.record(
                    LogLevel::Info,
                    "Executing",
                    "Message arriving during a frame",
                );

                glib::ControlFlow::Break
            });

            settle();
        }

        for _ in 0..20 {
            log.record(
                LogLevel::Error,
                "Wrapped entry",
                &"Long message ".repeat(80),
            );
        }

        settle();
        window.set_default_size(520, 220);
        settle();
        log.0.filter.set(
            LogFilter::ALL
                .with(LogLevel::Info, false)
                .with(LogLevel::Warning, false),
        );

        log.0.refilter();
        assert_projection();
        settle();
        changes.set(0);
        log.record(LogLevel::Info, "Hidden severity", "Retained but not copied");
        settle();
        assert_eq!(
            changes.get(),
            0,
            "A hidden severity must not rewrite visible text"
        );

        assert_projection();
        log.0.filter.set(LogFilter::ALL);
        log.0.refilter();
        settle();

        log.record(
            LogLevel::Info,
            "Pending follow",
            "Cancel before the next frame",
        );

        log.0.follow.set_active(false);
        log.0.adjustment.set_value(0.0);
        settle();
        assert_eq!(log.0.adjustment.value(), 0.0);
        log.record(
            LogLevel::Info,
            "Still not following",
            "Keep the reading position",
        );

        settle();
        assert_eq!(log.0.adjustment.value(), 0.0);
        log.0.follow.set_active(true);
        settle();

        log.0
            .buffer
            .select_range(&log.0.buffer.start_iter(), &log.0.buffer.end_iter());

        assert!(!log.0.follow.is_active());
        log.0.adjustment.set_value(0.0);
        log.record(
            LogLevel::Info,
            "Selection paused",
            "Keep the reading position",
        );

        settle();
        assert_eq!(log.0.adjustment.value(), 0.0);
        log.0.follow.set_active(true);
        assert!(!log.0.buffer.has_selection());
        settle();

        log.root().set_visible(false);
        changes.set(0);

        for _ in 0..10 {
            log.record(LogLevel::Info, "Hidden append", "Follow when shown again");
        }

        settle();
        assert_eq!(changes.get(), 0, "A hidden tab must not update its buffer");
        assert!(log.0.render_source.borrow().is_none());
        log.root().set_visible(true);
        settle();
        assert_projection();

        for _ in 0..MAX_LOG_ENTRIES {
            log.record(
                LogLevel::Info,
                "Eviction",
                "Retain the complete newest entry",
            );
        }

        log.record(LogLevel::Info, "Newest", "Complete final message");
        settle();
        assert_projection();
        let tail = log.0.view.iter_location(&log.0.buffer.end_iter());
        let visible = log.0.view.visible_rect();
        assert!(tail.y() + tail.height() <= visible.y() + visible.height());
        assert!(
            failures.borrow().is_empty(),
            "Tail gaps: {:?}",
            failures.borrow()
        );

        log.0.clear.emit_clicked();
        assert_projection();
        let weak = Rc::downgrade(&log.0);
        let invoked = Cell::new(false);
        let reentrant_handler = log.0.buffer.connect_changed(move |_| {
            if invoked.replace(true) {
                return;
            }

            let state = weak.upgrade().unwrap();
            state.clear.emit_clicked();
            state.filter.set(
                LogFilter::ALL
                    .with(LogLevel::Info, false)
                    .with(LogLevel::Warning, false),
            );

            state.refilter();
            ApplicationLog(state).record(LogLevel::Error, "After clear", "é界 preserved");
        });

        let reentrant_subscription =
            lifecycle::SignalSubscription::new(&log.0.buffer, reentrant_handler);

        log.record(
            LogLevel::Info,
            "Removed during insertion",
            "Must not reappear",
        );

        settle();
        assert_projection();
        let snapshot = log.0.history.borrow().snapshot(None, log.0.filter.get());
        assert_eq!(snapshot.retained, 1);
        let entry = &snapshot.entries[0];
        let heading = log.0.buffer.iter_at_offset(entry.time_chars);
        let body = log
            .0
            .buffer
            .iter_at_offset(entry.time_chars + entry.heading_chars);

        assert!(heading.has_tag(&log.0.level_tags[LogLevel::Error.index()]));
        assert!(!body.has_tag(&log.0.level_tags[LogLevel::Error.index()]));
        assert!(!body.has_tag(&log.0.time_tag));

        drop(reentrant_subscription);
        drop(changed_subscription);
        log.record(
            LogLevel::Error,
            "Pending teardown",
            "Do not retain the closed view",
        );

        assert!(log.0.render_source.borrow().is_some());
        drop(subscription);
        window.close();
        let state = Rc::downgrade(&log.0);
        drop(log);
        assert!(state.upgrade().is_none());
    }
}
