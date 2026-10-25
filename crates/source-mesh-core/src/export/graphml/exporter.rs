use std::collections::BTreeMap;
use std::io::Write;
use std::num::NonZeroUsize;
use std::slice;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use askama::Template;

use crate::cycle_presentation::group_color;
use crate::export::rendering::edge_style::CYCLE_EDGE_WIDTH;
use crate::export::rendering::edge_style::COLOR_EDGE_TEST;
use crate::export::rendering::edge_style::edge_label;
use crate::export::rendering::edge_style::kind_name;
use crate::export::rendering::exporter::Exporter;
use crate::export::rendering::exporter::Options;
use crate::export::rendering::structure::Folder;
use crate::export::rendering::structure::FolderEvent;
use crate::export::rendering::structure::Hierarchy;
use crate::export::rendering::structure::IndexedGraph;
use crate::model::DependencyKind;
use crate::model::FolderGraph;

pub(in crate::export) const NAMESPACE_GRAPHML: &str = "http://graphml.graphdrawing.org/xmlns";
pub(in crate::export) const NAMESPACE_YWORKS: &str = "http://www.yworks.com/xml/graphml";
const COLOR_GRAPHICS_DEFAULT: &str = "#47699B";
const LAYOUT_OVERFLOW: &str = "GraphML layout exceeds the supported coordinate range";

pub struct GraphMl;

struct Position {
    horizontal: usize,
    vertical: usize,
}

struct NodeVisual<'label> {
    index: usize,
    label: &'label str,
    display_label: &'label str,
    position: Position,
    width: usize,
    height: usize,
}

impl<'label> NodeVisual<'label> {
    fn new(index: usize, label: &'label str, position: Position) -> Result<Self> {
        let longest_line = label.lines().map(|line| line.chars().count()).max().unwrap_or(0);
        let (width, height) = text_dimensions(longest_line, label.lines().count())?;
        Ok(Self { index, label, display_label: label, position, width, height })
    }

    fn child_position(&self) -> Result<Position> {
        Ok(Position {
            horizontal: self.position.horizontal.checked_add(20).context(LAYOUT_OVERFLOW)?,
            vertical: self.position.vertical.checked_add(self.height).context(LAYOUT_OVERFLOW)?,
        })
    }

    fn include_child(&mut self, child: &Self) -> Result<()> {
        self.width = self.width.max(child.width.checked_add(40).context(LAYOUT_OVERFLOW)?);
        let child_bottom = self.height.checked_add(child.height);
        self.height = child_bottom.and_then(|height| height.checked_add(20)).context(LAYOUT_OVERFLOW)?;
        Ok(())
    }

    fn next_root_vertical_position(&self) -> Result<usize> {
        let root_bottom = self.position.vertical.checked_add(self.height);
        root_bottom.and_then(|position| position.checked_add(40)).context(LAYOUT_OVERFLOW)
    }
}

fn text_dimensions(longest_line: usize, line_count: usize) -> Result<(usize, usize)> {
    let padded_width = longest_line.checked_mul(8).and_then(|width| width.checked_add(40));
    let width = padded_width.context("GraphML label width exceeds the supported size")?;
    let padded_height = line_count.max(1).checked_mul(20).and_then(|height| height.checked_add(20));
    let height = padded_height.context("GraphML label height exceeds the supported size")?;
    Ok((width, height))
}

struct LayoutFrame<'hierarchy, 'label> {
    visual: NodeVisual<'label>,
    children: slice::Iter<'hierarchy, usize>,
}

impl<'hierarchy, 'label> LayoutFrame<'hierarchy, 'label> {
    fn new(folder: &'hierarchy Folder<'label>, position: Position) -> Result<Self> {
        let mut visual = NodeVisual::new(folder.index(), folder.display_label(), position)?;
        visual.label = folder.label();
        Ok(Self { visual, children: folder.children().iter() })
    }
}

struct Layout<'label> {
    visuals: BTreeMap<usize, NodeVisual<'label>>,
}

