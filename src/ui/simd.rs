//! Shared SIMD presentation and an editor over a bounded, bit-exact draft.

use super::*;
use crate::debugger::vector::{VectorValue, VectorWrite};
use std::fmt::Write as _;

mod editor;
pub(super) use editor::open_vector_editor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VectorBase {
    Hexadecimal,
    Signed,
    Unsigned,
    Binary,
    Octal,
}

impl VectorBase {
    const ALL: [Self; 5] = [
        Self::Hexadecimal,
        Self::Signed,
        Self::Unsigned,
        Self::Binary,
        Self::Octal,
    ];

    fn from_index(index: u32) -> Self {
        Self::ALL
            .get(index as usize)
            .copied()
            .unwrap_or(Self::Hexadecimal)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Hexadecimal => "Hexadecimal",
            Self::Signed => "Signed decimal",
            Self::Unsigned => "Unsigned decimal",
            Self::Binary => "Binary",
            Self::Octal => "Octal",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct VectorDisplay {
    format: VectorLaneFormat,
    base: VectorBase,
}

impl Default for VectorDisplay {
    fn default() -> Self {
        Self {
            format: VectorLaneFormat::Int64,
            base: VectorBase::Hexadecimal,
        }
    }
}

impl VectorDisplay {
    fn integer(self) -> (value::IntegerFormat, IntegerRadix) {
        let bits = (self.format.lane_bytes() * 8) as u32;
        let format = if self.base != VectorBase::Unsigned {
            value::IntegerFormat::signed(bits)
        } else {
            value::IntegerFormat::unsigned(bits)
        };

        let radix = match self.base {
            VectorBase::Signed | VectorBase::Unsigned => IntegerRadix::Decimal,
            VectorBase::Binary => IntegerRadix::Binary,
            VectorBase::Octal => IntegerRadix::Octal,
            VectorBase::Hexadecimal => IntegerRadix::Hexadecimal,
        };

        (format, radix)
    }

    fn text(self, raw: u64) -> String {
        if self.format.is_float() {
            let bytes = raw.to_be_bytes();
            let width = self.format.lane_bytes();
            let text = format_float_value(
                &bytes[8 - width..],
                (width * 8) as u32,
                FloatRepresentation::Decimal,
            );

            if text.len() > 20 {
                format_float_value(
                    &bytes[8 - width..],
                    (width * 8) as u32,
                    FloatRepresentation::Scientific,
                )
            } else {
                text
            }
        } else {
            let (format, radix) = self.integer();
            format_integer_value(u128::from(raw), format, radix)
        }
    }

    fn parse(self, text: &str) -> Result<u64, &'static str> {
        if self.format.is_float() {
            // Parse directly at the destination width to avoid double rounding.
            if self.format == VectorLaneFormat::Float32 {
                text.trim()
                    .parse::<f32>()
                    .map(|value| u64::from(value.to_bits()))
                    .map_err(|_| "Enter a 32-bit float, inf or nan")
            } else {
                text.trim()
                    .parse::<f64>()
                    .map(f64::to_bits)
                    .map_err(|_| "Enter a 64-bit float, inf or nan")
            }
        } else {
            let (format, radix) = self.integer();
            parse_integer_input(text, format, radix).map(|raw| raw as u64)
        }
    }

    pub(super) fn summary(self, register: &Register) -> String {
        let Some(value) = VectorValue::parse(&register.name, &register.value) else {
            if let Some((end, _)) = register.value.char_indices().nth(256) {
                return format!(
                    "{}...\nRaw layout - open View / edit for the full value",
                    &register.value[..end]
                );
            }

            return register.value.clone();
        };

        let count = value.bytes() / self.format.lane_bytes();
        let mut text = String::with_capacity(count * 28);
        let values = (0..count)
            .map(|index| self.text(value.lane(index, self.format.lane_bytes()).unwrap()))
            .collect::<Vec<_>>();
        let width = values.iter().map(String::len).max().unwrap_or(0);

        for (index, value) in values.iter().enumerate() {
            if index != 0 {
                text.push_str("   ");
            }

            // Keep the lane index and value together when GTK wraps the row.
            // Pad outside the brackets to align values for two-digit indices.
            let padding = if count > 10 && index < 10 {
                "\u{a0}"
            } else {
                ""
            };

            let _ = write!(text, "[{index}]{padding}\u{a0}{value:\u{a0}>width$}");
        }

        text
    }
}

#[derive(Clone)]
pub(super) struct VectorControls {
    pub(super) root: gtk::Box,
    interpretation: gtk::DropDown,
    base: gtk::DropDown,
}

impl VectorControls {
    pub(super) fn new() -> Self {
        let root = components::control_row();
        root.add_css_class("simd-controls");
        let interpretation =
            gtk::DropDown::from_strings(&VectorLaneFormat::ALL.map(VectorLaneFormat::label));

        interpretation.set_hexpand(true);
        interpretation.set_selected(3);
        interpretation.set_tooltip_text(Some(
            "Lane interpretation, starting at the least significant bits",
        ));

        let base = gtk::DropDown::from_strings(&VectorBase::ALL.map(VectorBase::label));
        base.set_hexpand(true);
        base.set_tooltip_text(Some("Integer representation"));
        root.append(&interpretation);
        root.append(&base);

        Self {
            root,
            interpretation,
            base,
        }
    }

