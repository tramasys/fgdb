use super::*;

use std::{
    collections::{BTreeSet, HashMap},
    hash::{DefaultHasher, Hash, Hasher},
};

const MAX_CFG_INSTRUCTIONS: usize = 800;
const MAX_CFG_BLOCKS: usize = 180;
const MAX_RENDERED_BLOCK_INSTRUCTIONS: usize = 8;
const BLOCK_HEADER_HEIGHT: f64 = 25.0;
const BLOCK_LINE_HEIGHT: f64 = 17.0;
const BLOCK_VERTICAL_PADDING: f64 = 9.0;
const BLOCK_GAP: f64 = 34.0;
const GRAPH_MARGIN: f64 = 18.0;
const EDGE_GUTTER: f64 = 72.0;
const MIN_BLOCK_WIDTH: f64 = 340.0;
const MIN_GRAPH_WIDTH: i32 = 520;
const MIN_GRAPH_HEIGHT: i32 = 280;

#[derive(Clone)]
pub(super) struct CfgView {
    pub(super) root: gtk::Box,
    summary: gtk::Label,
    block_count: gtk::Label,
    edge_count: gtk::Label,
    detail: gtk::Label,
    block_detail: gtk::Label,
    bounded_detail: gtk::Label,
    exits_detail: gtk::Label,
    drawing: gtk::DrawingArea,
    canvas: gtk::Fixed,
    scrolled: gtk::ScrolledWindow,
    empty: gtk::Label,
    follow: gtk::ToggleButton,
    graph: Rc<RefCell<Option<ControlFlowGraph>>>,
    text_widgets: Rc<RefCell<Vec<gtk::Label>>>,
    text_current_block: Rc<Cell<Option<usize>>>,
    text_palette: CfgTextPalette,
    scroll_generation: Rc<Cell<u64>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CfgInstruction {
    address: u64,
    address_text: String,
    text: String,
    function: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CfgFlow {
    Conditional(Option<u64>),
    Unconditional(Option<u64>),
    Return,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CfgEdgeKind {
    Branch,
    Fallthrough,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CfgEdge {
    from: usize,
    to: usize,
    kind: CfgEdgeKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CfgBlock {
    start: usize,
    end: usize,
    terminator: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ControlFlowGraph {
    architecture: TargetArchitecture,
    pointer_bits: u32,
    signature: u64,
    input_instruction_count: usize,
    function: String,
    instructions: Vec<CfgInstruction>,
    blocks: Vec<CfgBlock>,
    edges: Vec<CfgEdge>,
    current_address: Option<u64>,
    current_block: Option<usize>,
    external_edges: usize,
    truncated: bool,
}

#[derive(Clone, Copy)]
struct CfgColor {
    red: f64,
    green: f64,
    blue: f64,
    alpha: f64,
}

impl CfgColor {
    fn parse(value: &str) -> Self {
        let color = gtk::gdk::RGBA::parse(value).expect("built-in CFG theme color must be valid");

        Self {
            red: f64::from(color.red()),
            green: f64::from(color.green()),
            blue: f64::from(color.blue()),
            alpha: f64::from(color.alpha()),
        }
    }

    fn mix(self, other: Self, amount: f64) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        let retained = 1.0 - amount;

        Self {
            red: self.red * retained + other.red * amount,
            green: self.green * retained + other.green * amount,
            blue: self.blue * retained + other.blue * amount,
            alpha: self.alpha * retained + other.alpha * amount,
        }
    }

    fn apply(self, context: &gtk::cairo::Context) {
        context.set_source_rgba(self.red, self.green, self.blue, self.alpha);
    }
}

#[derive(Clone, Copy)]
struct CfgPalette {
    background: CfgColor,
    surface: CfgColor,
    raised: CfgColor,
    border: CfgColor,
    muted: CfgColor,
    accent: CfgColor,
    accent_hover: CfgColor,
}

impl CfgPalette {
    fn new(theme: &Theme) -> Self {
        let colors = &theme.colors;

        Self {
            background: CfgColor::parse(colors.background),
            surface: CfgColor::parse(colors.surface),
            raised: CfgColor::parse(colors.raised),
            border: CfgColor::parse(colors.border),
            muted: CfgColor::parse(colors.muted),
            accent: CfgColor::parse(colors.accent),
            accent_hover: CfgColor::parse(colors.accent_hover),
        }
    }
}

#[derive(Clone, Copy)]
struct CfgTextPalette {
    foreground: &'static str,
    muted: &'static str,
    accent: &'static str,
}

impl CfgTextPalette {
    const fn new(theme: &Theme) -> Self {
        Self {
            foreground: theme.colors.foreground,
            muted: theme.colors.muted,
            accent: theme.colors.accent_hover,
        }
    }
}

#[derive(Clone, Copy)]
struct CfgBlockLayout {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Clone, Copy)]
enum RenderedInstruction {
    Instruction(usize),
    Omitted(usize),
}

pub(super) fn build_cfg_view(theme: &Theme) -> CfgView {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_size_request(0, 0);
    root.set_vexpand(true);
    root.add_css_class("cfg-page");
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    toolbar.add_css_class("cfg-toolbar");
    let labels = gtk::Box::new(gtk::Orientation::Vertical, 5);
    labels.set_hexpand(true);
    labels.set_valign(gtk::Align::Center);
    let summary_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    summary_row.add_css_class("cfg-summary-row");
    let summary = gtk::Label::new(Some("CONTROL FLOW GRAPH"));
    summary.set_halign(gtk::Align::Start);
    summary.set_ellipsize(pango::EllipsizeMode::Middle);
    summary.add_css_class("cfg-summary");
    let block_count = gtk::Label::new(None);
    block_count.add_css_class("cfg-stat");
    block_count.set_visible(false);
    let edge_count = gtk::Label::new(None);
    edge_count.add_css_class("cfg-stat");
    edge_count.set_visible(false);
    summary_row.append(&summary);
    summary_row.append(&block_count);
    summary_row.append(&edge_count);
    let detail_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    detail_row.add_css_class("cfg-detail-row");
    let detail = gtk::Label::new(Some("Pause at readable code to build the graph"));
    detail.set_halign(gtk::Align::Start);
    detail.set_ellipsize(pango::EllipsizeMode::End);
    detail.add_css_class("cfg-detail");
    detail.set_hexpand(false);
    let block_detail = gtk::Label::new(None);
    block_detail.add_css_class("cfg-detail");
    block_detail.add_css_class("cfg-block-detail");
    block_detail.set_visible(false);
    let bounded_detail = gtk::Label::new(None);
    bounded_detail.add_css_class("cfg-detail");
    bounded_detail.add_css_class("cfg-bounded-detail");
    bounded_detail.set_visible(false);
    let exits_detail = gtk::Label::new(None);
    exits_detail.add_css_class("cfg-detail");
    exits_detail.set_visible(false);
    detail_row.append(&detail);
    detail_row.append(&block_detail);
    detail_row.append(&bounded_detail);
    detail_row.append(&exits_detail);
    labels.append(&summary_row);
    labels.append(&detail_row);
    toolbar.append(&labels);
    let follow = gtk::ToggleButton::with_label("Follow PC");
    follow.set_active(true);
    follow.set_valign(gtk::Align::Center);
    follow.add_css_class("toolbar-toggle");
    follow.add_css_class("cfg-follow");
    follow.set_tooltip_text(Some("Keep the current basic block centered while stepping"));
    toolbar.append(&follow);
    root.append(&toolbar);
    let legend = gtk::Box::new(gtk::Orientation::Vertical, 0);
    legend.add_css_class("cfg-legend");
    let legend_toggle = gtk::ToggleButton::new();
    legend_toggle.add_css_class("cfg-legend-toggle");
    let legend_toggle_label = gtk::Label::new(Some("Show graph legend"));
    legend_toggle_label.set_halign(gtk::Align::Start);
    legend_toggle_label.set_xalign(0.0);
    legend_toggle.set_child(Some(&legend_toggle_label));
    legend.append(&legend_toggle);
    let legend_items = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    legend_items.set_homogeneous(true);
    legend_items.add_css_class("cfg-legend-items");

    for (sample, title, detail, class) in [
        ("━━", "Branch path", "solid connector", "branch"),
        ("┄┄", "Fallthrough path", "dashed connector", "fallthrough"),
        ("▰", "Current position", "soft highlight", "current"),
    ] {
        let item = gtk::Box::new(gtk::Orientation::Horizontal, 9);
        item.set_hexpand(true);
        item.set_valign(gtk::Align::Center);
        item.add_css_class("cfg-legend-item");
        let sample_label = gtk::Label::new(Some(sample));
        sample_label.set_valign(gtk::Align::Center);
        sample_label.add_css_class("cfg-legend-sample");
        sample_label.add_css_class(class);
        let description = gtk::Box::new(gtk::Orientation::Vertical, 2);
        description.set_valign(gtk::Align::Center);
        let title_label = gtk::Label::new(Some(title));
        title_label.set_halign(gtk::Align::Start);
        title_label.add_css_class("cfg-legend-title");
        let detail_label = gtk::Label::new(Some(detail));
        detail_label.set_halign(gtk::Align::Start);
        detail_label.add_css_class("cfg-legend-detail");
        description.append(&title_label);
        description.append(&detail_label);
        item.append(&sample_label);
        item.append(&description);
        legend_items.append(&item);
    }

    let legend_revealer = gtk::Revealer::new();
    legend_revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    legend_revealer.set_transition_duration(120);
    legend_revealer.set_child(Some(&legend_items));
    legend.append(&legend_revealer);

    legend_toggle.connect_toggled(move |button| {
        let expanded = button.is_active();

        legend_toggle_label.set_text(if expanded {
            "Hide graph legend"
        } else {
            "Show graph legend"
        });

        legend_revealer.set_reveal_child(expanded);
    });

    root.append(&legend);
    let graph = Rc::new(RefCell::new(None::<ControlFlowGraph>));
    let drawing = gtk::DrawingArea::new();
    drawing.set_content_width(MIN_GRAPH_WIDTH);
    drawing.set_content_height(MIN_GRAPH_HEIGHT);
    drawing.set_hexpand(true);
    drawing.set_vexpand(true);
    drawing.add_css_class("cfg-canvas");
    let palette = CfgPalette::new(theme);
    let graph_for_drawing = Rc::clone(&graph);

    drawing.set_draw_func(move |_, context, width, height| {
        draw_cfg(
            context,
            f64::from(width),
            f64::from(height),
            graph_for_drawing.borrow().as_ref(),
            palette,
        );
    });

    let canvas = gtk::Fixed::new();
    canvas.add_css_class("cfg-canvas-layer");
    canvas.put(&drawing, 0.0, 0.0);
    let text_widgets = Rc::new(RefCell::new(Vec::new()));
    let text_current_block = Rc::new(Cell::new(None));
    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    scrolled.set_overlay_scrolling(false);
    scrolled.set_propagate_natural_width(false);
    scrolled.set_propagate_natural_height(false);
    scrolled.set_min_content_width(0);
    scrolled.set_vexpand(true);
    scrolled.add_css_class("cfg-scroll");
    scrolled.set_child(Some(&canvas));

    let empty = gtk::Label::new(Some(
        "The CFG appears when the inferior is paused at readable code",
    ));

    empty.set_halign(gtk::Align::Center);
    empty.set_valign(gtk::Align::Center);
    empty.set_wrap(true);
    empty.add_css_class("empty-state");
    empty.add_css_class("cfg-empty");
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&scrolled));
    overlay.add_overlay(&empty);
    overlay.set_vexpand(true);
    root.append(&overlay);
    let scroll_generation = Rc::new(Cell::new(0_u64));
    let weak_scrolled = scrolled.downgrade();
    let graph_for_map = Rc::clone(&graph);
    let generation_for_map = Rc::clone(&scroll_generation);
    let follow_for_map = follow.clone();

    drawing.connect_map(move |_| {
        if follow_for_map.is_active()
            && let Some(scrolled) = weak_scrolled.upgrade()
        {
            schedule_cfg_center(&scrolled, &graph_for_map, &generation_for_map);
        }
    });

    let weak_scrolled = scrolled.downgrade();
    let graph_for_follow = Rc::clone(&graph);
    let generation_for_follow = Rc::clone(&scroll_generation);

    follow.connect_toggled(move |button| {
        if button.is_active()
            && let Some(scrolled) = weak_scrolled.upgrade()
        {
            schedule_cfg_center(&scrolled, &graph_for_follow, &generation_for_follow);
        }
    });

    CfgView {
        root,
        summary,
        block_count,
        edge_count,
        detail,
        block_detail,
        bounded_detail,
        exits_detail,
        drawing,
        canvas,
        scrolled,
        empty,
        follow,
        graph,
        text_widgets,
        text_current_block,
        text_palette: CfgTextPalette::new(theme),
        scroll_generation,
    }
}

impl CfgView {
    pub(super) fn show(
        &self,
        instructions: &[Instruction],
        pc: &str,
        architecture: TargetArchitecture,
        pointer_bits: u32,
    ) {
        if instructions.is_empty() {
            self.clear();
            return;
        }

        let current_address = hex_value(pc);
        let signature = cfg_signature(instructions, architecture, pointer_bits);

        let reused = {
            let mut slot = self.graph.borrow_mut();

            slot.as_mut().is_some_and(|graph| {
                if graph.signature != signature
                    || graph.architecture != architecture
                    || graph.pointer_bits != pointer_bits
                    || graph.input_instruction_count != instructions.len()
                    || current_address
                        .is_some_and(|address| !graph.contains_rendered_address(address))
                {
                    return false;
                }

                graph.set_current(current_address);

                true
            })
        };

        if !reused {
            self.graph.replace(build_control_flow_graph(
                instructions,
                current_address,
                architecture,
                pointer_bits,
                signature,
            ));
        }

        let (
            summary,
            block_count,
            edge_count,
            detail,
            block_detail,
            bounded_detail,
            exits_detail,
            width,
            height,
        ) = {
            let graph = self.graph.borrow();

            let Some(graph) = graph.as_ref() else {
                drop(graph);
                self.clear();
                return;
            };

            let (detail, block_detail) = graph.current_block.map_or_else(
                || {
                    (
                        String::from("The current PC is outside this bounded disassembly view"),
                        None,
                    )
                },
                |block| {
                    let address = graph
                        .current_address
                        .map(|address| cfg_address(address, graph.pointer_bits))
                        .unwrap_or_else(|| String::from("unknown PC"));

                    (format!("PC {address}"), Some(format!("Block B{block}")))
                },
            );

            let exits_detail = if graph.external_edges > 0 {
                let suffix = if graph.external_edges == 1 {
                    "exit"
                } else {
                    "exits"
                };

                Some(format!("{} {suffix} leave the view", graph.external_edges))
            } else {
                None
            };

            (
                graph.function.clone(),
                format!("{} blocks", graph.blocks.len()),
                format!("{} edges", graph.edges.len()),
                detail,
                block_detail,
                graph.truncated.then_some("Bounded view"),
                exits_detail,
                graph_content_width(graph),
                graph_content_height(graph),
            )
        };

        self.summary.set_text(&summary);
        self.summary.set_tooltip_text(Some(&summary));
        self.block_count.set_text(&block_count);
        self.block_count.set_visible(true);
        self.edge_count.set_text(&edge_count);
        self.edge_count.set_visible(true);
        self.detail.set_text(&detail);
        self.detail.set_tooltip_text(Some(&detail));
        set_cfg_optional_label(&self.block_detail, block_detail.as_deref());
        set_cfg_optional_label(&self.bounded_detail, bounded_detail);
        set_cfg_optional_label(&self.exits_detail, exits_detail.as_deref());
        self.empty.set_visible(false);
        self.drawing.set_content_width(width);
        self.drawing.set_content_height(height);
        self.canvas.set_size_request(width, height);

        let current_block = self
            .graph
            .borrow()
            .as_ref()
            .and_then(|graph| graph.current_block);

        if !reused {
            rebuild_cfg_text_widgets(
                &self.canvas,
                &self.text_widgets,
                self.graph.borrow().as_ref(),
                self.text_palette,
                f64::from(width),
            );
        } else {
            refresh_cfg_dynamic_text(
                &self.text_widgets,
                self.graph.borrow().as_ref(),
                self.text_palette,
                self.text_current_block.get(),
                current_block,
                f64::from(width),
            );
        }

        self.text_current_block.set(current_block);
        self.drawing.queue_draw();

        if self.follow.is_active() && self.drawing.is_mapped() {
            schedule_cfg_center(&self.scrolled, &self.graph, &self.scroll_generation);
        }
    }

    pub(super) fn clear(&self) {
        self.graph.replace(None);
        clear_cfg_text_widgets(&self.canvas, &self.text_widgets);
        self.text_current_block.set(None);
        self.summary.set_text("CONTROL FLOW GRAPH");
        self.summary.set_tooltip_text(None);
        self.block_count.set_visible(false);
        self.edge_count.set_visible(false);

        self.detail
            .set_text("Pause at readable code to build the graph");

        self.detail.set_tooltip_text(None);
        self.block_detail.set_visible(false);
        self.bounded_detail.set_visible(false);
        self.exits_detail.set_visible(false);
        self.empty.set_visible(true);
        self.drawing.set_content_width(MIN_GRAPH_WIDTH);
        self.drawing.set_content_height(MIN_GRAPH_HEIGHT);

        self.canvas
            .set_size_request(MIN_GRAPH_WIDTH, MIN_GRAPH_HEIGHT);

        self.drawing.queue_draw();
    }
}

fn set_cfg_optional_label(label: &gtk::Label, text: Option<&str>) {
    label.set_text(text.unwrap_or_default());
    label.set_visible(text.is_some());
}

impl ControlFlowGraph {
    fn contains_rendered_address(&self, address: u64) -> bool {
        self.blocks.iter().any(|block| {
            let first = self.instructions[block.start].address;
            let last = self.instructions[block.end - 1].address;

            (first..=last).contains(&address)
        })
    }

    fn set_current(&mut self, address: Option<u64>) {
        self.current_address = address;

        self.current_block = address.and_then(|address| {
            self.blocks.iter().position(|block| {
                let first = self.instructions[block.start].address;
                let last = self.instructions[block.end - 1].address;

                (first..=last).contains(&address)
            })
        });
    }
}

fn cfg_signature(
    instructions: &[Instruction],
    architecture: TargetArchitecture,
    pointer_bits: u32,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    (architecture as u8).hash(&mut hasher);
    pointer_bits.hash(&mut hasher);
    instructions.len().hash(&mut hasher);

    for instruction in instructions {
        instruction.address.hash(&mut hasher);
        instruction.function.hash(&mut hasher);
        instruction.text.hash(&mut hasher);
        instruction.opcodes.hash(&mut hasher);
    }

    hasher.finish()
}

fn build_control_flow_graph(
    instructions: &[Instruction],
    current_address: Option<u64>,
    architecture: TargetArchitecture,
    pointer_bits: u32,
    signature: u64,
) -> Option<ControlFlowGraph> {
    let mut parsed = instructions
        .iter()
        .filter_map(|instruction| {
            Some(CfgInstruction {
                address: hex_value(&instruction.address)?,
                address_text: instruction.address.clone(),
                text: instruction.text.clone(),
                function: instruction.function.clone(),
            })
        })
        .collect::<Vec<_>>();

    parsed.sort_by_key(|instruction| instruction.address);
    parsed.dedup_by_key(|instruction| instruction.address);

    if parsed.is_empty() {
        return None;
    }

    let total_parsed = parsed.len();

    if parsed.len() > MAX_CFG_INSTRUCTIONS {
        let current = current_address
            .and_then(|address| {
                parsed
                    .binary_search_by_key(&address, |instruction| instruction.address)
                    .ok()
            })
            .unwrap_or(0);

        let preferred_history = MAX_CFG_INSTRUCTIONS / 5;

        let start = current
            .saturating_sub(preferred_history)
            .min(parsed.len() - MAX_CFG_INSTRUCTIONS);

        parsed = parsed[start..start + MAX_CFG_INSTRUCTIONS].to_vec();
    }

    let address_indexes = parsed
        .iter()
        .enumerate()
        .map(|(index, instruction)| (instruction.address, index))
        .collect::<HashMap<_, _>>();

    let flows = parsed
        .iter()
        .map(|instruction| cfg_flow(instruction, architecture))
        .collect::<Vec<_>>();

    let mut leaders = BTreeSet::from([0_usize]);

    for (index, flow) in flows.iter().copied().enumerate() {
        let Some(flow) = flow else {
            continue;
        };

        let delay = cfg_delay_slots(flow, architecture);

        if let CfgFlow::Conditional(target) | CfgFlow::Unconditional(target) = flow
            && let Some(target) = target
            && let Some(&target_index) = address_indexes.get(&target)
            && !(delay > 0 && target_index == index.saturating_add(1))
        {
            leaders.insert(target_index);
        }

        let successor = index.saturating_add(1).saturating_add(delay);

        if successor < parsed.len() {
            leaders.insert(successor);
        }
    }

    let leader_indexes = leaders.into_iter().collect::<Vec<_>>();

    let mut all_blocks = leader_indexes
        .iter()
        .enumerate()
        .map(|(position, &start)| {
            let end = leader_indexes
                .get(position + 1)
                .copied()
                .unwrap_or(parsed.len());

            let terminator = (start..end).find(|&index| flows[index].is_some());

            CfgBlock {
                start,
                end,
                terminator,
            }
        })
        .collect::<Vec<_>>();

    let current_source_index = current_address.and_then(|address| {
        parsed
            .binary_search_by_key(&address, |instruction| instruction.address)
            .ok()
    });

    let all_block_count = all_blocks.len();

    if all_blocks.len() > MAX_CFG_BLOCKS {
        let current_block = current_source_index
            .and_then(|current| {
                all_blocks
                    .iter()
                    .position(|block| (block.start..block.end).contains(&current))
            })
            .unwrap_or(0);

        let preferred_history = MAX_CFG_BLOCKS / 4;

        let start = current_block
            .saturating_sub(preferred_history)
            .min(all_blocks.len() - MAX_CFG_BLOCKS);

        all_blocks = all_blocks[start..start + MAX_CFG_BLOCKS].to_vec();
    }

    let source_to_block = all_blocks
        .iter()
        .enumerate()
        .flat_map(|(block, range)| (range.start..range.end).map(move |index| (index, block)))
        .collect::<HashMap<_, _>>();

    let mut edges = Vec::new();
    let mut external_edges = 0_usize;

    let mut add_edge = |from: usize, source_index: Option<usize>, kind: CfgEdgeKind| {
        let Some(source_index) = source_index else {
            external_edges = external_edges.saturating_add(1);
            return;
        };

        let Some(&to) = source_to_block.get(&source_index) else {
            external_edges = external_edges.saturating_add(1);
            return;
        };

        if !edges
            .iter()
            .any(|edge: &CfgEdge| edge.from == from && edge.to == to && edge.kind == kind)
        {
            edges.push(CfgEdge { from, to, kind });
        }
    };

    for (block_index, block) in all_blocks.iter().enumerate() {
        if let Some(terminator) = block.terminator {
            let flow = flows[terminator].expect("CFG terminator must have a flow kind");

            let successor = terminator
                .saturating_add(1)
                .saturating_add(cfg_delay_slots(flow, architecture));

            let successor = (successor < parsed.len()).then_some(successor);

            match flow {
                CfgFlow::Conditional(target) => {
                    add_edge(
                        block_index,
                        target.and_then(|target| address_indexes.get(&target).copied()),
                        CfgEdgeKind::Branch,
                    );

                    add_edge(block_index, successor, CfgEdgeKind::Fallthrough);
                }
                CfgFlow::Unconditional(target) => add_edge(
                    block_index,
                    target.and_then(|target| address_indexes.get(&target).copied()),
                    CfgEdgeKind::Branch,
                ),
                CfgFlow::Return | CfgFlow::Terminal => {}
            }
        } else {
            add_edge(
                block_index,
                (block.end < parsed.len()).then_some(block.end),
                CfgEdgeKind::Fallthrough,
            );
        }
    }

    edges.sort_by_key(|edge| (edge.from, edge.to, edge.kind as u8));

    let function = current_source_index
        .and_then(|index| parsed.get(index))
        .filter(|instruction| instruction.function != "??")
        .or_else(|| {
            parsed
                .iter()
                .find(|instruction| instruction.function != "??")
        })
        .map(|instruction| instruction.function.clone())
        .unwrap_or_else(|| String::from("unknown function"));

    let mut graph = ControlFlowGraph {
        architecture,
        pointer_bits,
        signature,
        input_instruction_count: instructions.len(),
        function,
        instructions: parsed,
        blocks: all_blocks,
        edges,
        current_address: None,
        current_block: None,
        external_edges,
        truncated: total_parsed > MAX_CFG_INSTRUCTIONS || all_block_count > MAX_CFG_BLOCKS,
    };

    graph.set_current(current_address);

    Some(graph)
}

fn cfg_flow(instruction: &CfgInstruction, architecture: TargetArchitecture) -> Option<CfgFlow> {
    let (mnemonic, operands) = normalized_instruction_parts(&instruction.text, architecture);
    let mnemonic = mnemonic.as_ref();

    if is_call_instruction(mnemonic, operands, architecture) {
        return None;
    }

    if is_return_instruction(mnemonic, operands, architecture) {
        return Some(CfgFlow::Return);
    }

    let unconditional = is_unconditional_branch(mnemonic, architecture)
        || (architecture == TargetArchitecture::Unknown
            && matches!(mnemonic, "jmp" | "ljmp" | "b" | "j"));

    if unconditional {
        return Some(CfgFlow::Unconditional(direct_control_flow_address(
            mnemonic,
            operands,
            architecture,
        )));
    }

    let conditional = is_conditional_branch(mnemonic, architecture)
        || (architecture == TargetArchitecture::Unknown
            && ((mnemonic.starts_with('j') && !mnemonic.starts_with("jmp"))
                || mnemonic.starts_with("loop")));

    if conditional {
        return Some(CfgFlow::Conditional(direct_control_flow_address(
            mnemonic,
            operands,
            architecture,
        )));
    }

    is_terminal_instruction(mnemonic, architecture).then_some(CfgFlow::Terminal)
}

fn is_terminal_instruction(mnemonic: &str, architecture: TargetArchitecture) -> bool {
    match architecture {
        TargetArchitecture::X86 | TargetArchitecture::X86_64 => {
            matches!(mnemonic, "ud2" | "hlt" | "int3")
        }
        TargetArchitecture::Arm | TargetArchitecture::AArch64 => {
            matches!(mnemonic, "brk" | "hlt" | "udf")
        }
        TargetArchitecture::RiscV32 | TargetArchitecture::RiscV64 => mnemonic == "ebreak",
        _ => false,
    }
}

fn cfg_delay_slots(flow: CfgFlow, architecture: TargetArchitecture) -> usize {
    usize::from(
        matches!(
            architecture,
            TargetArchitecture::Mips32 | TargetArchitecture::Mips64
        ) && matches!(
            flow,
            CfgFlow::Conditional(_) | CfgFlow::Unconditional(_) | CfgFlow::Return
        ),
    )
}

fn graph_content_width(graph: &ControlFlowGraph) -> i32 {
    let max_chars = graph
        .blocks
        .iter()
        .flat_map(|block| block.start..block.end)
        .map(|index| {
            graph.instructions[index]
                .address_text
                .chars()
                .count()
                .saturating_add(2)
                .saturating_add(graph.instructions[index].text.chars().count())
        })
        .max()
        .unwrap_or(0)
        .min(92);

    let text_width = i32::try_from(max_chars.saturating_mul(8)).unwrap_or(i32::MAX);

    MIN_GRAPH_WIDTH.max(text_width.saturating_add(190).min(980))
}

fn graph_content_height(graph: &ControlFlowGraph) -> i32 {
    let height = graph_layout(graph, f64::from(graph_content_width(graph)))
        .last()
        .map_or(f64::from(MIN_GRAPH_HEIGHT), |layout| {
            layout.y + layout.height + GRAPH_MARGIN
        });

    (height.ceil() as i64).clamp(i64::from(MIN_GRAPH_HEIGHT), i64::from(i32::MAX)) as i32
}

fn graph_layout(graph: &ControlFlowGraph, width: f64) -> Vec<CfgBlockLayout> {
    let block_width = (width - EDGE_GUTTER * 2.0).max(MIN_BLOCK_WIDTH);
    let x = ((width - block_width) / 2.0).max(EDGE_GUTTER);
    let mut y = GRAPH_MARGIN;

    graph
        .blocks
        .iter()
        .map(|block| {
            let lines = rendered_instructions(graph, block).len();

            let height = BLOCK_HEADER_HEIGHT
                + BLOCK_VERTICAL_PADDING * 2.0
                + BLOCK_LINE_HEIGHT * lines as f64;

            let layout = CfgBlockLayout {
                x,
                y,
                width: block_width,
                height,
            };

            y += height + BLOCK_GAP;

            layout
        })
        .collect()
}

fn rendered_instructions(graph: &ControlFlowGraph, block: &CfgBlock) -> Vec<RenderedInstruction> {
    let length = block.end.saturating_sub(block.start);

    if length <= MAX_RENDERED_BLOCK_INSTRUCTIONS {
        return (block.start..block.end)
            .map(RenderedInstruction::Instruction)
            .collect();
    }

    let current = graph.current_address.and_then(|address| {
        (block.start..block.end).find(|&index| graph.instructions[index].address == address)
    });

    let centered_slots = MAX_RENDERED_BLOCK_INSTRUCTIONS.saturating_sub(2);

    let mut visible_start = current.map_or(block.start, |current| {
        current
            .saturating_sub(centered_slots / 2)
            .max(block.start)
            .min(block.end - centered_slots)
    });

    let mut visible_end = (visible_start + centered_slots).min(block.end);

    if visible_start == block.start {
        visible_end = (visible_end + 1).min(block.end);
    } else if visible_end == block.end {
        visible_start = visible_start.saturating_sub(1).max(block.start);
    }

    let mut rendered = Vec::with_capacity(MAX_RENDERED_BLOCK_INSTRUCTIONS);

    if visible_start > block.start {
        rendered.push(RenderedInstruction::Omitted(visible_start - block.start));
    }

    rendered.extend((visible_start..visible_end).map(RenderedInstruction::Instruction));

    if visible_end < block.end {
        rendered.push(RenderedInstruction::Omitted(block.end - visible_end));
    }

    rendered
}

fn clear_cfg_text_widgets(canvas: &gtk::Fixed, text_widgets: &Rc<RefCell<Vec<gtk::Label>>>) {
    for label in text_widgets.borrow_mut().drain(..) {
        canvas.remove(&label);
    }
}

fn rebuild_cfg_text_widgets(
    canvas: &gtk::Fixed,
    text_widgets: &Rc<RefCell<Vec<gtk::Label>>>,
    graph: Option<&ControlFlowGraph>,
    palette: CfgTextPalette,
    width: f64,
) {
    clear_cfg_text_widgets(canvas, text_widgets);

    let Some(graph) = graph else {
        return;
    };

    let layouts = graph_layout(graph, width);
    let mut widgets = text_widgets.borrow_mut();
    widgets.reserve(graph.blocks.len().saturating_mul(2));

    for (block_index, (block, layout)) in graph.blocks.iter().zip(layouts).enumerate() {
        let first = graph.instructions[block.start].address;
        let last = graph.instructions[block.end - 1].address;

        let title = format!(
            "B{block_index}  {} – {}",
            cfg_address(first, graph.pointer_bits),
            cfg_address(last, graph.pointer_bits)
        );

        let header = gtk::Label::new(Some(&truncate_cfg_text(&title, 76)));
        header.set_halign(gtk::Align::Start);
        header.set_valign(gtk::Align::Start);
        header.set_xalign(0.0);
        header.set_yalign(0.0);
        enable_stable_text_selection(&header);
        header.set_ellipsize(pango::EllipsizeMode::End);
        header.set_size_request((layout.width - 20.0).max(1.0) as i32, 18);
        header.add_css_class("cfg-block-label");
        header.add_css_class("cfg-block-header-label");
        canvas.put(&header, layout.x + 10.0, layout.y + 4.0);
        widgets.push(header);
        let body = gtk::Label::new(None);
        body.set_markup(&cfg_block_body_markup(graph, block, palette, layout.width));
        body.set_halign(gtk::Align::Start);
        body.set_valign(gtk::Align::Start);
        body.set_xalign(0.0);
        body.set_yalign(0.0);
        enable_stable_text_selection(&body);
        let attributes = pango::AttrList::new();

        attributes.insert(pango::AttrInt::new_line_height_absolute(
            (BLOCK_LINE_HEIGHT * f64::from(pango::SCALE)) as i32,
        ));

        body.set_attributes(Some(&attributes));

        body.set_size_request(
            (layout.width - 16.0).max(1.0) as i32,
            (layout.height - BLOCK_HEADER_HEIGHT - BLOCK_VERTICAL_PADDING * 2.0).max(1.0) as i32,
        );

        body.add_css_class("cfg-block-label");
        body.add_css_class("cfg-block-body-label");

        canvas.put(
            &body,
            layout.x + 8.0,
            layout.y + BLOCK_HEADER_HEIGHT + BLOCK_VERTICAL_PADDING,
        );

        widgets.push(body);
    }
}

fn refresh_cfg_dynamic_text(
    text_widgets: &Rc<RefCell<Vec<gtk::Label>>>,
    graph: Option<&ControlFlowGraph>,
    palette: CfgTextPalette,
    previous_block: Option<usize>,
    current_block: Option<usize>,
    width: f64,
) {
    let Some(graph) = graph else {
        return;
    };

    let layouts = graph_layout(graph, width);
    let widgets = text_widgets.borrow();
    let mut updated = None;

    for block_index in [previous_block, current_block].into_iter().flatten() {
        if updated == Some(block_index) {
            continue;
        }

        updated = Some(block_index);

        let Some(block) = graph.blocks.get(block_index) else {
            continue;
        };

        let Some(layout) = layouts.get(block_index) else {
            continue;
        };

        let Some(body) = widgets.get(block_index.saturating_mul(2).saturating_add(1)) else {
            continue;
        };

        body.set_markup(&cfg_block_body_markup(graph, block, palette, layout.width));
    }
}

fn cfg_block_body_markup(
    graph: &ControlFlowGraph,
    block: &CfgBlock,
    palette: CfgTextPalette,
    block_width: f64,
) -> String {
    let address_columns = usize::try_from(graph.pointer_bits / 4)
        .unwrap_or(16)
        .clamp(8, 16)
        + 6;

    let available_columns = ((block_width - 16.0) / 7.0).floor().max(24.0) as usize;

    rendered_instructions(graph, block)
        .into_iter()
        .map(|rendered| match rendered {
            RenderedInstruction::Instruction(index) => {
                let instruction = &graph.instructions[index];
                let address = cfg_address(instruction.address, graph.pointer_bits);
                let padded_address = format!("  {address}  ");

                let text = gtk::glib::markup_escape_text(&truncate_cfg_text(
                    &instruction.text,
                    available_columns.saturating_sub(address_columns).max(8),
                ));

                format!(
                    "<span foreground=\"{}\">{}</span><span foreground=\"{}\">{text}</span>",
                    palette.accent, padded_address, palette.foreground
                )
            }
            RenderedInstruction::Omitted(count) => format!(
                "<span foreground=\"{}\">  {count} instructions omitted</span>",
                palette.muted
            ),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn draw_cfg(
    context: &gtk::cairo::Context,
    width: f64,
    height: f64,
    graph: Option<&ControlFlowGraph>,
    palette: CfgPalette,
) {
    palette.background.apply(context);
    context.rectangle(0.0, 0.0, width, height);
    let _ = context.fill();

    let Some(graph) = graph else {
        return;
    };

    let layouts = graph_layout(graph, width);
    draw_cfg_edges(context, graph, &layouts, palette);

    for (index, (block, layout)) in graph.blocks.iter().zip(&layouts).enumerate() {
        draw_cfg_block(context, graph, index, block, *layout, palette);
    }
}

fn draw_cfg_edges(
    context: &gtk::cairo::Context,
    graph: &ControlFlowGraph,
    layouts: &[CfgBlockLayout],
    palette: CfgPalette,
) {
    context.set_line_cap(gtk::cairo::LineCap::Square);

    for (edge_index, edge) in graph.edges.iter().enumerate() {
        let (Some(source), Some(target)) = (layouts.get(edge.from), layouts.get(edge.to)) else {
            continue;
        };

        let active = graph.current_block == Some(edge.from);

        let color = if active {
            palette.accent_hover
        } else if edge.kind == CfgEdgeKind::Branch {
            palette.accent
        } else {
            palette.muted.mix(palette.background, 0.18)
        };

        color.apply(context);
        context.set_line_width(if active { 2.0 } else { 1.2 });

        if edge.kind == CfgEdgeKind::Fallthrough {
            context.set_dash(&[3.0, 4.0], 0.0);
            let x = source.x + source.width / 2.0;
            let start_y = source.y + source.height;
            let end_y = target.y;
            context.move_to(x, start_y);
            context.line_to(x, end_y);
            let _ = context.stroke();
            draw_arrow_head(context, x, end_y, 0.0, 1.0, color);
            continue;
        }

        context.set_dash(&[], 0.0);
        let forward = target.y > source.y;
        let lane = (edge_index % 7) as f64;

        let (source_x, target_x, rail_x, direction) = if forward {
            let edge_x = source.x + source.width;

            (
                edge_x,
                target.x + target.width,
                edge_x + 13.0 + lane * 7.0,
                -1.0,
            )
        } else {
            let edge_x = source.x;

            (edge_x, target.x, edge_x - 13.0 - lane * 7.0, 1.0)
        };

        let source_y = source.y + BLOCK_HEADER_HEIGHT / 2.0;
        let target_y = target.y + BLOCK_HEADER_HEIGHT / 2.0;

        if edge.from == edge.to {
            let loop_y = source_y + BLOCK_HEADER_HEIGHT;
            context.move_to(source_x, source_y);
            context.line_to(rail_x, source_y);
            context.line_to(rail_x, loop_y);
            context.line_to(target_x, loop_y);
            context.line_to(target_x, target_y);
        } else {
            context.move_to(source_x, source_y);
            context.line_to(rail_x, source_y);
            context.line_to(rail_x, target_y);
            context.line_to(target_x, target_y);
        }

        let _ = context.stroke();
        draw_arrow_head(context, target_x, target_y, direction, 0.0, color);
    }

    context.set_dash(&[], 0.0);
}

fn draw_arrow_head(
    context: &gtk::cairo::Context,
    tip_x: f64,
    tip_y: f64,
    direction_x: f64,
    direction_y: f64,
    color: CfgColor,
) {
    let perpendicular_x = -direction_y;
    let perpendicular_y = direction_x;
    let base_x = tip_x - direction_x * 7.0;
    let base_y = tip_y - direction_y * 7.0;
    color.apply(context);
    context.move_to(tip_x, tip_y);

    context.line_to(
        base_x + perpendicular_x * 3.5,
        base_y + perpendicular_y * 3.5,
    );

    context.line_to(
        base_x - perpendicular_x * 3.5,
        base_y - perpendicular_y * 3.5,
    );

    context.close_path();
    let _ = context.fill();
}

fn draw_cfg_block(
    context: &gtk::cairo::Context,
    graph: &ControlFlowGraph,
    block_index: usize,
    block: &CfgBlock,
    layout: CfgBlockLayout,
    palette: CfgPalette,
) {
    let current_block = graph.current_block == Some(block_index);

    let background = if current_block {
        palette.surface.mix(palette.accent, 0.055)
    } else {
        palette.surface
    };

    background.apply(context);
    context.rectangle(layout.x, layout.y, layout.width, layout.height);
    let _ = context.fill();

    let header = if current_block {
        palette.raised.mix(palette.accent, 0.09)
    } else {
        palette.raised.mix(palette.surface, 0.42)
    };

    header.apply(context);
    context.rectangle(layout.x, layout.y, layout.width, BLOCK_HEADER_HEIGHT);
    let _ = context.fill();
    let lines = rendered_instructions(graph, block);

    for (line, rendered) in lines.iter().enumerate() {
        let RenderedInstruction::Instruction(index) = rendered else {
            continue;
        };

        if graph.current_address != Some(graph.instructions[*index].address) {
            continue;
        }

        let row_y = layout.y
            + BLOCK_HEADER_HEIGHT
            + BLOCK_VERTICAL_PADDING
            + line as f64 * BLOCK_LINE_HEIGHT;

        palette.accent.mix(background, 0.88).apply(context);
        context.rectangle(layout.x + 4.0, row_y, layout.width - 8.0, BLOCK_LINE_HEIGHT);
        let _ = context.fill();
        palette.accent_hover.apply(context);
        context.rectangle(layout.x + 4.0, row_y + 2.0, 2.0, BLOCK_LINE_HEIGHT - 4.0);
        let _ = context.fill();
    }

    if current_block {
        palette.border.mix(palette.accent, 0.38).apply(context);
        context.set_line_width(1.5);
    } else {
        palette.border.apply(context);
        context.set_line_width(1.0);
    }

    context.rectangle(
        layout.x + 0.5,
        layout.y + 0.5,
        layout.width - 1.0,
        layout.height - 1.0,
    );

    let _ = context.stroke();
}

fn truncate_cfg_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }

    if max_chars == 0 {
        return String::new();
    }

    if max_chars == 1 {
        return String::from("…");
    }

    let mut truncated = text.chars().take(max_chars - 1).collect::<String>();
    truncated.push('…');

    truncated
}

fn cfg_address(address: u64, pointer_bits: u32) -> String {
    let width = usize::try_from(pointer_bits / 4).unwrap_or(16).clamp(8, 16);

    format!("0x{address:0width$x}")
}

fn schedule_cfg_center(
    scrolled: &gtk::ScrolledWindow,
    graph: &Rc<RefCell<Option<ControlFlowGraph>>>,
    generation: &Rc<Cell<u64>>,
) {
    let center = graph.borrow().as_ref().and_then(|graph| {
        let block = graph.current_block?;
        let layout = graph_layout(graph, f64::from(graph_content_width(graph)));
        let layout = layout.get(block)?;

        Some(layout.y + layout.height / 2.0)
    });

    let Some(center) = center else {
        return;
    };

    let next_generation = generation.get().wrapping_add(1);
    generation.set(next_generation);
    let scrolled = scrolled.clone();
    let generation = Rc::clone(generation);

    glib::idle_add_local_once(move || {
        if generation.get() != next_generation {
            return;
        }

        let adjustment = scrolled.vadjustment();
        let target = center - adjustment.page_size() / 2.0;
        let maximum = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
        adjustment.set_value(target.clamp(adjustment.lower(), maximum));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instruction(address: u64, text: &str) -> Instruction {
        Instruction {
            address: format!("0x{address:x}"),
            function: String::from("worker"),
            offset: String::new(),
            opcodes: Some(String::from("90")),
            text: text.to_owned(),
            source: None,
        }
    }

    fn graph(instructions: &[Instruction], pc: u64) -> ControlFlowGraph {
        graph_for_arch(instructions, pc, TargetArchitecture::X86_64)
    }

    fn graph_for_arch(
        instructions: &[Instruction],
        pc: u64,
        architecture: TargetArchitecture,
    ) -> ControlFlowGraph {
        build_control_flow_graph(
            instructions,
            Some(pc),
            architecture,
            64,
            cfg_signature(instructions, architecture, 64),
        )
        .expect("test disassembly must produce a CFG")
    }

    #[test]
    fn splits_conditional_and_unconditional_x86_flow_into_basic_blocks() {
        let instructions = [
            instruction(0x100, "cmp $0x0,%eax"),
            instruction(0x102, "je 0x108 <worker+8>"),
            instruction(0x104, "add $0x1,%eax"),
            instruction(0x106, "jmp 0x10a <worker+10>"),
            instruction(0x108, "xor %eax,%eax"),
            instruction(0x10a, "ret"),
        ];

        let graph = graph(&instructions, 0x104);
        assert_eq!(graph.blocks.len(), 4);
        assert_eq!(graph.current_block, Some(1));

        let edges = graph
            .edges
            .iter()
            .map(|edge| (edge.from, edge.to, edge.kind))
            .collect::<Vec<_>>();

        assert_eq!(
            edges,
            vec![
                (0, 1, CfgEdgeKind::Fallthrough),
                (0, 2, CfgEdgeKind::Branch),
                (1, 3, CfgEdgeKind::Branch),
                (2, 3, CfgEdgeKind::Fallthrough),
            ]
        );
    }

    #[test]
    fn calls_stay_in_the_callers_basic_block() {
        let instructions = [
            instruction(0x100, "push %rbp"),
            instruction(0x101, "call 0x900 <helper>"),
            instruction(0x106, "add $0x1,%eax"),
            instruction(0x109, "ret"),
        ];

        let graph = graph(&instructions, 0x101);
        assert_eq!(graph.blocks.len(), 1);
        assert!(graph.edges.is_empty());
        assert_eq!(graph.external_edges, 0);
    }

    #[test]
    fn indirect_jumps_do_not_invent_a_fallthrough_edge() {
        let instructions = [
            instruction(0x100, "test %rax,%rax"),
            instruction(0x103, "jmp *%rax"),
            instruction(0x105, "ret"),
        ];

        let graph = graph(&instructions, 0x103);
        assert_eq!(graph.blocks.len(), 2);
        assert!(graph.edges.is_empty());
        assert_eq!(graph.external_edges, 1);
    }

    #[test]
    fn classifies_direct_and_indirect_x86_control_flow_operands() {
        for instruction in ["jmp 0x401000", "je 0x401000", "call 0x401000"] {
            let (mnemonic, operands) =
                normalized_instruction_parts(instruction, TargetArchitecture::X86_64);

            assert_eq!(
                direct_control_flow_address(&mnemonic, operands, TargetArchitecture::X86_64),
                Some(0x401000),
                "{instruction}"
            );
        }

        for instruction in [
            "jmp *%rax",
            "jmp *0x8(%rax)",
            "jmp *0x401000",
            "call *%rax",
            "jmp rax",
            "jmp [rax]",
            "jmp qword ptr [0x401000]",
            "call rax",
        ] {
            let (mnemonic, operands) =
                normalized_instruction_parts(instruction, TargetArchitecture::X86_64);

            assert_eq!(
                direct_control_flow_address(&mnemonic, operands, TargetArchitecture::X86_64),
                None,
                "{instruction}"
            );
        }
    }

    #[test]
    fn indirect_memory_jumps_do_not_create_known_cfg_edges() {
        for jump in ["jmp *0x8(%rax)", "jmp *0x401000", "jmp [rax]"] {
            let instructions = [instruction(0x100, jump), instruction(0x108, "ret")];
            let graph = graph(&instructions, 0x100);

            assert!(graph.edges.is_empty(), "{jump}");
            assert_eq!(graph.external_edges, 1, "{jump}");
        }
    }

    #[test]
    fn bounded_graph_keeps_the_current_region_of_a_large_function() {
        let instructions = (0..1_200_u64)
            .map(|index| instruction(0x1000 + index, "nop"))
            .collect::<Vec<_>>();

        let graph = graph(&instructions, 0x1000 + 1_000);
        assert!(graph.truncated);
        assert_eq!(graph.current_block, Some(0));

        assert!(
            graph
                .instructions
                .iter()
                .any(|instruction| instruction.address == 0x1000 + 1_000)
        );

        assert!(graph.instructions.len() <= MAX_CFG_INSTRUCTIONS);
    }

    #[test]
    fn long_block_window_keeps_a_stable_height_and_includes_the_pc() {
        let mut instructions = (0..12_u64)
            .map(|index| instruction(0x100 + index, "nop"))
            .collect::<Vec<_>>();

        instructions.push(instruction(0x10c, "ret"));

        for pc in [0x100, 0x106, 0x10c] {
            let graph = graph(&instructions, pc);
            let rendered = rendered_instructions(&graph, &graph.blocks[0]);
            assert_eq!(rendered.len(), MAX_RENDERED_BLOCK_INSTRUCTIONS);

            assert!(rendered.iter().any(|line| matches!(
                line,
                RenderedInstruction::Instruction(index)
                    if graph.instructions[*index].address == pc
            )));
        }
    }

    #[test]
    fn branch_heavy_graph_reports_when_a_pc_has_left_its_rendered_window() {
        let instructions = (0..400_u64)
            .map(|index| {
                let address = 0x1000 + index * 2;

                instruction(address, &format!("je 0x{address:x}"))
            })
            .collect::<Vec<_>>();

        let graph = graph(&instructions, 0x1000 + 300 * 2);
        assert_eq!(graph.blocks.len(), MAX_CFG_BLOCKS);
        assert!(graph.contains_rendered_address(0x1000 + 300 * 2));
        assert!(!graph.contains_rendered_address(0x1000 + 10 * 2));
    }

    #[test]
    fn mips_delay_slot_stays_with_its_branch_block() {
        let instructions = [
            instruction(0x100, "beq $a0,$zero,0x10c"),
            instruction(0x104, "nop"),
            instruction(0x108, "addiu $v0,$v0,1"),
            instruction(0x10c, "jr $ra"),
            instruction(0x110, "nop"),
        ];

        let graph = graph_for_arch(&instructions, 0x100, TargetArchitecture::Mips64);
        assert_eq!(graph.blocks.len(), 3);
        assert_eq!((graph.blocks[0].start, graph.blocks[0].end), (0, 2));
        assert_eq!((graph.blocks[2].start, graph.blocks[2].end), (3, 5));

        assert!(graph.edges.contains(&CfgEdge {
            from: 0,
            to: 1,
            kind: CfgEdgeKind::Fallthrough,
        }));

        assert!(graph.edges.contains(&CfgEdge {
            from: 0,
            to: 2,
            kind: CfgEdgeKind::Branch,
        }));
    }
}