impl<'label> Layout<'label> {
    fn grouped(hierarchy: &Hierarchy<'label>) -> Result<Self> {
        let mut visuals = BTreeMap::new();
        let mut vertical_position = 0;
        for root in hierarchy.roots() {
            let mut work = vec![LayoutFrame::new(root, Position { horizontal: 0, vertical: vertical_position })?];
            while let Some(mut frame) = work.pop() {
                if let Some(&child) = frame.children.next() {
                    let position = frame.visual.child_position()?;
                    work.push(frame);
                    work.push(LayoutFrame::new(hierarchy.folder(child), position)?);
                } else {
                    if let Some(parent) = work.last_mut() {
                        parent.visual.include_child(&frame.visual)?;
                    } else {
                        vertical_position = frame.visual.next_root_vertical_position()?;
                    }
                    visuals.insert(frame.visual.index, frame.visual);
                }
            }
        }
        Ok(Self { visuals })
    }

    fn flat(graph: &IndexedGraph<'label>) -> Result<Self> {
        let mut visuals = BTreeMap::new();
        let mut vertical_position = 0;
        for (label, &index) in graph.nodes() {
            let visual = NodeVisual::new(index, label, Position { horizontal: 0, vertical: vertical_position })?;
            vertical_position = visual.next_root_vertical_position()?;
            visuals.insert(index, visual);
        }
        Ok(Self { visuals })
    }

    #[must_use]
    fn visual(&self, index: usize) -> &NodeVisual<'label> {
        let Some(visual) = self.visuals.get(&index) else {
            unreachable!("Every visible hierarchy folder must have a computed layout");
        };
        visual
    }
}

#[must_use]
fn escaped_label(label: &str) -> String {
    // XML readers normalize literal CR to LF.
    quick_xml::escape::escape(label).replace('\r', "&#13;")
}

fn validate_graph(graph: &FolderGraph, cycle_group_count: usize) -> Result<()> {
    // Cycle group numbers are contiguous and one-based, so the count bounds every edge's group.
    i32::try_from(cycle_group_count).context("cycle group exceeds GraphML's signed int range")?;
    ensure!(
        graph.nodes().iter().all(|node| node.label().chars().all(|character| matches!(character,
            '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}'
        ))),
        "a folder label contains a character forbidden in XML 1.0"
    );
    for weight in graph.edges().values() {
        i64::try_from(weight.value()).context("edge weight exceeds GraphML's signed long range")?;
    }
    Ok(())
}

struct NodePresentation {
    index: usize,
    label: String,
    display_label: String,
    horizontal: usize,
    vertical: usize,
    width: usize,
    height: usize,
}

impl NodePresentation {
    #[must_use]
    fn new(visual: &NodeVisual<'_>) -> Self {
        Self {
            index: visual.index,
            label: escaped_label(visual.label),
            display_label: escaped_label(visual.display_label),
            horizontal: visual.position.horizontal,
            vertical: visual.position.vertical,
            width: visual.width,
            height: visual.height,
        }
    }
}

struct GroupRealizer {
    width: usize,
    height: usize,
    closed: bool,
}

struct GroupPresentation {
    node: NodePresentation,
    realizers: [GroupRealizer; 2],
}

enum NodeEvent {
    Leaf(NodePresentation),
    EnterGroup(GroupPresentation),
    LeaveGroup,
}

struct EdgePresentation {
    index: usize,
    source: usize,
    target: usize,
    weight: usize,
    kind: &'static str,
    label: String,
    color: String,
    line_style: &'static str,
    width: &'static str,
    cycle_group: Option<NonZeroUsize>,
}

#[derive(Template)]
#[template(path = "export/graphml/template.jinja2", escape = "none")]
struct GraphMlTemplate {
    graphml_namespace: &'static str,
    yworks_namespace: &'static str,
    default_graphics_color: &'static str,
    has_cycle_groups: bool,
    node_events: Vec<NodeEvent>,
    edges: Vec<EdgePresentation>,
}

