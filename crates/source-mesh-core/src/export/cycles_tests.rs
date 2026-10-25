use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::num::NonZeroUsize;

use anyhow::Context as _;
use anyhow::Error;
use anyhow::Result;
use anyhow::bail;
use claims::assert_ok;
use claims::assert_some;
use graphviz_rust::dot_structures::Edge;
use graphviz_rust::dot_structures::Graph;
use graphviz_rust::dot_structures::Id;
use graphviz_rust::dot_structures::Stmt as Statement;
use roxmltree::Document;
use roxmltree::Node;
use rstest::rstest;

use pretty_assertions::assert_eq;
use super::rendering::edge_style::CYCLE_EDGE_WIDTH;
use crate::cycles::CycleGroupId;
use crate::cycles::analyze_folders;
use crate::export::Options;
use crate::export::Format;
use crate::export::rendering::serialization_fixture::cycle_graph_fixture as graph_fixture;
use crate::model::DependencyCount;
use crate::model::DependencyKind;
use crate::model::FolderEdge;
use crate::model::FolderGraph;
use crate::model::FolderNode;

fn dot_statements(output: &str) -> Result<Vec<Statement>> {
    let Graph::DiGraph { stmts: statements, .. } = graphviz_rust::parse(output).map_err(Error::msg)? else {
        bail!("Expected a directed DOT graph");
    };
    Ok(statements)
}

fn dot_attribute<'edge>(edge: &'edge Edge, name: &str) -> Result<&'edge Id> {
    edge.attributes.iter().find(|attribute| attribute.0 == Id::Plain(name.into()))
        .map(|attribute| &attribute.1).context("DOT attribute is missing")
}

fn graphml_data<'document>(node: Node<'document, '_>, key: &str) -> Result<&'document str> {
    node.children().find(|child| child.attribute("key") == Some(key))
        .and_then(|child| child.text()).context("GraphML data is missing")
}

