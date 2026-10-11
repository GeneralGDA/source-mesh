use std::io::Write;

use anyhow::Result;
use graphviz_rust::dot_structures::Attribute;
use graphviz_rust::dot_structures::Edge;
use graphviz_rust::dot_structures::EdgeTy;
use graphviz_rust::dot_structures::Graph;
use graphviz_rust::dot_structures::Id;
use graphviz_rust::dot_structures::Node;
use graphviz_rust::dot_structures::NodeId;
use graphviz_rust::dot_structures::Stmt as Statement;
use graphviz_rust::dot_structures::Subgraph;
use graphviz_rust::dot_structures::Vertex;
use graphviz_rust::printer::DotPrinter as _;
use graphviz_rust::printer::PrinterContext;

use super::edge_style::CYCLE_EDGE_WIDTH;
use super::edge_style::TEST_EDGE_COLOR;
use super::edge_style::edge_label;
use super::exporter::Exporter;
use super::exporter::Options;
use super::structure::Folder;
use super::structure::IndexedGraph;
use crate::cycle_presentation::group_color;
use crate::model::DependencyKind;
use crate::model::FolderGraph;

pub struct Dot;

impl Exporter for Dot {
    fn write(&self, graph: &FolderGraph, output: &mut dyn Write) -> Result<()> {
        self.write_with_options(graph, &Options::new(false, None), output)
    }

    fn write_grouped(&self, graph: &FolderGraph, output: &mut dyn Write) -> Result<()> {
        self.write_with_options(graph, &Options::new(true, None), output)
    }

    fn write_with_options(&self, graph: &FolderGraph, options: &Options<'_>, output: &mut dyn Write) -> Result<()> {
        let indexed_graph = IndexedGraph::with_cycles(graph, options.cycles())?;
        let nodes = if options.group_folders() {
            Folder::hierarchy(&indexed_graph)?.iter().map(folder_statement).collect()
        } else {
            indexed_graph.nodes().iter().map(|(label, &index)| node_statement(label, index)).collect()
        };
        write_graph(nodes, &indexed_graph, output)
    }
}

#[must_use]
fn node_identifier(index: usize) -> NodeId {
    NodeId(Id::Plain(format!("n{index}")), None)
}

#[must_use]
fn escaped_label(label: &str) -> Id {
    // graphviz-rust prints Id::Escaped verbatim, including its quotes.
    let label = label
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    Id::Escaped(format!("\"{label}\""))
}

#[must_use]
fn node_statement(label: &str, index: usize) -> Statement {
    Statement::Node(Node::new(
        node_identifier(index),
        vec![
            Attribute(Id::Plain("label".into()), escaped_label(label)),
            Attribute(Id::Plain("shape".into()), Id::Plain("box".into())),
        ],
    ))
}

#[must_use]
fn folder_statement(folder: &Folder<'_>) -> Statement {
    if folder.children().is_empty() {
        return node_statement(folder.display_label(), folder.index());
    }
    let mut statements = vec![Statement::Attribute(Attribute(
        Id::Plain("label".into()), escaped_label(folder.display_label()),
    ))];
    if folder.dependency() {
        statements.push(node_statement(folder.display_label(), folder.index()));
    }
    statements.extend(folder.children().iter().map(folder_statement));
    Statement::Subgraph(Subgraph {
        id: Id::Plain(format!("cluster_n{}", folder.index())),
        stmts: statements,
    })
}

fn write_graph(nodes: Vec<Statement>, graph: &IndexedGraph<'_>, output: &mut dyn Write) -> Result<()> {
    let mut statements = vec![Statement::Attribute(Attribute(
        Id::Plain("rankdir".into()), Id::Plain("LR".into()),
    ))];
    statements.extend(nodes);
    statements.extend(graph.edges().iter().map(|edge| {
        let mut attributes = vec![Attribute(
            Id::Plain("label".into()), escaped_label(&edge_label(edge.kind(), edge.weight())),
        )];
        let color = edge.cycle_group().map(|group| group_color(group, graph.cycle_group_count()))
            .or_else(|| (edge.kind() == DependencyKind::Test).then(|| TEST_EDGE_COLOR.to_owned()));
        if let Some(color) = color {
            attributes.push(Attribute(Id::Plain("color".into()), escaped_label(&color)));
        }
        if edge.kind() == DependencyKind::Test {
            attributes.push(Attribute(Id::Plain("style".into()), Id::Plain("dashed".into())));
        }
        if let Some(group) = edge.cycle_group() {
            attributes.push(Attribute(Id::Plain("penwidth".into()), Id::Plain(CYCLE_EDGE_WIDTH.into())));
            attributes.push(Attribute(Id::Plain("cycle_group".into()), Id::Plain(group.number().to_string())));
        }
        Statement::Edge(Edge {
            ty: EdgeTy::Pair(
                Vertex::N(node_identifier(edge.source())),
                Vertex::N(node_identifier(edge.target())),
            ),
            attributes,
        })
    }));
    let dot_graph = Graph::DiGraph {
        id: Id::Plain("folders".into()),
        strict: graph.edges().iter().all(|edge| edge.kind() == DependencyKind::Production),
        stmts: statements,
    };
    writeln!(output, "{}", dot_graph.print(&mut PrinterContext::default()))?;
    Ok(())
}