impl GraphMlTemplate {
    #[must_use]
    fn new(graph: &IndexedGraph<'_>, layout: &Layout<'_>, hierarchy: Option<&Hierarchy<'_>>) -> Self {
        let node_events = hierarchy.map_or_else(
            || layout.visuals.values().map(|visual| NodeEvent::Leaf(NodePresentation::new(visual))).collect(),
            |hierarchy| hierarchy.events().map(|event| match event {
                FolderEvent::Enter(folder) => {
                    let visual = layout.visual(folder.index());
                    let node = NodePresentation::new(visual);
                    if folder.children().is_empty() {
                        NodeEvent::Leaf(node)
                    } else {
                        NodeEvent::EnterGroup(GroupPresentation {
                            node,
                            realizers: [
                                GroupRealizer { width: visual.width, height: visual.height, closed: false },
                                GroupRealizer { width: 160, height: 50, closed: true },
                            ],
                        })
                    }
                }
                FolderEvent::Leave => NodeEvent::LeaveGroup,
            }).collect(),
        );
        let edges = graph.edges().iter().enumerate().map(|(index, edge)| {
            let (color, line_style) = match edge.kind() {
                DependencyKind::Production => (COLOR_GRAPHICS_DEFAULT, "line"),
                DependencyKind::Test => (COLOR_EDGE_TEST, "dashed"),
            };
            let cycle_color = edge.cycle_group().map(|group| group_color(group, graph.cycle_group_count()));
            EdgePresentation {
                index,
                source: edge.source(),
                target: edge.target(),
                weight: edge.weight().value(),
                kind: kind_name(edge.kind()),
                label: edge_label(edge.kind(), edge.weight()),
                color: cycle_color.unwrap_or_else(|| color.to_owned()),
                line_style,
                width: if edge.cycle_group().is_some() { CYCLE_EDGE_WIDTH } else { "1.0" },
                cycle_group: edge.cycle_group().map(|group| group.number()),
            }
        }).collect();
        Self {
            graphml_namespace: NAMESPACE_GRAPHML,
            yworks_namespace: NAMESPACE_YWORKS,
            default_graphics_color: COLOR_GRAPHICS_DEFAULT,
            has_cycle_groups: graph.cycle_group_count() != 0,
            node_events,
            edges,
        }
    }
}

impl Exporter for GraphMl {
    fn write(&self, graph: &FolderGraph, output: &mut dyn Write) -> Result<()> {
        self.write_with_options(graph, &Options::default(), output)
    }

    fn write_grouped(&self, graph: &FolderGraph, output: &mut dyn Write) -> Result<()> {
        self.write_with_options(graph, &Options::builder().group_folders(true).build(), output)
    }

