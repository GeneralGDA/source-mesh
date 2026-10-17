use std::io::Write;
use std::num::NonZeroUsize;

use anyhow::Result;
use askama::Template;

use crate::cycle_presentation::group_color;
use crate::export::rendering::edge_style::CYCLE_EDGE_WIDTH;
use crate::export::rendering::edge_style::TEST_EDGE_COLOR;
use crate::export::rendering::edge_style::edge_label;
use crate::export::rendering::exporter::Exporter;
use crate::export::rendering::exporter::Options;
use crate::export::rendering::structure::FolderEvent;
use crate::export::rendering::structure::Hierarchy;
use crate::export::rendering::structure::IndexedGraph;
use crate::model::DependencyKind;
use crate::model::FolderGraph;

pub struct Dot;

struct NodePresentation {
    index: usize,
    label: String,
    depth: usize,
}

struct GroupPresentation {
    node: NodePresentation,
    dependency: bool,
}

enum NodeEvent {
    Leaf(NodePresentation),
    EnterGroup(GroupPresentation),
    LeaveGroup(usize),
}

struct EdgePresentation {
    source: usize,
    target: usize,
    label: String,
    color: Option<String>,
    test: bool,
    cycle_group: Option<NonZeroUsize>,
}

#[derive(Template)]
#[template(path = "export/dot/template.txt", escape = "none")]
struct DotTemplate {
    strict: bool,
    node_events: Vec<NodeEvent>,
    edges: Vec<EdgePresentation>,
}

impl DotTemplate {
    #[must_use]
    fn new(graph: &IndexedGraph<'_>, hierarchy: Option<&Hierarchy<'_>>) -> Self {
        let node_events = hierarchy.map_or_else(
            || graph.nodes().iter().map(|(label, &index)| {
                NodeEvent::Leaf(NodePresentation { index, label: escaped_label(label), depth: 1 })
            }).collect(),
            |hierarchy| {
                let mut depth = 1;
                hierarchy.events().map(|event| match event {
                    FolderEvent::Enter(folder) => {
                        let label = escaped_label(folder.display_label());
                        let node = NodePresentation { index: folder.index(), label, depth };
                        if folder.children().is_empty() {
                            NodeEvent::Leaf(node)
                        } else {
                            depth += 1;
                            NodeEvent::EnterGroup(GroupPresentation { node, dependency: folder.dependency() })
                        }
                    }
                    FolderEvent::Leave => {
                        depth -= 1;
                        NodeEvent::LeaveGroup(depth)
                    }
                }).collect()
            },
        );
        let edges = graph.edges().iter().map(|edge| {
            let test = edge.kind() == DependencyKind::Test;
            let cycle_color = edge.cycle_group().map(|group| group_color(group, graph.cycle_group_count()));
            EdgePresentation {
                source: edge.source(),
                target: edge.target(),
                label: edge_label(edge.kind(), edge.weight()),
                color: cycle_color.or_else(|| test.then(|| TEST_EDGE_COLOR.to_owned())),
                test,
                cycle_group: edge.cycle_group().map(|group| group.number()),
            }
        }).collect();
        Self {
            strict: graph.edges().iter().all(|edge| edge.kind() == DependencyKind::Production),
            node_events,
            edges,
        }
    }
}

impl Exporter for Dot {
    fn write(&self, graph: &FolderGraph, output: &mut dyn Write) -> Result<()> {
        self.write_with_options(graph, &Options::default(), output)
    }

    fn write_grouped(&self, graph: &FolderGraph, output: &mut dyn Write) -> Result<()> {
        self.write_with_options(graph, &Options::builder().group_folders(true).build(), output)
    }

    fn write_with_options(&self, graph: &FolderGraph, options: &Options<'_>, output: &mut dyn Write) -> Result<()> {
        let indexed_graph = IndexedGraph::with_cycles(graph, options.cycles())?;
        let hierarchy = if options.group_folders() { Some(Hierarchy::new(&indexed_graph)?) } else { None };
        let template = DotTemplate::new(&indexed_graph, hierarchy.as_ref());
        template.write_into(output)?;
        Ok(())
    }
}

#[must_use]
fn escaped_label(label: &str) -> String {
    label.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r")
}