    pub(super) fn display(&self) -> VectorDisplay {
        VectorDisplay {
            format: VectorLaneFormat::from_index(self.interpretation.selected()),
            base: VectorBase::from_index(self.base.selected()),
        }
    }

    fn set_display(&self, display: VectorDisplay) {
        self.interpretation.set_selected(
            VectorLaneFormat::ALL
                .iter()
                .position(|format| *format == display.format)
                .unwrap_or(3) as u32,
        );

        self.base.set_selected(
            VectorBase::ALL
                .iter()
                .position(|base| *base == display.base)
                .unwrap_or(0) as u32,
        );

        self.base.set_sensitive(!display.format.is_float());
    }

    fn connect_table(&self, store: &gio::ListStore, list: &VectorRegisterList) {
        for control in [&self.interpretation, &self.base] {
            let store = store.downgrade();
            let list = list.clone();
            let interpretation = self.interpretation.downgrade();
            let base = self.base.downgrade();

            control.connect_selected_notify(move |_| {
                let (Some(store), Some(interpretation), Some(base)) =
                    (store.upgrade(), interpretation.upgrade(), base.upgrade())
                else {
                    return;
                };

                let display = VectorDisplay {
                    format: VectorLaneFormat::from_index(interpretation.selected()),
                    base: VectorBase::from_index(base.selected()),
                };

                base.set_sensitive(!display.format.is_float());

                let rows = (0..store.n_items())
                    .filter_map(|index| {
                        let object = store.item(index)?.downcast::<glib::BoxedAnyObject>().ok()?;
                        let mut row = object.borrow::<RegisterRowData>().clone();
                        row.vector_display = display;
                        Some(row)
                    })
                    .collect::<Vec<_>>();

                replace_rows(&store, &list, rows);
            });
        }
    }
}

pub(super) fn build_register_group(title: &str) -> RegisterGroupView {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(false)
        .css_classes(["simd-register-list"])
        .build();

    let rendered = gio::ListStore::new::<glib::BoxedAnyObject>();
    list.bind_model(Some(&rendered), register_row);
    let view = VectorRegisterList {
        list: list.clone(),
        rendered,
    };
    let source = store.downgrade();
    let rendered = view.rendered.downgrade();

    list.connect_map(move |list| {
        let list = list.downgrade();
        let source = source.clone();
        let rendered = rendered.clone();

        glib::idle_add_local_once(move || {
            if let (Some(list), Some(source), Some(rendered)) =
                (list.upgrade(), source.upgrade(), rendered.upgrade())
            {
                VectorRegisterList { list, rendered }.sync(&source);
            }
        });
    });

    let controls = VectorControls::new();
    controls.connect_table(&store, &view);
    let inspect = gtk::Button::with_label("View / edit");
    inspect.set_tooltip_text(Some("Inspect or edit the selected vector register"));
    let weak_list = list.downgrade();

    inspect.connect_clicked(move |_| {
        if let Some(list) = weak_list.upgrade()
            && let Some(row) = list.selected_row()
        {
            list.emit_by_name::<()>("row-activated", &[&row]);
        }
    });

    controls.root.append(&inspect);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&controls.root);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    header.add_css_class("simd-column-header");
    let name = register_name_label("REGISTER");
    name.remove_css_class("register-name");
    header.append(&name);
    let lanes = gtk::Label::builder()
        .label("LANES / LOW TO HIGH")
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["simd-lanes"])
        .build();

    header.append(&lanes);
    body.append(&header);
    body.append(&list);
    let panel = build_disclosure(title, &body, false, "register-disclosure");
    panel.add_css_class("register-group-panel");
    panel.set_visible(false);

    RegisterGroupView {
        kind: RegisterGroupKind::Vector,
        store,
        view: RegisterGroupWidget::Vector(view),
        panel,
        vector_controls: Some(controls),
    }
}

