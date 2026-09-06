use super::*;
use crate::kernel::KernelMemoryAccounting;

struct MemoryMetric {
    label: &'static str,
    read: fn(&KernelMemoryAccounting) -> Option<u64>,
}

struct MemoryGroup {
    title: &'static str,
    source: &'static str,
    metrics: &'static [MemoryMetric],
}

const GROUPS: &[MemoryGroup] = &[
    MemoryGroup {
        title: "STATM / HTOP",
        source: "/proc/<pid>/statm",
        metrics: &[
            MemoryMetric {
                label: "VIRT (htop)",
                read: |value| value.statm_virtual_bytes,
            },
            MemoryMetric {
                label: "RES (htop)",
                read: |value| value.statm_rss,
            },
        ],
    },
    MemoryGroup {
        title: "FOOTPRINT",
        source: "/proc/<pid>/smaps",
        metrics: &[
            MemoryMetric {
                label: "Virtual (VSS)",
                read: |value| Some(value.virtual_bytes),
            },
            MemoryMetric {
                label: "Resident (RSS)",
                read: |value| Some(value.rss),
            },
            MemoryMetric {
                label: "Not resident (VSS - RSS)",
                read: |value| Some(value.virtual_bytes.saturating_sub(value.rss)),
            },
            MemoryMetric {
                label: "Proportional (PSS)",
                read: |value| Some(value.pss),
            },
            MemoryMetric {
                label: "Process-private (USS)",
                read: |value| Some(value.unique_rss()),
            },
        ],
    },
    MemoryGroup {
        title: "RESIDENT OWNERSHIP",
        source: "/proc/<pid>/smaps",
        metrics: &[
            MemoryMetric {
                label: "Private clean",
                read: |value| Some(value.private_clean),
            },
            MemoryMetric {
                label: "Private dirty",
                read: |value| Some(value.private_dirty),
            },
            MemoryMetric {
                label: "Shared RSS",
                read: |value| Some(value.shared_rss()),
            },
            MemoryMetric {
                label: "Shared clean",
                read: |value| Some(value.shared_clean),
            },
            MemoryMetric {
                label: "Shared dirty",
                read: |value| Some(value.shared_dirty),
            },
        ],
    },
    MemoryGroup {
        title: "BACKING AND RECLAMATION",
        source: "/proc/<pid>/smaps",
        metrics: &[
            MemoryMetric {
                label: "Anonymous",
                read: |value| Some(value.anonymous),
            },
            MemoryMetric {
                label: "Anonymous huge pages",
                read: |value| Some(value.anon_huge_pages),
            },
            MemoryMetric {
                label: "Huge / PMD",
                read: |value| Some(value.huge_bytes()),
            },
            MemoryMetric {
                label: "KSM",
                read: |value| Some(value.ksm),
            },
            MemoryMetric {
                label: "Swap",
                read: |value| Some(value.swap),
            },
            MemoryMetric {
                label: "Referenced",
                read: |value| Some(value.referenced),
            },
            MemoryMetric {
                label: "Lazy free",
                read: |value| Some(value.lazy_free),
            },
            MemoryMetric {
                label: "Locked",
                read: |value| Some(value.locked),
            },
        ],
    },
    MemoryGroup {
        title: "STATUS COUNTERS",
        source: "/proc/<pid>/status",
        metrics: &[
            MemoryMetric {
                label: "Page tables",
                read: |value| Some(value.page_tables),
            },
            MemoryMetric {
                label: "Pinned",
                read: |value| Some(value.pinned),
            },
        ],
    },
];

#[derive(Clone, Copy)]
enum MemoryUnit {
    KiB,
    MiB,
    GiB,
}

impl MemoryUnit {
    fn selected(index: u32) -> Self {
        match index {
            1 => Self::MiB,
            2 => Self::GiB,
            _ => Self::KiB,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::KiB => "KiB",
            Self::MiB => "MiB",
            Self::GiB => "GiB",
        }
    }

    fn format(self, bytes: u64) -> String {
        let (scale, decimals) = match self {
            Self::KiB => (1024, 2),
            Self::MiB => (1024 * 1024, 3),
            Self::GiB => (1024 * 1024 * 1024, 6),
        };

        format_scaled_binary(bytes, scale, decimals)
    }
}

#[derive(Clone)]
pub(in crate::ui) struct MemorySummary {
    pub(super) root: gtk::Box,
    meta: gtk::Label,
    unit: gtk::DropDown,
    rows: Rc<[MemoryRow]>,
}

struct MemoryRow {
    metric: &'static MemoryMetric,
    source: &'static str,
    value: gtk::Label,
    pages: gtk::Label,
    last_value: Cell<Option<(Option<u64>, u64)>>,
}

