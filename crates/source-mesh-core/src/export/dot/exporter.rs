use std::io::Write;
use std::num::NonZeroUsize;

use anyhow::Result;
use askama::Template;

use crate::cycle_presentation::group_color;
use crate::export::rendering::edge_style::CYCLE_EDGE_WIDTH;
use crate::export::rendering::edge_style::COLOR_EDGE_TEST;
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
#[template(path = "export/dot/template.jinja2", escape = "none")]
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
                color: cycle_color.or_else(|| test.then(|| COLOR_EDGE_TEST.to_owned())),
                test,
                cycle_group: edge.cycle_group().map(|group| group.number()),
            }
        }).collect();
        Self { strict: graph.edges().iter().all(|edge| edge.kind() == DependencyKind::Production), node_events, edges }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use claims::assert_ok;
    use claims::assert_some;
    use graphviz_rust::dot_structures::EdgeTy;
    use graphviz_rust::dot_structures::Graph;
    use graphviz_rust::dot_structures::Id;
    use graphviz_rust::dot_structures::Stmt as Statement;
    use graphviz_rust::dot_structures::Vertex;
    use pretty_assertions::assert_eq;

    use super::Dot;
    use crate::export::rendering::exporter::Exporter as _;
    use crate::export::rendering::grouping_fixture::dot_membership;
    use crate::export::rendering::grouping_fixture::dot_statements;
    use crate::export::rendering::grouping_fixture::hierarchy_fixture;
    use crate::export::rendering::render;
    use crate::export::rendering::render_grouped;
    use crate::export::rendering::serialization_fixture::graph_fixture;

    #[test]
    fn test_dot_labels_direction_weights_and_isolated_nodes() {
        let fixture = graph_fixture();
        let fixture = assert_ok!(fixture, "DOT label fixture must form a valid folder graph");
        let system_under_test = Dot;

        let output = render(&system_under_test, &fixture);
        let output = assert_ok!(output, "DOT must render punctuation labels and isolated nodes");
        let graph = graphviz_rust::parse(&output);
        let graph = assert_ok!(graph, "DOT output must parse as a graph");
        let directed = if let Graph::DiGraph { strict, stmts: statements, .. } = graph {
            Some((strict, statements))
        } else { None };
        let (strict, statements) = assert_some!(directed, "DOT output must be a directed graph");
        let nodes: Vec<_> = statements.iter().filter_map(|statement| {
            if let Statement::Node(ref node) = *statement { Some(node) } else { None }
        }).collect();
        let edges: Vec<_> = statements.iter().filter_map(|statement| {
            if let Statement::Edge(ref edge) = *statement { Some(edge) } else { None }
        }).collect();
        let first_node = nodes.first();
        let first_node = assert_some!(first_node, "DOT output must contain the first dependency node");
        let isolated_node = nodes.last();
        let isolated_node = assert_some!(isolated_node, "DOT output must contain the isolated node");
        let edge = edges.first();
        let edge = assert_some!(edge, "DOT output must contain the dependency edge");
        let endpoints = if let EdgeTy::Pair(Vertex::N(ref source), Vertex::N(ref target)) = edge.ty {
            Some((source, target))
        } else { None };
        let (source, target) = assert_some!(endpoints, "DOT dependency must be a simple directed edge");

        assert!(strict, "DOT graph must be strict");
        assert_eq!(nodes.len(), 3, "Expected three DOT nodes");
        let label = first_node.attributes.first();
        let label = assert_some!(label, "First DOT node must have a label attribute");
        assert_eq!(
            &label.1,
            &Id::Escaped("\"src/a\\\"<&\\\\#`\\n\\r\u{43f}\u{430}\u{43f}\u{43a}\u{430}\"".into()),
            "DOT punctuation label was not preserved or escaped correctly"
        );
        let isolated_label = isolated_node.attributes.first();
        let isolated_label = assert_some!(isolated_label, "Isolated DOT node must have a label attribute");
        assert_eq!(&isolated_label.1, &Id::Escaped("\"src/isolated\"".into()), "DOT isolated node label was not preserved");
        assert_eq!(edges.len(), 1, "Expected one DOT edge");
        assert_eq!(&source.0, &Id::Plain("n0".into()), "DOT edge source is incorrect");
        assert_eq!(&target.0, &Id::Plain("n1".into()), "DOT edge target is incorrect");
        let weight = edge.attributes.first();
        let weight = assert_some!(weight, "DOT edge must have a weight label");
        assert_eq!(&weight.1, &Id::Escaped("\"7\"".into()), "DOT edge weight is incorrect");
    }

    #[test]
    fn test_dot_nested_clusters_and_original_dependency_endpoints() {
        let fixture = hierarchy_fixture();
        let fixture = assert_ok!(fixture, "Nested-cluster fixture must form a valid folder graph");
        let system_under_test = Dot;
        let mut flat_output = Vec::new();

        let result = system_under_test.write(&fixture, &mut flat_output);
        assert_ok!(result, "DOT must serialize the flat hierarchy fixture");
        let grouped_output = render_grouped(&system_under_test, &fixture);
        let grouped_output = assert_ok!(grouped_output, "DOT must render nested folder clusters");
        let flat_output = String::from_utf8(flat_output);
        let flat_output = assert_ok!(flat_output, "Flat DOT output must be UTF-8");
        let flat_statements = dot_statements(&flat_output);
        let flat_statements = assert_ok!(flat_statements, "Flat DOT output must parse as a directed graph");
        let grouped_statements = dot_statements(&grouped_output);
        let grouped_statements = assert_ok!(grouped_statements, "Grouped DOT output must parse as a directed graph");
        let flat_edges: Vec<_> = flat_statements.iter().filter(|statement| matches!(**statement, Statement::Edge(_))).collect();
        let grouped_edges = grouped_statements.iter().filter(|statement| matches!(**statement, Statement::Edge(_)));
        let grouped_edges: Vec<_> = grouped_edges.collect();
        let mut membership = BTreeMap::new();
        dot_membership(&grouped_statements, &[], &mut membership);
        let expected = BTreeMap::from([
            ("n0".into(), vec![]),
            ("n1".into(), vec!["cluster_n1".into()]),
            ("n2".into(), vec!["cluster_n1".into(), "cluster_n2".into()]),
            ("n3".into(), vec!["cluster_n1".into(), "cluster_n2".into()]),
            ("n4".into(), vec!["cluster_n1".into()]),
            ("n5".into(), vec![]),
        ]);

        assert_eq!(flat_edges, grouped_edges, "grouping changed DOT dependency endpoints or weights");
        assert_eq!(membership, expected, "DOT hierarchy or parent dependency nodes are incorrect");
    }
}