fn remember_group_color(colors: &mut BTreeMap<NonZeroUsize, String>, group: CycleGroupId, color: &str) {
    let previous = colors.entry(group.number()).or_insert_with(|| color.into());
    assert_eq!(previous.as_str(), color, "Edges in one cycle group have different colors");
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
#[case(Format::Graphml)]
fn test_cycle_highlighting_topology_and_dependency_kinds(#[case] format: Format, #[values(false, true)] grouped: bool) {
    let fixture = graph_fixture();
    let fixture = assert_ok!(fixture, "Cycle-highlighting fixture must form a valid folder graph");
    let cycles = analyze_folders(&fixture);
    let cycles = assert_ok!(cycles, "Cycle-highlighting fixture must support cycle analysis");
    let system_under_test = format.exporter();
    let mut default_output = Vec::new();
    let mut highlighted_output = Vec::new();

    let default_options = Options::builder().group_folders(grouped).build();
    let default_write_result = system_under_test.write_with_options(&fixture, &default_options, &mut default_output);
    assert_ok!(default_write_result, "Cycle fixture must serialize without highlighting, grouped={grouped}");
    let highlighted_options = Options::builder().group_folders(grouped).cycles(&cycles).build();
    let highlighted_write_result = system_under_test.write_with_options(&fixture, &highlighted_options, &mut highlighted_output);
    assert_ok!(highlighted_write_result, "Cycle fixture must serialize with highlighting, grouped={grouped}");
    let default_output = String::from_utf8(default_output);
    let default_output = assert_ok!(default_output, "Default cycle export must be UTF-8");
    let highlighted_output = String::from_utf8(highlighted_output);
    let highlighted_output = assert_ok!(highlighted_output, "Highlighted cycle export must be UTF-8");
    let mut colors = BTreeMap::new();

    assert_eq!(cycles.group_count(), 3, "Fixture must include two multi-node cycles and a self-loop");
    match format {
        Format::Dot => {
            let default_statements = dot_statements(&default_output);
            let default_statements = assert_ok!(default_statements, "Default DOT must parse as a directed graph");
            let highlighted_statements = dot_statements(&highlighted_output);
            let highlighted_statements = assert_ok!(highlighted_statements, "Highlighted DOT must parse as a directed graph");
            let default_nodes: Vec<_> = default_statements.iter()
                .filter(|statement| !matches!(**statement, Statement::Edge(_))).collect();
            let highlighted_nodes: Vec<_> = highlighted_statements.iter()
                .filter(|statement| !matches!(**statement, Statement::Edge(_))).collect();
            let default_edges: Vec<_> = default_statements.iter().filter_map(|statement| {
                if let Statement::Edge(ref edge) = *statement { Some(edge) } else { None }
            }).collect();
            let highlighted_edges: Vec<_> = highlighted_statements.iter().filter_map(|statement| {
                if let Statement::Edge(ref edge) = *statement { Some(edge) } else { None }
            }).collect();

            assert_eq!(default_nodes, highlighted_nodes, "Highlighting changed nodes or synthetic ancestor groups");
            assert_eq!(highlighted_edges.len(), fixture.edges().len(), "Highlighting added or removed DOT edges");
            for (edge, (default_edge, highlighted_edge)) in fixture.edges().keys()
                .zip(default_edges.iter().zip(&highlighted_edges))
            {
                assert_eq!(&default_edge.ty, &highlighted_edge.ty, "Highlighting changed DOT endpoints");
                let default_label = dot_attribute(default_edge, "label");
                let default_label = assert_ok!(default_label, "Default DOT edge must have a dependency label");
                let highlighted_label = dot_attribute(highlighted_edge, "label");
                let highlighted_label = assert_ok!(highlighted_label, "Highlighted DOT edge must have a dependency label");
                assert_eq!(default_label, highlighted_label, "Highlighting changed a dependency weight or test label");
                if let Some(group) = cycles.edge_group(edge) {
                    let color = dot_attribute(highlighted_edge, "color");
                    let color = assert_ok!(color, "Highlighted DOT cycle edge must have a color");
                    let color = if let Id::Escaped(ref color) = *color { Some(color) } else { None };
                    let color = assert_some!(color, "Highlighted DOT cycle color must be quoted");
                    remember_group_color(&mut colors, group, color);
                    let width = dot_attribute(highlighted_edge, "penwidth");
                    let width = assert_ok!(width, "Highlighted DOT cycle edge must have a pen width");
                    assert_eq!(width, &Id::Plain(CYCLE_EDGE_WIDTH.into()), "Cycle edge has the wrong DOT width");
                    let identifier = dot_attribute(highlighted_edge, "cycle_group");
                    let identifier = assert_ok!(identifier, "Highlighted DOT edge must identify its cycle group");
                    assert_eq!(identifier, &Id::Plain(group.number().to_string()), "DOT cycle group identifier is missing");
                } else {
                    assert_eq!(default_edge, highlighted_edge, "Bridge between cycle groups changed style");
                }
                if edge.kind() == DependencyKind::Test {
                    let style = dot_attribute(highlighted_edge, "style");
                    let style = assert_ok!(style, "Highlighted DOT test edge must retain a line style");
                    assert_eq!(style, &Id::Plain("dashed".into()), "Highlighting removed the test-edge dash style");
                }
            }
        }
        Format::Mermaid => {
            let default_structure: Vec<_> = default_output.lines().filter(|line| !line.contains("linkStyle ")).collect();
            let highlighted_structure: Vec<_> = highlighted_output.lines().filter(|line| !line.contains("linkStyle ")).collect();

            assert_eq!(default_structure, highlighted_structure, "Highlighting changed Mermaid topology, labels, or groups");
            for (index, edge) in fixture.edges().keys().enumerate() {
                let prefix = format!("    linkStyle {index} ");
                let default_style = default_output.lines().find(|line| line.starts_with(&prefix));
                let highlighted_style = highlighted_output.lines().find(|line| line.starts_with(&prefix));
                if let Some(group) = cycles.edge_group(edge) {
                    let style = assert_some!(highlighted_style, "Mermaid cycle edge {index} must have a style");
                    let attributes = style.strip_prefix(&prefix);
                    let attributes = assert_some!(attributes, "Mermaid cycle style must begin with its edge index");
                    let attributes: BTreeMap<_, _> = attributes.split(',').map(|attribute| {
                        let pair = attribute.split_once(':');
                        assert_some!(pair, "Mermaid style must contain a name and value: {attribute}")
                    }).collect();
                    let color = attributes.get("stroke");
                    let color = assert_some!(color, "Mermaid cycle edge {index} must have a stroke color");
                    let expected_width = format!("{CYCLE_EDGE_WIDTH}px");
                    remember_group_color(&mut colors, group, color);

                    assert_eq!(attributes.get("color"), Some(color), "Cycle label and arrow colors differ");
                    let width = attributes.get("stroke-width").copied();
                    assert_eq!(width, Some(expected_width.as_str()), "Mermaid cycle edge has the wrong width");
                } else {
                    assert_eq!(default_style, highlighted_style, "Bridge between Mermaid cycle groups changed style");
                }
            }
        }
        Format::Graphml => {
            let default_document = Document::parse(&default_output);
            let default_document = assert_ok!(default_document, "Default GraphML must be valid XML");
            let highlighted_document = Document::parse(&highlighted_output);
            let highlighted_document = assert_ok!(highlighted_document, "Highlighted GraphML must be valid XML");
            let default_nodes: Vec<_> = default_document.descendants().filter(|node| node.has_tag_name("node")).map(|node| {
                let markup = default_output.get(node.range());
                assert_some!(markup, "Default GraphML node range must refer to its serialized XML")
            }).collect();
            let highlighted_nodes = highlighted_document.descendants().filter(|node| node.has_tag_name("node"));
            let highlighted_nodes: Vec<_> = highlighted_nodes.map(|node| {
                let markup = highlighted_output.get(node.range());
                assert_some!(markup, "Highlighted GraphML node range must refer to its serialized XML")
            }).collect();
            let default_edges: Vec<_> = default_document.descendants().filter(|node| node.has_tag_name("edge")).collect();
            let highlighted_edges: Vec<_> = highlighted_document.descendants().filter(|node| node.has_tag_name("edge")).collect();

            assert_eq!(default_nodes, highlighted_nodes, "Highlighting changed GraphML nodes or synthetic ancestor groups");
            assert_eq!(highlighted_edges.len(), fixture.edges().len(), "Highlighting added or removed GraphML edges");
            for (edge, (default_edge, highlighted_edge)) in fixture.edges().keys()
                .zip(default_edges.iter().zip(&highlighted_edges))
            {
                assert!(["id", "source", "target"].into_iter()
                    .all(|attribute| default_edge.attribute(attribute) == highlighted_edge.attribute(attribute)),
                    "Highlighting changed GraphML edge identifiers or endpoints");
                let default_kind = graphml_data(*default_edge, "kind");
                let default_kind = assert_ok!(default_kind, "Default GraphML edge must contain dependency-kind data");
                let highlighted_kind = graphml_data(*highlighted_edge, "kind");
                let highlighted_kind = assert_ok!(highlighted_kind, "Highlighted GraphML edge must contain dependency-kind data");
                assert_eq!(default_kind, highlighted_kind, "Highlighting changed dependency kinds");
                let default_weight = graphml_data(*default_edge, "weight");
                let default_weight = assert_ok!(default_weight, "Default GraphML edge must contain weight data");
                let highlighted_weight = graphml_data(*highlighted_edge, "weight");
                let highlighted_weight = assert_ok!(highlighted_weight, "Highlighted GraphML edge must contain weight data");
                assert_eq!(default_weight, highlighted_weight, "Highlighting changed dependency weights");
                let style = highlighted_edge.descendants().find(|node| node.has_tag_name("LineStyle"));
                let style = assert_some!(style, "Highlighted GraphML edge must have a line style");
                if let Some(group) = cycles.edge_group(edge) {
                    let color = style.attribute("color");
                    let color = assert_some!(color, "Highlighted GraphML cycle edge must have a color");
                    remember_group_color(&mut colors, group, color);
                    assert_eq!(style.attribute("width"), Some(CYCLE_EDGE_WIDTH), "GraphML cycle edge has the wrong width");
                    let identifier = graphml_data(*highlighted_edge, "cycle_group");
                    let identifier = assert_ok!(identifier, "Highlighted GraphML edge must identify its cycle group");
                    assert_eq!(identifier, group.number().to_string(), "GraphML cycle group identifier is missing");
                } else {
                    assert_eq!(default_output.get(default_edge.range()), highlighted_output.get(highlighted_edge.range()),
                        "Bridge between GraphML cycle groups changed style");
                }
                let expected_line = if edge.kind() == DependencyKind::Test { "dashed" } else { "line" };
                assert_eq!(style.attribute("type"), Some(expected_line), "Highlighting changed the test-edge line style");
            }
        }
    }
    assert_eq!(colors.len(), cycles.group_count(), "Some cycle groups were not highlighted");
    assert_eq!(colors.values().collect::<BTreeSet<_>>().len(), colors.len(), "Different cycle groups share a color");
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
#[case(Format::Graphml)]
fn test_default_export_options(#[case] format: Format, #[values(false, true)] grouped: bool) {
    let fixture = graph_fixture();
    let fixture = assert_ok!(fixture, "Default-options fixture must form a valid folder graph");
    let system_under_test = format.exporter();
    let mut legacy_output = Vec::new();
    let mut options_output = Vec::new();

    let legacy_write_result = if grouped {
        system_under_test.write_grouped(&fixture, &mut legacy_output)
    } else {
        system_under_test.write(&fixture, &mut legacy_output)
    };
    assert_ok!(legacy_write_result, "Legacy export entry point must serialize successfully, grouped={grouped}");
    let options = Options::builder().group_folders(grouped).build();
    let options_write_result = system_under_test.write_with_options(&fixture, &options, &mut options_output);
    assert_ok!(options_write_result, "Default export options must serialize successfully, grouped={grouped}");

    assert_eq!(legacy_output, options_output, "Options changed the default serializer output");
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
#[case(Format::Graphml)]
fn test_acyclic_highlighting(#[case] format: Format, #[values(false, true)] grouped: bool) {
    let source = FolderNode::new("src/api".into());
    let target = FolderNode::new("src/storage".into());
    let fixture = FolderGraph::new(
        [source.clone(), target.clone()].into(),
        BTreeMap::from([(FolderEdge::new(source, target, DependencyKind::Test), DependencyCount::new(1))]),
    );
    let fixture = assert_ok!(fixture, "Acyclic dependency must form a valid folder graph");
    let cycles = analyze_folders(&fixture);
    let cycles = assert_ok!(cycles, "Acyclic graph must support cycle analysis");
    let system_under_test = format.exporter();
    let mut default_output = Vec::new();
    let mut highlighted_output = Vec::new();

    let default_options = Options::builder().group_folders(grouped).build();
    let default_write_result = system_under_test.write_with_options(&fixture, &default_options, &mut default_output);
    assert_ok!(default_write_result, "Acyclic graph must serialize without highlighting, grouped={grouped}");
    let highlighted_options = Options::builder().group_folders(grouped).cycles(&cycles).build();
    let highlighted_write_result = system_under_test.write_with_options(&fixture, &highlighted_options, &mut highlighted_output);
    assert_ok!(highlighted_write_result, "Acyclic graph must serialize with highlighting enabled, grouped={grouped}");

    assert_eq!(cycles.group_count(), 0, "Fixture unexpectedly contains cycles");
    assert_eq!(default_output, highlighted_output, "Highlighting changed an acyclic graph");
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
#[case(Format::Graphml)]
fn test_highlighting_analysis_topology_validation(#[case] format: Format, #[values(false, true)] grouped: bool) {
    let source = FolderNode::new("src/api".into());
    let target = FolderNode::new("src/storage".into());
    let cyclic_fixture = FolderGraph::new(
        [source.clone(), target.clone()].into(),
        BTreeMap::from([
            (FolderEdge::new(source.clone(), target.clone(), DependencyKind::Production), DependencyCount::new(1)),
            (FolderEdge::new(target.clone(), source.clone(), DependencyKind::Production), DependencyCount::new(1)),
        ]),
    );
    let cyclic_fixture = assert_ok!(cyclic_fixture, "Original cycle topology must form a valid graph");
    let fixture = FolderGraph::new(
        cyclic_fixture.nodes().clone(),
        BTreeMap::from([(FolderEdge::new(source, target, DependencyKind::Production), DependencyCount::new(1))]),
    );
    let fixture = assert_ok!(fixture, "Changed acyclic topology must still form a valid graph");
    let cycles = analyze_folders(&cyclic_fixture);
    let cycles = assert_ok!(cycles, "Original cycle topology must support analysis");
    let system_under_test = format.exporter();
    let mut output = Vec::new();

    let options = Options::builder().group_folders(grouped).cycles(&cycles).build();
    let result = system_under_test.write_with_options(&fixture, &options, &mut output);

    assert!(result.is_err(), "Exporter accepted analysis of different topology");
    assert!(output.is_empty(), "Exporter wrote partial output for analysis of different topology");
}
