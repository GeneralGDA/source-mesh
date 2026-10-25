use anyhow::Context as _;
use anyhow::Result;
use claims::assert_ok;
use claims::assert_some;
use graphviz_rust::dot_structures::Edge;
use graphviz_rust::dot_structures::Graph;
use graphviz_rust::dot_structures::Id;
use graphviz_rust::dot_structures::Stmt as Statement;
use roxmltree::Document;
use roxmltree::Node;
use rstest::rstest;

use super::rendering::edge_style::COLOR_EDGE_TEST;
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
fn test_serialization_output_equivalence(#[case] format: Format, #[case] grouped: bool, #[case] expected: &str) {
    let fixture = parallel_dependency_fixture();
    let fixture = assert_ok!(fixture, "Serialization-reference fixture must form a valid folder graph");
    let system_under_test = format.exporter();
    let mut output = Vec::new();

    let result = if grouped {
        system_under_test.write_grouped(&fixture, &mut output)
    } else {
        system_under_test.write(&fixture, &mut output)
    };
    assert_ok!(result, "Reference graph must serialize successfully, grouped={grouped}");

    let output = String::from_utf8(output);
    let output = assert_ok!(output, "Reference graph serialization must be UTF-8");
    let expected = expected.replace("\r\n", "\n");

    assert_eq!(output, expected, "Serialization, ordering, identifiers, or layout changed");
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
#[case(Format::Graphml)]
fn test_parallel_dependency_styles(#[case] format: Format, #[values(false, true)] grouped: bool) {
    let fixture = parallel_dependency_fixture();
    let fixture = assert_ok!(fixture, "Parallel-dependency fixture must form a valid folder graph");
    let system_under_test = format.exporter();
    let mut output = Vec::new();

    let result = if grouped {
        system_under_test.write_grouped(&fixture, &mut output)
    } else {
        system_under_test.write(&fixture, &mut output)
    };
    assert_ok!(result, "Parallel dependency kinds must serialize successfully, grouped={grouped}");
    let output = String::from_utf8(output);
    let output = assert_ok!(output, "Parallel dependency serialization must be UTF-8");
    let test_label = format!("{}: 3", kind_name(DependencyKind::Test));

    match format {
        Format::Dot => {
            let graph = graphviz_rust::parse(&output);
            let graph = assert_ok!(graph, "Parallel-dependency DOT must parse as a graph");
            let directed = if let Graph::DiGraph { strict, stmts: statements, .. } = graph {
                Some((strict, statements))
            } else { None };
            let (strict, statements) = assert_some!(directed, "Parallel-dependency DOT must be directed");
            let edges: Vec<_> = statements.iter().filter_map(|statement| {
                if let Statement::Edge(ref edge) = *statement { Some(edge) } else { None }
            }).collect();
            let production_edge = edges.first();
            let production_edge = assert_some!(production_edge, "DOT must contain the production dependency edge");
            let test_edge = edges.last();
            let test_edge = assert_some!(test_edge, "DOT must contain the test dependency edge");

            assert!(!strict && edges.len() == 2, "DOT must preserve parallel dependency kinds");
            assert_eq!(&production_edge.ty, &test_edge.ty, "Parallel DOT edges have different endpoints");
            let production_label = dot_attribute(production_edge, "label");
            let production_label = assert_ok!(production_label, "Production DOT edge must have a label");
            assert_eq!(production_label, &Id::Escaped("\"2\"".into()), "Production DOT label changed");
            assert_eq!(production_edge.attributes.len(), 1, "Production DOT style changed");
            let label = dot_attribute(test_edge, "label");
            let label = assert_ok!(label, "Test DOT edge must have a label");
            assert_eq!(label, &Id::Escaped(format!("\"{test_label}\"")), "Test DOT label is missing");
            let color = dot_attribute(test_edge, "color");
            let color = assert_ok!(color, "Test DOT edge must have a color");
            assert_eq!(color, &Id::Escaped(format!("\"{COLOR_EDGE_TEST}\"")), "Test DOT color is incorrect");
            let style = dot_attribute(test_edge, "style");
            let style = assert_ok!(style, "Test DOT edge must have a line style");
            assert_eq!(style, &Id::Plain("dashed".into()), "Test DOT edge must be dashed");
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
            let expected_style = format!("    linkStyle 1 stroke:{COLOR_EDGE_TEST},color:{COLOR_EDGE_TEST}");
            assert_eq!(styles, [expected_style], "Mermaid styles target the wrong parallel edge");
        }
        Format::Graphml => {
            let document = Document::parse(&output);
            let document = assert_ok!(document, "Parallel-dependency GraphML must be valid XML");
            let edges: Vec<_> = document.descendants().filter(|node| node.has_tag_name("edge")).collect();
            let production_edge = edges.first().copied();
            let production_edge = assert_some!(production_edge, "GraphML must contain the production dependency edge");
            let test_edge = edges.last().copied();
            let test_edge = assert_some!(test_edge, "GraphML must contain the test dependency edge");
            let production_style = production_edge.descendants().find(|node| node.has_tag_name("LineStyle"));
            let production_style = assert_some!(production_style, "Production GraphML edge must have a line style");
            let test_style = test_edge.descendants().find(|node| node.has_tag_name("LineStyle"));
            let test_style = assert_some!(test_style, "Test GraphML edge must have a line style");
            let visible_label = test_edge.descendants().find(|node| node.has_tag_name("EdgeLabel"));
            let visible_label = visible_label.and_then(|node| node.text());
            let visible_label = assert_some!(visible_label, "Test GraphML edge must have visual label text");

            assert_eq!(edges.len(), 2, "GraphML merged parallel dependency kinds");
            assert_ne!(production_edge.attribute("id"), test_edge.attribute("id"), "Parallel GraphML edges share an identifier");
            assert!(
                production_edge.attribute("source") == test_edge.attribute("source")
                    && production_edge.attribute("target") == test_edge.attribute("target"),
                "Parallel GraphML endpoints changed"
            );
            let production_kind = graphml_data(production_edge, "kind");
            let production_kind = assert_ok!(production_kind, "Production GraphML edge must carry dependency-kind data");
            assert_eq!(production_kind, kind_name(DependencyKind::Production), "Production GraphML dependency kind is missing");
            let test_kind = graphml_data(test_edge, "kind");
            let test_kind = assert_ok!(test_kind, "Test GraphML edge must carry dependency-kind data");
            assert_eq!(test_kind, kind_name(DependencyKind::Test), "Test GraphML dependency kind is missing");
            let production_weight = graphml_data(production_edge, "weight");
            let production_weight = assert_ok!(production_weight, "Production GraphML edge must carry numeric weight data");
            assert_eq!(production_weight, "2", "Production GraphML numeric weight changed");
            let test_weight = graphml_data(test_edge, "weight");
            let test_weight = assert_ok!(test_weight, "Test GraphML edge must carry numeric weight data");
            assert_eq!(test_weight, "3", "Test GraphML numeric weight changed");
            assert!(
                production_style.attribute("type") == Some("line")
                    && production_style.attribute("color") != Some(COLOR_EDGE_TEST),
                "Production GraphML style changed"
            );
            assert!(
                test_style.attribute("type") == Some("dashed") && test_style.attribute("color") == Some(COLOR_EDGE_TEST),
                "Test GraphML edge is not orange and dashed"
            );
            assert_eq!(visible_label, test_label.as_str(), "Test GraphML label lost its dependency kind");
        }
    }
}
