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

use super::rendering::edge_style::TEST_EDGE_COLOR;
use super::rendering::edge_style::kind_name;
use crate::export::Format;
use crate::export::rendering::serialization_fixture::parallel_dependency_fixture;
use crate::model::DependencyKind;
use pretty_assertions::assert_eq;

fn dot_attribute<'edge>(edge: &'edge Edge, name: &str) -> Result<&'edge Id> {
    let attribute = edge.attributes.iter().find(|attribute| attribute.0 == Id::Plain(name.into()));
    attribute.map(|attribute| &attribute.1).context("DOT edge attribute is missing")
}

fn graphml_data<'document>(node: Node<'document, '_>, key: &str) -> Result<&'document str> {
    let data = node.children().find(|child| child.attribute("key") == Some(key)).and_then(|child| child.text());
    data.context("GraphML edge data is missing")
}

#[rstest]
#[case(Format::Dot, false, concat!(
    "digraph folders {\n", "  rankdir=LR\n",
    "  n0[label=\"src/api/http\" shape=box]\n", "  n1[label=\"src/storage\" shape=box]\n",
    "  n0 -> n1 [label=\"2\"]\n", "  n0 -> n1 [label=\"test: 3\" color=\"#D97706\" style=dashed]\n", "}\n",
))]
#[case(Format::Dot, true, concat!(
    "digraph folders {\n", "  rankdir=LR\n", "  subgraph cluster_n2 {\n", "    label=\"api\"\n",
    "    n0[label=\"api/http\" shape=box]\n", "  }\n", "  n1[label=\"storage\" shape=box]\n",
    "  n0 -> n1 [label=\"2\"]\n", "  n0 -> n1 [label=\"test: 3\" color=\"#D97706\" style=dashed]\n", "}\n",
))]
#[case(Format::Mermaid, false, concat!(
    "flowchart LR\n", "    n0[\"src/api/http\"]\n", "    n1[\"src/storage\"]\n",
    "    n0 -->|2| n1\n", "    n0 -.->|test: 3| n1\n", "    linkStyle 1 stroke:#D97706,color:#D97706\n",
))]
#[case(Format::Mermaid, true, concat!(
    "flowchart LR\n", "    subgraph g2[\"api\"]\n", "    n0[\"api/http\"]\n", "    end\n",
    "    n1[\"storage\"]\n", "    n0 -->|2| n1\n", "    n0 -.->|test: 3| n1\n",
    "    linkStyle 1 stroke:#D97706,color:#D97706\n",
))]
#[case(Format::Graphml, false, include_str!("graphml/test_reference_flat.graphml"))]
#[case(Format::Graphml, true, include_str!("graphml/test_reference_grouped.graphml"))]
fn test_serialization_output_equivalence(#[case] format: Format, #[case] grouped: bool, #[case] expected: &str) -> Result<()> {
    let fixture = parallel_dependency_fixture()?;
    let system_under_test = format.exporter();
    let mut output = Vec::new();

    if grouped {
        system_under_test.write_grouped(&fixture, &mut output)?;
    } else {
        system_under_test.write(&fixture, &mut output)?;
    }

    let output = String::from_utf8(output)?;
    let expected = expected.replace("\r\n", "\n");

    assert_eq!(output, expected, "Serialization, ordering, identifiers, or layout changed");
    Ok(())
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
#[case(Format::Graphml)]
fn test_parallel_dependency_styles(#[case] format: Format, #[values(false, true)] grouped: bool) -> Result<()> {
    let fixture = parallel_dependency_fixture()?;
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
            let Graph::DiGraph { strict, stmts: statements, .. } = graphviz_rust::parse(&output).map_err(Error::msg)? else {
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
            assert_eq!(
                dot_attribute(test_edge, "label")?,
                &Id::Escaped(format!("\"{test_label}\"")),
                "Test DOT label is missing"
            );
            assert_eq!(
                dot_attribute(test_edge, "color")?,
                &Id::Escaped(format!("\"{TEST_EDGE_COLOR}\"")),
                "Test DOT color is incorrect"
            );
            assert_eq!(dot_attribute(test_edge, "style")?, &Id::Plain("dashed".into()), "Test DOT edge must be dashed");
        }
        Format::Mermaid => {
            let production_edges: Vec<_> = output.lines().filter(|line| line.contains(" -->|")).collect();
            let test_edges: Vec<_> = output.lines().filter(|line| line.contains(" -.->|")).collect();
            let styles: Vec<_> = output.lines().filter(|line| line.contains("linkStyle ")).collect();

            assert_eq!(production_edges, ["    n0 -->|2| n1"], "Production Mermaid edge changed");
            assert_eq!(
                test_edges,
                [format!("    n0 -.->|{test_label}| n1")],
                "Test Mermaid edge is missing or has incorrect styling"
            );
            let expected_style = format!("    linkStyle 1 stroke:{TEST_EDGE_COLOR},color:{TEST_EDGE_COLOR}");
            assert_eq!(styles, [expected_style], "Mermaid styles target the wrong parallel edge");
        }
        Format::Graphml => {
            let document = Document::parse(&output)?;
            let edges: Vec<_> = document.descendants().filter(|node| node.has_tag_name("edge")).collect();
            let production_edge = *edges.first().context("Production GraphML edge is missing")?;
            let test_edge = *edges.last().context("Test GraphML edge is missing")?;
            let production_style = production_edge.descendants().find(|node| node.has_tag_name("LineStyle"));
            let production_style = production_style.context("Production GraphML style is missing")?;
            let test_style = test_edge.descendants().find(|node| node.has_tag_name("LineStyle"));
            let test_style = test_style.context("Test GraphML style is missing")?;
            let visible_label = test_edge.descendants().find(|node| node.has_tag_name("EdgeLabel"));
            let visible_label = visible_label.and_then(|node| node.text()).context("Test GraphML visual label is missing")?;

            assert_eq!(edges.len(), 2, "GraphML merged parallel dependency kinds");
            assert_ne!(production_edge.attribute("id"), test_edge.attribute("id"), "Parallel GraphML edges share an identifier");
            assert!(
                production_edge.attribute("source") == test_edge.attribute("source")
                    && production_edge.attribute("target") == test_edge.attribute("target"),
                "Parallel GraphML endpoints changed"
            );
            assert_eq!(
                graphml_data(production_edge, "kind")?,
                kind_name(DependencyKind::Production),
                "Production GraphML dependency kind is missing"
            );
            assert_eq!(
                graphml_data(test_edge, "kind")?,
                kind_name(DependencyKind::Test),
                "Test GraphML dependency kind is missing"
            );
            assert_eq!(graphml_data(production_edge, "weight")?, "2", "Production GraphML numeric weight changed");
            assert_eq!(graphml_data(test_edge, "weight")?, "3", "Test GraphML numeric weight changed");
            assert!(
                production_style.attribute("type") == Some("line")
                    && production_style.attribute("color") != Some(TEST_EDGE_COLOR),
                "Production GraphML style changed"
            );
            assert!(
                test_style.attribute("type") == Some("dashed") && test_style.attribute("color") == Some(TEST_EDGE_COLOR),
                "Test GraphML edge is not orange and dashed"
            );
            assert_eq!(visible_label, test_label.as_str(), "Test GraphML label lost its dependency kind");
        }
    }
    Ok(())
}
