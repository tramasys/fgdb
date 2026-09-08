use super::*;

pub(crate) struct MemoryWatchRequest {
    pub id: u64,
    pub revision: u64,
    pub expression: String,
    pub byte_offset: i64,
    pub byte_count: usize,
}

impl MemoryWatchRequest {
    pub(crate) fn command(&self) -> String {
        format!(
            "-data-read-memory-bytes -o {} {} {}",
            self.byte_offset,
            crate::debugger::quote(&self.expression),
            self.byte_count,
        )
    }
}

#[derive(Clone, Copy)]
enum Unit {
    Bytes,
    KiB,
    MiB,
    Pages,
}

const UNITS: [(Unit, &str); 4] = [
    (Unit::Bytes, "Bytes"),
    (Unit::KiB, "KiB"),
    (Unit::MiB, "MiB"),
    (Unit::Pages, "Pages"),
];

#[derive(Clone)]
pub(in crate::ui) struct Controls {
    pub root: gtk::FlowBox,
    pub(super) base: gtk::Button,
    pub(super) previous: gtk::Button,
    pub(super) next: gtk::Button,
    pub(super) distance: gtk::Entry,
    pub(super) units: gtk::DropDown,
    pub(super) backward: gtk::Button,
    pub(super) forward: gtk::Button,
}

impl Controls {
    pub(super) fn new(byte_count: usize) -> Self {
        let root = components::action_flow();
        root.set_max_children_per_line(4);
        root.set_halign(gtk::Align::Start);
        let previous = arrow_button("go-previous-symbolic", "Read the preceding inspector page");
        let base = memory_toolbar_button("Base", "Return to the original expression");
        base.set_sensitive(false);
        let next = arrow_button("go-next-symbolic", "Read the following inspector page");

        let pages = components::control_group(
            "PAGE",
            &[
                previous.clone().upcast(),
                base.clone().upcast(),
                next.clone().upcast(),
            ],
        );

        pages.set_halign(gtk::Align::Start);
        root.insert(&pages, -1);

        let distance = gtk::Entry::builder()
            .text("1")
            .width_chars(8)
            .max_width_chars(8)
            .max_length(20)
            .tooltip_text("Positive decimal or hexadecimal distance, such as 16 or 0x1000. Enter jumps forward")
            .build();

        let units = gtk::DropDown::from_strings(&UNITS.map(|(_, label)| label));
        units.set_selected(3);
        units.set_tooltip_text(Some(&format!(
            "One inspector page is {byte_count} bytes, not the operating system's page size"
        )));

        let backward = arrow_button(
            "go-previous-symbolic",
            "Jump backward by the configured distance",
        );

        let forward = arrow_button(
            "go-next-symbolic",
            "Jump forward by the configured distance",
        );

        let jump = components::control_group(
            "JUMP",
            &[
                backward.clone().upcast(),
                distance.clone().upcast(),
                units.clone().upcast(),
                forward.clone().upcast(),
            ],
        );

        jump.set_halign(gtk::Align::Start);
        root.insert(&jump, -1);

        Self {
            root,
            base,
            previous,
            next,
            distance,
            units,
            backward,
            forward,
        }
    }

    pub(super) fn connect(
        &self,
        id: u64,
        watches: &Rc<RefCell<Vec<MemoryWatchView>>>,
        handler: &Rc<RefCell<Option<MemoryWatchHandler>>>,
        available: &Rc<Cell<bool>>,
    ) {
        connect(&self.base, id, watches, handler, available, |_| Ok(0));

        for (button, backward) in [(&self.previous, true), (&self.next, false)] {
            connect(button, id, watches, handler, available, move |watch| {
                let distance =
                    i64::try_from(watch.byte_count).map_err(|_| "Page size is too large")?;

                destination(watch.byte_offset.get(), distance, backward)
            });
        }

        for (button, backward) in [(&self.backward, true), (&self.forward, false)] {
            let distance = self.distance.clone();
            let units = self.units.clone();

            connect(button, id, watches, handler, available, move |watch| {
                let unit = UNITS
                    .get(units.selected() as usize)
                    .ok_or("Select a jump unit")?
                    .0;

                let bytes = parse_distance(&distance.text(), unit, watch.byte_count)?;
                destination(watch.byte_offset.get(), bytes, backward)
            });
        }

        let forward = self.forward.downgrade();

        self.distance.connect_activate(move |_| {
            if let Some(forward) = forward.upgrade() {
                forward.emit_clicked();
            }
        });
    }