impl MemorySummary {
    pub(super) fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
        root.add_css_class("kernel-memory-summary");
        root.set_vexpand(true);
        let header = components::card();
        let controls = components::control_row();
        let description = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
        description.set_hexpand(true);
        description.set_valign(gtk::Align::Center);
        let title = section_title("MEMORY ACCOUNTING");
        description.append(&title);
        let unit_label = gtk::Label::new(Some("Units"));
        unit_label.add_css_class("muted");
        let unit = gtk::DropDown::from_strings(&["KiB", "MiB", "GiB"]);
        unit.set_valign(gtk::Align::Center);
        unit.set_tooltip_text(Some(
            "Display units. Changes use the current snapshot without querying the target",
        ));

        let meta = gtk::Label::new(Some("No snapshot"));
        meta.add_css_class("muted");
        meta.set_xalign(0.0);
        meta.set_wrap(true);
        meta.set_wrap_mode(pango::WrapMode::WordChar);
        meta.set_width_chars(1);
        enable_stable_text_selection(&meta);
        description.append(&meta);
        controls.append(&description);
        controls.append(&unit_label);
        controls.append(&unit);
        header.append(&controls);
        root.append(&header);
        let content = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
        let columns =
            std::array::from_fn::<_, 3, _>(|_| gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal));

        let mut rows = Vec::new();
        let mut unit_headers = Vec::new();

        for group in GROUPS {
            let card = components::card();
            card.add_css_class("kernel-memory-group");
            card.set_spacing(0);
            let grid = gtk::Grid::new();
            grid.set_hexpand(true);
            let heading = gtk::Box::new(gtk::Orientation::Vertical, 2);
            heading.add_css_class("kernel-memory-group-heading");
            heading.append(&section_title(group.title));
            let source = gtk::Label::new(Some(group.source));
            source.add_css_class("muted");
            source.set_xalign(0.0);
            enable_stable_text_selection(&source);
            heading.append(&source);
            columns[0].add_widget(&heading);
            grid.attach(&heading, 0, 0, 1, 1);

            for (column, title) in [(1, "KiB"), (2, "Page equiv.")] {
                let label = memory_cell(title, column, &columns);
                label.add_css_class("kernel-memory-column-heading");
                label.add_css_class("muted");
                grid.attach(&label, column as i32, 0, 1, 1);

                if column == 1 {
                    unit_headers.push(label);
                } else {
                    label.set_tooltip_text(Some("Base-page equivalents, not physical page allocations. Fractional counts are retained"));
                }
            }

            let divider = gtk::Separator::new(gtk::Orientation::Horizontal);
            divider.add_css_class("kernel-memory-divider");
            grid.attach(&divider, 0, 1, 3, 1);

            for (index, metric) in group.metrics.iter().enumerate() {
                let row = (index + 2) as i32;
                let name = memory_cell(metric.label, 0, &columns);
                name.set_tooltip_text(Some(&format!("{}\nSource: {}", metric.label, group.source)));
                let value = memory_cell("-", 1, &columns);
                let pages = memory_cell("-", 2, &columns);

                for cell in [&name, &value, &pages] {
                    if index == 0 {
                        cell.add_css_class("kernel-memory-first-row");
                    }

                    if index + 1 == group.metrics.len() {
                        cell.add_css_class("kernel-memory-last-row");
                    }
                }

                grid.attach(&name, 0, row, 1, 1);
                grid.attach(&value, 1, row, 1, 1);
                grid.attach(&pages, 2, row, 1, 1);

                rows.push(MemoryRow {
                    metric,
                    source: group.source,
                    value,
                    pages,
                    last_value: Cell::new(None),
                });
            }

            card.append(&grid);
            content.append(&card);
        }

        let scroll = gtk::ScrolledWindow::builder()
            .child(&content)
            .vexpand(true)
            .overlay_scrolling(false)
            .build();

        configure_content_scroller(&scroll);
        root.append(&scroll);
        let rows: Rc<[MemoryRow]> = rows.into();
        let rows_for_unit = Rc::downgrade(&rows);

        unit.connect_selected_notify(move |selector| {
            let Some(rows) = rows_for_unit.upgrade() else {
                return;
            };

            let unit = MemoryUnit::selected(selector.selected());

            for header in &unit_headers {
                set_label_text(header, unit.label());
            }

            for row in &*rows {
                row.render_value(unit);
            }
        });

        Self {
            root,
            meta,
            unit,
            rows,
        }
    }

    pub(super) fn update(&self, accounting: &KernelMemoryAccounting, mapping_count: usize) {
        let unit = MemoryUnit::selected(self.unit.selected());

        for row in &*self.rows {
            row.update((row.metric.read)(accounting), accounting.page_size, unit);
        }

        set_label_text(
            &self.meta,
            &format!(
                "{} VMAs  base page {} ({} bytes)",
                format_grouped_count(mapping_count as u64),
                crate::kernel::format_bytes(accounting.page_size),
                format_grouped_count(accounting.page_size),
            ),
        );
    }

    pub(super) fn clear(&self) {
        set_label_text(&self.meta, "No snapshot");
        let unit = MemoryUnit::selected(self.unit.selected());

        for row in &*self.rows {
            row.update(None, 0, unit);
        }
    }
}

