use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io::Write;
use std::num::NonZeroUsize;

use anyhow::Context as _;
use anyhow::Error;
use anyhow::Result;
use anyhow::bail;
use graphviz_rust::dot_structures::Edge;
use graphviz_rust::dot_structures::Graph;
use graphviz_rust::dot_structures::Id;
use graphviz_rust::dot_structures::Stmt as Statement;
use roxmltree::Document;
use roxmltree::Node;
use rstest::rstest;

use pretty_assertions::assert_eq;
use super::edge_style::CYCLE_EDGE_WIDTH;
use crate::cycles::CycleGroupId;
use crate::cycles::analyze_folders;
use crate::export::Options;
use crate::export::Exporter;
use crate::export::Format;
use crate::model::DependencyCount;
use crate::model::DependencyKind;
use crate::model::FolderEdge;
use crate::model::FolderGraph;
use crate::model::FolderNode;

fn graph_fixture() -> Result<FolderGraph> {
    FolderGraph::new(
        ["src/first/a", "src/first/b", "src/isolated", "src/second/a", "src/second/b", "src/third/self"]
            .map(String::from).map(FolderNode::new).into(),
        [
            ("src/first/a", "src/first/b", DependencyKind::Production, 2),
            ("src/first/a", "src/first/b", DependencyKind::Test, 3),
            ("src/first/b", "src/first/a", DependencyKind::Production, 4),
            ("src/first/b", "src/second/a", DependencyKind::Production, 5),
            ("src/first/b", "src/second/a", DependencyKind::Test, 9),
            ("src/second/a", "src/second/b", DependencyKind::Production, 6),
            ("src/second/b", "src/second/a", DependencyKind::Production, 7),
            ("src/third/self", "src/third/self", DependencyKind::Production, 8),
        ].into_iter().map(|(source, target, kind, weight)| (
            FolderEdge::new(FolderNode::new(source.into()), FolderNode::new(target.into()), kind),
            DependencyCount::new(weight),
        )).collect(),
    )
}

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
fn test_cycle_highlighting_topology_and_dependency_kinds(
    #[case] format: Format,
    #[values(false, true)] grouped: bool,
) -> Result<()> {
    let fixture = graph_fixture()?;
    let cycles = analyze_folders(&fixture)?;
    let system_under_test = format.exporter();
    let mut default_output = Vec::new();
    let mut highlighted_output = Vec::new();

    system_under_test.write_with_options(&fixture, &Options::new(grouped, None), &mut default_output)?;
    system_under_test.write_with_options(&fixture, &Options::new(grouped, Some(&cycles)), &mut highlighted_output)?;
    let default_output = String::from_utf8(default_output)?;
    let highlighted_output = String::from_utf8(highlighted_output)?;
    let mut colors = BTreeMap::new();

    assert_eq!(cycles.group_count(), 3, "Fixture must include two multi-node cycles and a self-loop");
    match format {
        Format::Dot => {
            let default_statements = dot_statements(&default_output)?;
            let highlighted_statements = dot_statements(&highlighted_output)?;
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
                assert_eq!(dot_attribute(default_edge, "label")?, dot_attribute(highlighted_edge, "label")?,
                    "Highlighting changed a dependency weight or test label");
                if let Some(group) = cycles.edge_group(edge) {
                    let Id::Escaped(ref color) = *dot_attribute(highlighted_edge, "color")? else {
                        bail!("Expected quoted DOT cycle color");
                    };
                    remember_group_color(&mut colors, group, color);
                    assert_eq!(dot_attribute(highlighted_edge, "penwidth")?, &Id::Plain(CYCLE_EDGE_WIDTH.into()),
                        "Cycle edge has the wrong DOT width");
                    assert_eq!(dot_attribute(highlighted_edge, "cycle_group")?, &Id::Plain(group.number().to_string()),
                        "DOT cycle group identifier is missing");
                } else {
                    assert_eq!(default_edge, highlighted_edge, "Bridge between cycle groups changed style");
                }
                if edge.kind() == DependencyKind::Test {
                    assert_eq!(dot_attribute(highlighted_edge, "style")?, &Id::Plain("dashed".into()),
                        "Highlighting removed the test-edge dash style");
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
                    let attributes: BTreeMap<_, _> = highlighted_style.context("Mermaid cycle style is missing")?
                        .strip_prefix(&prefix).context("Mermaid style prefix is missing")?
                        .split(',').map(|attribute| attribute.split_once(':').context("Invalid Mermaid style"))
                        .collect::<Result<_>>()?;
                    let color = attributes.get("stroke").context("Mermaid cycle color is missing")?;
                    let expected_width = format!("{CYCLE_EDGE_WIDTH}px");
                    remember_group_color(&mut colors, group, color);

                    assert_eq!(attributes.get("color"), Some(color), "Cycle label and arrow colors differ");
                    assert_eq!(attributes.get("stroke-width").copied(), Some(expected_width.as_str()), "Mermaid cycle edge has the wrong width");
                } else {
                    assert_eq!(default_style, highlighted_style, "Bridge between Mermaid cycle groups changed style");
                }
            }
        }
        Format::Graphml => {
            let default_document = Document::parse(&default_output)?;
            let highlighted_document = Document::parse(&highlighted_output)?;
            let default_nodes: Vec<_> = default_document.descendants().filter(|node| node.has_tag_name("node"))
                .map(|node| default_output.get(node.range()).context("Invalid node XML range"))
                .collect::<Result<_>>()?;
            let highlighted_nodes: Vec<_> = highlighted_document.descendants().filter(|node| node.has_tag_name("node"))
                .map(|node| highlighted_output.get(node.range()).context("Invalid node XML range"))
                .collect::<Result<_>>()?;
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
                assert_eq!(graphml_data(*default_edge, "kind")?, graphml_data(*highlighted_edge, "kind")?,
                    "Highlighting changed dependency kinds");
                assert_eq!(graphml_data(*default_edge, "weight")?, graphml_data(*highlighted_edge, "weight")?,
                    "Highlighting changed dependency weights");
                let style = highlighted_edge.descendants().find(|node| node.has_tag_name("LineStyle"))
                    .context("GraphML edge has no line style")?;
                if let Some(group) = cycles.edge_group(edge) {
                    remember_group_color(&mut colors, group, style.attribute("color").context("Cycle color is missing")?);
                    assert_eq!(style.attribute("width"), Some(CYCLE_EDGE_WIDTH),
                        "GraphML cycle edge has the wrong width");
                    assert_eq!(graphml_data(*highlighted_edge, "cycle_group")?, group.number().to_string(),
                        "GraphML cycle group identifier is missing");
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
    Ok(())
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
#[case(Format::Graphml)]
fn test_default_export_options(#[case] format: Format, #[values(false, true)] grouped: bool) -> Result<()> {
    let fixture = graph_fixture()?;
    let system_under_test = format.exporter();
    let mut legacy_output = Vec::new();
    let mut options_output = Vec::new();

    if grouped {
        system_under_test.write_grouped(&fixture, &mut legacy_output)?;
    } else {
        system_under_test.write(&fixture, &mut legacy_output)?;
    }
    system_under_test.write_with_options(&fixture, &Options::new(grouped, None), &mut options_output)?;

    assert_eq!(legacy_output, options_output, "Options changed the default serializer output");
    Ok(())
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
#[case(Format::Graphml)]
fn test_acyclic_highlighting(#[case] format: Format, #[values(false, true)] grouped: bool) -> Result<()> {
    let source = FolderNode::new("src/api".into());
    let target = FolderNode::new("src/storage".into());
    let fixture = FolderGraph::new(
        [source.clone(), target.clone()].into(),
        BTreeMap::from([(FolderEdge::new(source, target, DependencyKind::Test), DependencyCount::new(1))]),
    )?;
    let cycles = analyze_folders(&fixture)?;
    let system_under_test = format.exporter();
    let mut default_output = Vec::new();
    let mut highlighted_output = Vec::new();

    system_under_test.write_with_options(&fixture, &Options::new(grouped, None), &mut default_output)?;
    system_under_test.write_with_options(&fixture, &Options::new(grouped, Some(&cycles)), &mut highlighted_output)?;

    assert_eq!(cycles.group_count(), 0, "Fixture unexpectedly contains cycles");
    assert_eq!(default_output, highlighted_output, "Highlighting changed an acyclic graph");
    Ok(())
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
#[case(Format::Graphml)]
fn test_highlighting_analysis_topology_validation(
    #[case] format: Format,
    #[values(false, true)] grouped: bool,
) -> Result<()> {
    let source = FolderNode::new("src/api".into());
    let target = FolderNode::new("src/storage".into());
    let cyclic_fixture = FolderGraph::new(
        [source.clone(), target.clone()].into(),
        BTreeMap::from([
            (FolderEdge::new(source.clone(), target.clone(), DependencyKind::Production), DependencyCount::new(1)),
            (FolderEdge::new(target.clone(), source.clone(), DependencyKind::Production), DependencyCount::new(1)),
        ]),
    )?;
    let fixture = FolderGraph::new(
        cyclic_fixture.nodes().clone(),
        BTreeMap::from([(FolderEdge::new(source, target, DependencyKind::Production), DependencyCount::new(1))]),
    )?;
    let cycles = analyze_folders(&cyclic_fixture)?;
    let system_under_test = format.exporter();
    let mut output = Vec::new();

    let result = system_under_test.write_with_options(&fixture, &Options::new(grouped, Some(&cycles)), &mut output);

    assert!(result.is_err(), "Exporter accepted analysis of different topology");
    assert!(output.is_empty(), "Exporter wrote partial output for analysis of different topology");
    Ok(())
}

struct ExistingExporter;

impl Exporter for ExistingExporter {
    fn write(&self, graph: &FolderGraph, output: &mut dyn Write) -> Result<()> {
        writeln!(output, "flat {}", graph.nodes().len())?;
        Ok(())
    }

    fn write_grouped(&self, graph: &FolderGraph, output: &mut dyn Write) -> Result<()> {
        writeln!(output, "grouped {}", graph.nodes().len())?;
        Ok(())
    }
}

#[rstest]
#[case(false, "flat 0\n")]
#[case(true, "grouped 0\n")]
fn test_existing_exporter_default_dispatch(#[case] grouped: bool, #[case] expected: &str) -> Result<()> {
    let fixture = FolderGraph::default();
    let system_under_test = ExistingExporter;
    let mut output = Vec::new();

    system_under_test.write_with_options(&fixture, &Options::new(grouped, None), &mut output)?;

    assert_eq!(output, expected.as_bytes(), "Default options did not call the existing exporter method");
    Ok(())
}

#[test]
fn test_existing_exporter_unsupported_highlighting() -> Result<()> {
    let fixture = graph_fixture()?;
    let cycles = analyze_folders(&fixture)?;
    let system_under_test = ExistingExporter;
    let mut output = Vec::new();

    let result = system_under_test.write_with_options(&fixture, &Options::new(false, Some(&cycles)), &mut output);

    assert!(result.is_err(), "Unsupported highlighting was silently accepted");
    assert!(output.is_empty(), "Unsupported highlighting wrote partial output");
    Ok(())
}
