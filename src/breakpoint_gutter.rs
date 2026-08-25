use std::{cell::RefCell, rc::Rc};

use gtk::{gdk, glib, graphene, prelude::*, subclass::prelude::*};
use sourceview5::{
    GutterLines, GutterRenderer, GutterRendererAlignmentMode, prelude::*, subclass::prelude::*,
};

const GUTTER_WIDTH: i32 = 38;

#[derive(Clone)]
pub(crate) struct LineStyle {
    pub(crate) text: String,
    pub(crate) foreground: gdk::RGBA,
    pub(crate) background: Option<gdk::RGBA>,
}

type StyleProvider = Rc<dyn Fn(&sourceview5::Buffer, u32) -> LineStyle>;
type ActivateHandler = Rc<dyn Fn(&BreakpointGutterRenderer, &gtk::TextIter, &gdk::Rectangle, u32)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct BreakpointGutterRenderer {
        pub(super) style_provider: RefCell<Option<StyleProvider>>,
        pub(super) activate_handler: RefCell<Option<ActivateHandler>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BreakpointGutterRenderer {
        const NAME: &'static str = "FgdbBreakpointGutterRenderer";
        type Type = super::BreakpointGutterRenderer;
        type ParentType = GutterRenderer;
    }

    impl ObjectImpl for BreakpointGutterRenderer {}

    impl WidgetImpl for BreakpointGutterRenderer {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            if orientation == gtk::Orientation::Horizontal {
                (GUTTER_WIDTH, GUTTER_WIDTH, -1, -1)
            } else {
                self.parent_measure(orientation, for_size)
            }
        }
    }

    impl GutterRendererImpl for BreakpointGutterRenderer {
        fn query_activatable(&self, _iter: &gtk::TextIter, _area: &gdk::Rectangle) -> bool {
            true
        }

        fn activate(
            &self,
            iter: &gtk::TextIter,
            area: &gdk::Rectangle,
            button: u32,
            _state: gdk::ModifierType,
            _n_presses: i32,
        ) {
            if button == 1 {
                self.obj().activate_at(iter, area, button);
            }
        }

        fn snapshot_line(&self, snapshot: &gtk::Snapshot, lines: &GutterLines, line: u32) {
            let renderer = self.obj();
            let Some(style) = renderer.buffer().and_then(|buffer| {
                self.style_provider
                    .borrow()
                    .as_ref()
                    .map(|provider| provider(&buffer, line))
            }) else {
                return;
            };
            let (line_y, line_height) = lines.line_extent(line, GutterRendererAlignmentMode::Cell);
            let width = renderer.width() as f32;
            if let Some(background) = style.background {
                snapshot.append_color(
                    &background,
                    &graphene::Rect::new(0.0, line_y as f32, width, line_height as f32),
                );
            }

            let layout = renderer.create_pango_layout(Some(&style.text));
            let (text_width, text_height) = layout.pixel_size();
            let x = (width - text_width as f32 - 4.0).max(0.0);
            let y = line_y as f32 + ((line_height as f32 - text_height as f32) / 2.0).max(0.0);
            snapshot.save();
            snapshot.translate(&graphene::Point::new(x, y));
            snapshot.append_layout(&layout, &style.foreground);
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    pub struct BreakpointGutterRenderer(ObjectSubclass<imp::BreakpointGutterRenderer>)
        @extends GutterRenderer, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl BreakpointGutterRenderer {
    pub(crate) fn new(
        style_provider: impl Fn(&sourceview5::Buffer, u32) -> LineStyle + 'static,
        activate_handler: impl Fn(&BreakpointGutterRenderer, &gtk::TextIter, &gdk::Rectangle, u32)
        + 'static,
    ) -> Self {
        let renderer: Self = glib::Object::new();
        renderer
            .imp()
            .style_provider
            .replace(Some(Rc::new(style_provider)));
        renderer
            .imp()
            .activate_handler
            .replace(Some(Rc::new(activate_handler)));
        renderer.set_alignment_mode(GutterRendererAlignmentMode::Cell);
        renderer.add_css_class("breakpoint-gutter");
        renderer
    }

    pub(crate) fn activate_at(&self, iter: &gtk::TextIter, area: &gdk::Rectangle, button: u32) {
        if let Some(handler) = self.imp().activate_handler.borrow().as_ref() {
            handler(self, iter, area, button);
        }
    }
}