impl MemoryRow {
    fn update(&self, bytes: Option<u64>, page_size: u64, unit: MemoryUnit) {
        if self.last_value.replace(Some((bytes, page_size))) == Some((bytes, page_size)) {
            return;
        }

        self.render_value(unit);
        let pages = bytes.map_or_else(
            || String::from("-"),
            |bytes| format_page_equivalents(bytes, page_size),
        );

        set_label_text(&self.pages, &pages);
        let mut tooltip = format!("{}\nSource: {}", self.metric.label, self.source);

        if let Some(bytes) = bytes {
            tooltip.push_str(&format!(
                "\n{} bytes\n{} KiB\n{} MiB\n{} GiB\n{pages} base-page equivalents",
                format_grouped_count(bytes),
                MemoryUnit::KiB.format(bytes),
                MemoryUnit::MiB.format(bytes),
                MemoryUnit::GiB.format(bytes),
            ));
        }

        for label in [&self.value, &self.pages] {
            label.set_tooltip_text(Some(&tooltip));
            set_css_class(label, "muted", bytes.is_none_or(|bytes| bytes == 0));
        }
    }

    fn render_value(&self, unit: MemoryUnit) {
        let bytes = self.last_value.get().and_then(|(bytes, _)| bytes);
        let text = bytes.map_or_else(|| String::from("-"), |bytes| unit.format(bytes));
        set_label_text(&self.value, &text);
    }
}

