use super::*;
use crate::config::settings::IntegerDisplay;

pub(super) struct VariablePresentation {
    format: Cell<IntegerDisplay>,
    pointer_bits: Rc<Cell<u32>>,
    items: RefCell<Vec<glib::WeakRef<gtk::ListItem>>>,
}

impl VariablePresentation {
    pub fn new(format: IntegerDisplay, pointer_bits: Rc<Cell<u32>>) -> Rc<Self> {
        Rc::new(Self {
            format: Cell::new(format),
            pointer_bits,
            items: RefCell::new(Vec::new()),
        })
    }

    pub fn register(&self, item: &gtk::ListItem) {
        let mut items = self.items.borrow_mut();

        // Amortize pruning across capacity growth instead of scanning every
        // existing cell each time GTK realizes another row.
        if items.len() == items.capacity() {
            items.retain(|item| item.upgrade().is_some());
        }

        items.push(item.downgrade());
    }

    pub fn display(&self, variable: &Variable, value: &str, details: &str) -> String {
        variable_display_value(
            variable,
            value,
            details,
            self.pointer_bits.get(),
            self.format.get(),
        )
    }

    pub fn set_format(&self, format: IntegerDisplay) {
        if self.format.replace(format) == format {
            return;
        }

        // Only realized value cells need updating. Resolve their current binding
        // at use time and never rebuild the tree or retain a recycled row's data.
        let items: Vec<_> = self
            .items
            .borrow()
            .iter()
            .filter_map(glib::WeakRef::upgrade)
            .collect();

        for item in items {
            let (Some(label), Some(row)) = (
                item.child().and_downcast::<gtk::Label>(),
                item.item().and_downcast::<gtk::TreeListRow>(),
            ) else {
                continue;
            };

            let Some(data) = row.item().and_downcast::<glib::BoxedAnyObject>() else {
                continue;
            };

            let display = {
                let node = data.borrow::<VariableNode>();
                let (value, details) = variable_value_parts(&node.variable.value);

                self.display(&node.variable, value, details)
            };

            if label.text().as_str() != display {
                label.set_text(&display);
            }
        }
    }
}