    fn write_with_options(&self, graph: &FolderGraph, options: &Options<'_>, output: &mut dyn Write) -> Result<()> {
        let indexed_graph = IndexedGraph::with_cycles(graph, options.cycles())?;
        validate_graph(graph, indexed_graph.cycle_group_count())?;
        let hierarchy = if options.group_folders() { Some(Hierarchy::new(&indexed_graph)?) } else { None };
        let layout = if let Some(hierarchy) = hierarchy.as_ref() {
            Layout::grouped(hierarchy)?
        } else {
            Layout::flat(&indexed_graph)?
        };
        let template = GraphMlTemplate::new(&indexed_graph, &layout, hierarchy.as_ref());
        template.write_into(output)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use claims::assert_ok;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::NodeVisual;
    use super::Position;
    use super::text_dimensions;
    use super::validate_graph;
    use crate::model::FolderGraph;

    #[rstest]
    #[case("", (40, 40))]
    #[case("ab\n\u{3bb}", (56, 60))]
    #[case("x\r\n", (48, 40))]
    fn test_label_dimensions(#[case] label: &str, #[case] expected_dimensions: (usize, usize)) {
        let system_under_test = NodeVisual::new(0, label, Position { horizontal: 0, vertical: 0 });

        let visual = assert_ok!(system_under_test, "Representable label must produce node geometry: {label:?}");
        let dimensions = (visual.width, visual.height);

        assert_eq!(dimensions, expected_dimensions, "Label measurement changed line or Unicode character handling");
    }

    #[rstest]
    #[case(usize::MAX, 1)]
    #[case(usize::MAX.div_euclid(8), 1)]
    #[case(0, usize::MAX)]
    #[case(0, usize::MAX.div_euclid(20))]
    fn test_label_dimension_overflow(#[case] longest_line: usize, #[case] line_count: usize) {
        let system_under_test = text_dimensions(longest_line, line_count);

        assert!(system_under_test.is_err(), "Unrepresentable label dimensions must be a size error");
    }

    #[test]
    fn test_label_dimension_boundary() {
        let longest_line = (usize::MAX - 40).div_euclid(8);
        let line_count = (usize::MAX - 20).div_euclid(20);

        let system_under_test = text_dimensions(longest_line, line_count);

        let dimensions = assert_ok!(system_under_test, "Label dimensions at the representable boundary must succeed");
        assert_eq!(
            dimensions,
            (longest_line * 8 + 40, line_count * 20 + 20),
            "Representable label dimensions must not be capped"
        );
    }

    #[rstest]
    #[case(40, usize::MAX, 40, false)]
    #[case(40, usize::MAX - 39, 40, false)]
    #[case(1, 40, usize::MAX, false)]
    #[case(40, 40, usize::MAX - 40, false)]
    #[case(40, usize::MAX - 40, 40, true)]
    fn test_group_dimensions(
        #[case] parent_height: usize,
        #[case] child_width: usize,
        #[case] child_height: usize,
        #[case] representable: bool,
    ) {
        let mut system_under_test = NodeVisual {
            index: 0,
            label: "",
            display_label: "",
            position: Position { horizontal: 0, vertical: 0 },
            width: 40,
            height: parent_height,
        };
        let child = NodeVisual {
            index: 1,
            label: "",
            display_label: "",
            position: Position { horizontal: 20, vertical: 40 },
            width: child_width,
            height: child_height,
        };

        let result = system_under_test.include_child(&child);

        if representable {
            assert_ok!(result, "Representable group bounds including child dimensions and padding must succeed");
        } else {
            assert!(result.is_err(), "Group bounds must reject overflowing child dimensions or padding");
        }
    }

    #[rstest]
    #[case(usize::MAX, 0, 40, false)]
    #[case(0, usize::MAX, 40, false)]
    #[case(usize::MAX - 20, usize::MAX - 40, 40, true)]
    fn test_child_position(
        #[case] horizontal: usize,
        #[case] vertical: usize,
        #[case] height: usize,
        #[case] representable: bool,
    ) {
        let position = Position { horizontal, vertical };
        let system_under_test = NodeVisual { index: 0, label: "", display_label: "", position, width: 40, height };

        let result = system_under_test.child_position();

        if representable {
            assert_ok!(result, "Representable child coordinates including offsets must succeed");
        } else {
            assert!(result.is_err(), "Child coordinates must reject overflowing offsets");
        }
    }

    #[rstest]
    #[case(usize::MAX, 1, false)]
    #[case(usize::MAX - 40, 1, false)]
    #[case(usize::MAX - 80, 40, true)]
    fn test_root_spacing(#[case] vertical: usize, #[case] height: usize, #[case] representable: bool) {
        let system_under_test = NodeVisual {
            index: 0,
            label: "",
            display_label: "",
            position: Position { horizontal: 0, vertical },
            width: 40,
            height,
        };

        let result = system_under_test.next_root_vertical_position();

        if representable {
            assert_ok!(result, "Representable root coordinates including height and spacing must succeed");
        } else {
            assert!(result.is_err(), "Root coordinates must reject overflowing height or spacing");
        }
    }

    #[test]
    fn test_cycle_group_signed_int_range() {
        let fixture = FolderGraph::default();

        let maximum = usize::try_from(i32::MAX);
        let maximum = assert_ok!(maximum, "GraphML signed-int maximum must fit in the platform's index type");
        let system_under_test = validate_graph;
        let boundary = system_under_test(&fixture, maximum);
        let overflow = system_under_test(&fixture, maximum + 1);

        assert_ok!(boundary, "Cycle-group count at the GraphML signed-int maximum must be accepted");
        assert!(overflow.is_err(), "Cycle-group count above the GraphML signed-int maximum must be rejected");
    }
}
