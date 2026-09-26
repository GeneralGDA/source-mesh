use std::collections::BTreeMap;

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
use super::edge_style::TEST_EDGE_COLOR;
use super::edge_style::kind_name;
use crate::export::Format;
use crate::model::DependencyCount;
use crate::model::DependencyKind;
use crate::model::FolderEdge;
use crate::model::FolderGraph;
use crate::model::FolderNode;

fn dot_attribute<'edge>(edge: &'edge Edge, name: &str) -> Result<&'edge Id> {
    edge.attributes.iter().find(|attribute| attribute.0 == Id::Plain(name.into()))
        .map(|attribute| &attribute.1).context("DOT edge attribute is missing")
}

fn graphml_data<'document>(node: Node<'document, '_>, key: &str) -> Result<&'document str> {
    node.children().find(|child| child.attribute("key") == Some(key))
        .and_then(|child| child.text()).context("GraphML edge data is missing")
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
#[case(Format::Graphml)]
fn test_parallel_dependency_styles(
    #[case] format: Format,
    #[values(false, true)] grouped: bool,
) -> Result<()> {
    let source = FolderNode::new("src/api/http".into());
    let target = FolderNode::new("src/storage".into());
    let fixture = FolderGraph::new(
        [source.clone(), target.clone()].into(),
        BTreeMap::from([
            (FolderEdge::new(source.clone(), target.clone(), DependencyKind::Production), DependencyCount::new(2)),
            (FolderEdge::new(source, target, DependencyKind::Test), DependencyCount::new(3)),
        ]),
    )?;
    let system_under_test = format.exporter();
    let mut output = Vec::new();

    if grouped {
        system_under_test.write_grouped(&fixture, &mut output)?;
    } else {
        system_under_test.write(&fixture, &mut output)?;
    }
    let output = String::from_utf8(output)?;
    let test_label = format!("{}: 3", kind_name(DependencyKind::Test));

    match format {
        Format::Dot => {
            let Graph::DiGraph { strict, stmts: statements, .. } =
                graphviz_rust::parse(&output).map_err(Error::msg)? else {
                bail!("Expected a directed DOT graph");
            };
            let edges: Vec<_> = statements.iter().filter_map(|statement| {
                if let Statement::Edge(ref edge) = *statement { Some(edge) } else { None }
            }).collect();
            let production_edge = edges.first().context("Production DOT edge is missing")?;
            let test_edge = edges.last().context("Test DOT edge is missing")?;

            assert!(!strict && edges.len() == 2, "DOT must preserve parallel dependency kinds");
            assert_eq!(&production_edge.ty, &test_edge.ty, "Parallel DOT edges have different endpoints");
            assert_eq!(dot_attribute(production_edge, "label")?, &Id::Escaped("\"2\"".into()), "Production DOT label changed");
            assert_eq!(production_edge.attributes.len(), 1, "Production DOT style changed");
            assert_eq!(dot_attribute(test_edge, "label")?, &Id::Escaped(format!("\"{test_label}\"")), "Test DOT label is missing");
            assert_eq!(dot_attribute(test_edge, "color")?, &Id::Escaped(format!("\"{TEST_EDGE_COLOR}\"")), "Test DOT color is incorrect");
            assert_eq!(dot_attribute(test_edge, "style")?, &Id::Plain("dashed".into()), "Test DOT edge must be dashed");
        }
        Format::Mermaid => {
            let production_edges: Vec<_> = output.lines().filter(|line| line.contains(" -->|")).collect();
            let test_edges: Vec<_> = output.lines().filter(|line| line.contains(" -.->|")).collect();
            let styles: Vec<_> = output.lines().filter(|line| line.contains("linkStyle ")).collect();

            assert_eq!(production_edges, ["    n0 -->|2| n1"], "Production Mermaid edge changed");
            assert_eq!(test_edges, [format!("    n0 -.->|{test_label}| n1")], "Test Mermaid edge is missing or has incorrect styling");
            assert_eq!(styles, [format!("    linkStyle 1 stroke:{TEST_EDGE_COLOR},color:{TEST_EDGE_COLOR}")], "Mermaid styles target the wrong parallel edge");
        }
        Format::Graphml => {
            let document = Document::parse(&output)?;
            let edges: Vec<_> = document.descendants().filter(|node| node.has_tag_name("edge")).collect();
            let production_edge = *edges.first().context("Production GraphML edge is missing")?;
            let test_edge = *edges.last().context("Test GraphML edge is missing")?;
            let production_style = production_edge.descendants().find(|node| node.has_tag_name("LineStyle"))
                .context("Production GraphML style is missing")?;
            let test_style = test_edge.descendants().find(|node| node.has_tag_name("LineStyle"))
                .context("Test GraphML style is missing")?;

            assert_eq!(edges.len(), 2, "GraphML merged parallel dependency kinds");
            assert_ne!(production_edge.attribute("id"), test_edge.attribute("id"), "Parallel GraphML edges share an identifier");
            assert!(production_edge.attribute("source") == test_edge.attribute("source")
                && production_edge.attribute("target") == test_edge.attribute("target"), "Parallel GraphML endpoints changed");
            assert_eq!(graphml_data(production_edge, "kind")?, kind_name(DependencyKind::Production), "Production GraphML dependency kind is missing");
            assert_eq!(graphml_data(test_edge, "kind")?, kind_name(DependencyKind::Test), "Test GraphML dependency kind is missing");
            assert_eq!(graphml_data(production_edge, "weight")?, "2", "Production GraphML numeric weight changed");
            assert_eq!(graphml_data(test_edge, "weight")?, "3", "Test GraphML numeric weight changed");
            assert!(production_style.attribute("type") == Some("line")
                && production_style.attribute("color") != Some(TEST_EDGE_COLOR), "Production GraphML style changed");
            assert!(test_style.attribute("type") == Some("dashed")
                && test_style.attribute("color") == Some(TEST_EDGE_COLOR), "Test GraphML edge is not orange and dashed");
            let visible_label = test_edge.descendants().find(|node| node.has_tag_name("EdgeLabel"))
                .and_then(|node| node.text()).context("Test GraphML visual label is missing")?;

            assert_eq!(visible_label, test_label.as_str(), "Test GraphML label lost its dependency kind");
        }
    }
    Ok(())
}