fn memory_cell(text: &str, column: usize, columns: &[gtk::SizeGroup; 3]) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("kernel-memory-cell");
    label.set_halign(gtk::Align::Fill);
    label.set_xalign(if column == 0 { 0.0 } else { 1.0 });
    label.set_hexpand(column == 0);
    label.set_width_chars(if column == 0 { 23 } else { 12 });
    enable_stable_text_selection(&label);
    columns[column].add_widget(&label);

    label
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display"]
    fn memory_summary_retains_metrics_units_and_alignment_without_rewriting_unchanged_values() {
        gtk::init().unwrap();
        Theme::graphite().install();
        let summary = MemorySummary::new();
        let accounting = KernelMemoryAccounting {
            page_size: 4096,
            virtual_bytes: 12288,
            rss: 8192,
            pss: 6144,
            private_clean: 512,
            private_dirty: 1024,
            shared_clean: 2048,
            shared_dirty: 4096,
            anonymous: 1024,
            anon_huge_pages: 2048,
            file_pmd_mapped: 4096,
            shmem_pmd_mapped: 8192,
            private_hugetlb: 16384,
            shared_hugetlb: 32768,
            ksm: 3072,
            swap: 5120,
            referenced: 7168,
            lazy_free: 9216,
            locked: 11264,
            statm_virtual_bytes: Some(14336),
            statm_rss: None,
            page_tables: 13312,
            pinned: 15360,
            ..KernelMemoryAccounting::default()
        };

        summary.update(&accounting, 42);
        let expected = [
            Some(14336),
            None,
            Some(12288),
            Some(8192),
            Some(4096),
            Some(6144),
            Some(1536),
            Some(512),
            Some(1024),
            Some(6144),
            Some(2048),
            Some(4096),
            Some(1024),
            Some(2048),
            Some(63488),
            Some(3072),
            Some(5120),
            Some(7168),
            Some(9216),
            Some(11264),
            Some(13312),
            Some(15360),
        ];

        assert_eq!(summary.rows.len(), expected.len());

        for (row, bytes) in summary.rows.iter().zip(expected) {
            assert_eq!(
                row.last_value.get(),
                Some((bytes, 4096)),
                "{}",
                row.metric.label
            );
        }

        let changes = Rc::new(Cell::new(0));

        for row in &*summary.rows {
            for label in [&row.value, &row.pages] {
                let changes = Rc::clone(&changes);
                label.connect_label_notify(move |_| changes.set(changes.get() + 1));
            }
        }

        summary.update(&accounting, 42);
        assert_eq!(changes.get(), 0);
        assert_eq!(summary.rows[0].metric.label, "VIRT (htop)");
        assert_eq!(summary.rows[1].metric.label, "RES (htop)");
        assert_eq!(summary.rows[6].pages.text(), "0.38");
        let virtual_row = &summary.rows[2];

        for (selected, value) in [(0, "12"), (1, "0.012"), (2, "0.000011")] {
            summary.unit.set_selected(selected);
            assert_eq!(virtual_row.value.text(), value);
            assert_eq!(virtual_row.pages.text(), "3");
            assert_eq!(summary.rows[1].value.text(), "-");
        }

        let tooltip = virtual_row.value.tooltip_text().unwrap();
        assert!(tooltip.contains("12,288 bytes"));
        assert!(tooltip.contains("12 KiB"));
        assert!(tooltip.contains("0.012 MiB"));
        assert!(tooltip.contains("0.000011 GiB"));
        let window = gtk::Window::builder()
            .default_width(520)
            .default_height(650)
            .child(&summary.root)
            .build();

        window.present();
        let scroll = summary
            .root
            .last_child()
            .unwrap()
            .downcast::<gtk::ScrolledWindow>()
            .unwrap();

        for width in [520, 900, 360] {
            window.set_default_size(width, 650);
            let deadline = Instant::now() + Duration::from_millis(100);

            while Instant::now() < deadline {
                glib::MainContext::default().iteration(false);
                std::thread::sleep(Duration::from_millis(1));
            }

            if width >= 520 {
                let adjustment = scroll.hadjustment();
                assert!(adjustment.upper() <= adjustment.page_size() + 1.0);
            }

            let first = &summary.rows[0];
            let value_x = first.value.compute_bounds(&summary.root).unwrap().x();
            let pages_x = first.pages.compute_bounds(&summary.root).unwrap().x();

            for row in &*summary.rows {
                let value = row.value.compute_bounds(&summary.root).unwrap();
                let pages = row.pages.compute_bounds(&summary.root).unwrap();
                assert!((value.x() - value_x).abs() <= 1.0);
                assert!((pages.x() - pages_x).abs() <= 1.0);
                assert!(value.x() + value.width() <= pages.x() + 1.0);
            }

            let mut offset = 0;

            for group in GROUPS {
                let first = &summary.rows[offset].value;
                let last = &summary.rows[offset + group.metrics.len() - 1].value;
                let grid = first.parent().unwrap().downcast::<gtk::Grid>().unwrap();
                let card = grid.parent().unwrap();
                let divider = grid.child_at(0, 1).unwrap();
                let line = divider.compute_bounds(&card).unwrap();
                assert!(line.x().abs() <= 1.0);
                assert!((line.width() - card.width() as f32).abs() <= 1.0);
                let heading = grid.child_at(0, 0).unwrap();
                let title = heading.first_child().unwrap();
                let top = title.compute_bounds(&card).unwrap().y();
                let source = heading.last_child().unwrap();
                let source_bounds = source.compute_bounds(&card).unwrap();
                let above = line.y() - source_bounds.y() - source_bounds.height();
                let first_bounds = first.compute_bounds(&card).unwrap();
                let last_bounds = last.compute_bounds(&card).unwrap();
                let below = first_bounds.y()
                    + (first_bounds.height() - first.layout().pixel_size().1 as f32) / 2.0
                    - line.y()
                    - line.height();

                let bottom = card.height() as f32
                    - last_bounds.y()
                    - (last_bounds.height() + last.layout().pixel_size().1 as f32) / 2.0;

                assert!((above - 8.0).abs() <= 1.0, "header inset {above}");
                assert!((top - above).abs() <= 1.0, "header insets {top}/{above}");
                assert!(
                    (below - above).abs() <= 1.0,
                    "divider insets {above}/{below}"
                );

                assert!(
                    (bottom - below).abs() <= 1.0,
                    "body insets {below}/{bottom}"
                );

                offset += group.metrics.len();
            }
        }

        summary.clear();
        assert_eq!(summary.meta.text(), "No snapshot");
        assert!(
            summary
                .rows
                .iter()
                .all(|row| row.value.text() == "-" && row.pages.text() == "-")
        );

        summary.unit.set_selected(0);
        summary.update(&accounting, 42);
        assert_eq!(virtual_row.value.text(), "12");
        let rows = Rc::downgrade(&summary.rows);
        window.close();
        window.set_child(None::<&gtk::Widget>);
        drop(scroll);
        drop(summary);
        assert!(rows.upgrade().is_none());
    }
}