    pub(super) fn update_offset(&self, offset: i64) {
        self.base.set_sensitive(offset != 0);
    }
}

fn arrow_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.add_css_class("memory-navigation-button");
    button.set_tooltip_text(Some(tooltip));
    button.update_property(&[gtk::accessible::Property::Label(tooltip)]);

    button
}

fn connect(
    button: &gtk::Button,
    id: u64,
    watches: &Rc<RefCell<Vec<MemoryWatchView>>>,
    handler: &Rc<RefCell<Option<MemoryWatchHandler>>>,
    available: &Rc<Cell<bool>>,
    target: impl Fn(&MemoryWatchView) -> Result<i64, &'static str> + 'static,
) {
    let watches = Rc::downgrade(watches);
    let handler = Rc::clone(handler);
    let available = Rc::clone(available);

    button.connect_clicked(move |_| {
        let Some(watches) = watches.upgrade() else {
            return;
        };

        let watch = watches
            .borrow()
            .iter()
            .find(|watch| watch.id == id)
            .cloned();

        let Some(watch) = watch else { return };

        let result = if available.get() {
            target(&watch)
        } else {
            Err("Pause the target before navigating memory")
        };

        match result {
            Ok(offset) => {
                watch.byte_offset.set(offset);
                update_memory_watch_offset(&watch);
                request_memory_watch(&watch, &handler);
            }
            Err(error) => {
                watch.status.add_css_class("memory-watch-error");
                watch.status.set_text(error);
                watch.status.set_tooltip_text(Some(error));
            }
        }
    });
}

fn parse_distance(input: &str, unit: Unit, byte_count: usize) -> Result<i64, &'static str> {
    let input = input.trim();
    let (digits, radix) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
        .map_or((input, 10), |digits| (digits, 16));

    if digits.is_empty()
        || !digits.bytes().all(|byte| match radix {
            16 => byte.is_ascii_hexdigit(),
            _ => byte.is_ascii_digit(),
        })
    {
        return Err("Enter a positive decimal or hexadecimal jump distance");
    }

    let amount = u64::from_str_radix(digits, radix).map_err(|_| "Jump distance is too large")?;

    if amount == 0 {
        return Err("Jump distance must be greater than zero");
    }

    let multiplier = match unit {
        Unit::Bytes => 1,
        Unit::KiB => 1024,
        Unit::MiB => 1024 * 1024,
        Unit::Pages => byte_count as u64,
    };

    amount
        .checked_mul(multiplier)
        .and_then(|bytes| i64::try_from(bytes).ok())
        .ok_or("Jump distance is too large")
}

fn destination(offset: i64, distance: i64, backward: bool) -> Result<i64, &'static str> {
    if backward {
        offset.checked_sub(distance)
    } else {
        offset.checked_add(distance)
    }
    .ok_or("Jump exceeds the supported byte offset range")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jump_distances_are_exact_and_checked() {
        assert_eq!(parse_distance("0x10", Unit::Pages, 256), Ok(4096));
        assert_eq!(parse_distance("2", Unit::KiB, 128), Ok(2048));
        assert_eq!(parse_distance("3", Unit::MiB, 128), Ok(3 * 1024 * 1024));
        assert_eq!(parse_distance(" 0Xff ", Unit::Bytes, 128), Ok(255));

        for input in [
            "",
            "0",
            "-1",
            "+1",
            "1.5",
            "0x",
            "$rsp",
            "1+2",
            "18446744073709551616",
        ] {
            assert!(parse_distance(input, Unit::Bytes, 256).is_err(), "{input}");
        }

        assert!(parse_distance("9223372036854775807", Unit::Pages, 256).is_err());
        assert_eq!(destination(512, 1024, true), Ok(-512));
        assert_eq!(destination(-512, 1024, false), Ok(512));
        assert!(destination(i64::MAX, 1, false).is_err());
        assert!(destination(i64::MIN, 1, true).is_err());
    }
}