fn register_row(object: &glib::Object) -> gtk::Widget {
    let object = object.downcast_ref::<glib::BoxedAnyObject>().unwrap();
    let row = object.borrow::<RegisterRowData>();
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let name = register_name_label(&format!("${}", row.register.name));

    if row.changed {
        name.add_css_class("modified-register");
    }

    body.append(&name);

    let value = gtk::Label::builder()
        .label(row.vector_display.summary(&row.register))
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .width_chars(1)
        .max_width_chars(64)
        .hexpand(true)
        .css_classes(["simd-lanes"])
        .build();

    value.set_tooltip_text(Some(
        "Lane 0 contains the least significant bits. Double-click the register name or use View / edit.",
    ));

    value.add_css_class(register_value_css(
        &row.register,
        row.architecture,
        row.endian,
        row.pointer_bits,
    ));

    enable_stable_text_selection(&value);
    body.append(&value);
    body.upcast()
}

fn register_name_label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .width_request(86)
        .css_classes(["simd-lanes", "register-name"])
        .build()
}

pub(super) fn replace_rows(
    store: &gio::ListStore,
    list: &VectorRegisterList,
    rows: Vec<RegisterRowData>,
) {
    if replace_boxed_store_if_changed(store, rows) {
        list.sync(store);
    }
}

#[derive(Clone)]
pub(super) struct VectorRegisterList {
    pub(super) list: gtk::ListBox,
    rendered: gio::ListStore,
}

impl VectorRegisterList {
    pub(super) fn connect_activate(&self, source: &gio::ListStore, action: impl Fn(u32) + 'static) {
        let source = source.downgrade();
        let rendered = self.rendered.downgrade();

        self.list.connect_row_activated(move |_, row| {
            let (Some(source), Some(rendered)) = (source.upgrade(), rendered.upgrade()) else {
                return;
            };

            let Ok(index) = u32::try_from(row.index()) else {
                return;
            };

            let item = source.item(index);

            // A mapped pane may still be waiting for its deferred presentation.
            // Never interpret an old row's position as a different live register.
            if item.is_some() && item == rendered.item(index) {
                action(index);
            }
        });
    }

    fn sync(&self, source: &gio::ListStore) {
        // Hidden SIMD sections retain their presentation without formatting or
        // creating widgets on each stop. Mapping consumes the latest snapshot.
        if !self.list.is_mapped() {
            return;
        }

        let selected = self
            .list
            .selected_row()
            .and_then(|row| self.rendered.item(row.index() as u32))
            .and_downcast::<glib::BoxedAnyObject>()
            .map(|row| row.borrow::<RegisterRowData>().register.name.clone());

        let mut position = None;

        for index in 0..source.n_items() {
            let Some(object) = source.item(index).and_downcast::<glib::BoxedAnyObject>() else {
                continue;
            };

            if selected
                .as_ref()
                .is_some_and(|name| *name == object.borrow::<RegisterRowData>().register.name)
            {
                position = Some(index);
            }

            if self.rendered.item(index).as_ref() != Some(object.upcast_ref()) {
                self.rendered
                    .splice(index, u32::from(index < self.rendered.n_items()), &[object]);
            }
        }

        if self.rendered.n_items() > source.n_items() {
            self.rendered.splice(
                source.n_items(),
                self.rendered.n_items() - source.n_items(),
                &[] as &[glib::BoxedAnyObject],
            );
        }

        if let Some(position) = position {
            self.list
                .select_row(self.list.row_at_index(position as i32).as_ref());
        }
    }
}
