use std::collections::BTreeMap;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::bail;
use graphviz_rust::dot_structures::Graph;
use graphviz_rust::dot_structures::Id;
use graphviz_rust::dot_structures::Stmt as Statement;
use rstest::rstest;

use pretty_assertions::assert_eq;
use crate::export::Exporter;
use crate::export::Format;
use crate::model::DependencyCount;
use crate::model::DependencyKind;
use crate::model::FolderEdge;
use crate::model::FolderGraph;
use crate::model::FolderNode;

fn hierarchy_fixture() -> Result<FolderGraph> {
    FolderGraph::new(
        [".", "src", "src/api", "src/api/http", "src/storage", "tests"]
            .map(String::from).map(FolderNode::new).into(),
        BTreeMap::from([
            (FolderEdge::new(FolderNode::new(".".into()), FolderNode::new("src".into()), DependencyKind::Production), DependencyCount::new(2)),
            (FolderEdge::new(FolderNode::new("src".into()), FolderNode::new("src/api".into()), DependencyKind::Production), DependencyCount::new(3)),
            (FolderEdge::new(FolderNode::new("src/api".into()), FolderNode::new("src/api".into()), DependencyKind::Production), DependencyCount::new(11)),
            (FolderEdge::new(FolderNode::new("src/api/http".into()), FolderNode::new("src/storage".into()), DependencyKind::Production), DependencyCount::new(7)),
        ]),
    )
}

fn render_grouped(exporter: &dyn Exporter, graph: &FolderGraph) -> Result<String> {
    let mut output = Vec::new();
    exporter.write_grouped(graph, &mut output)?;
    Ok(String::from_utf8(output)?)
}

fn dot_statements(output: &str) -> Result<Vec<Statement>> {
    let Graph::DiGraph { stmts: statements, .. } =
        graphviz_rust::parse(output).map_err(anyhow::Error::msg)? else {
        bail!("expected a directed DOT graph");
    };
    Ok(statements)
}

fn dot_membership(
    statements: &[Statement],
    groups: &[String],
    membership: &mut BTreeMap<String, Vec<String>>,
) -> Result<()> {
    for statement in statements {
        match *statement {
            Statement::Node(ref node) => {
                let Id::Plain(ref identifier) = node.id.0 else {
                    bail!("expected plain DOT node identifier");
                };
                let previous = membership.insert(identifier.clone(), groups.to_vec());
                assert!(previous.is_none(), "DOT node was declared more than once");
            }
            Statement::Subgraph(ref group) => {
                let Id::Plain(ref identifier) = group.id else {
                    bail!("expected plain DOT group identifier");
                };
                let mut ancestors = groups.to_vec();
                ancestors.push(identifier.clone());
                dot_membership(&group.stmts, &ancestors, membership)?;
            }
            Statement::Edge(_) => assert!(groups.is_empty(), "dependency edges must stay at graph scope"),
            Statement::Attribute(_) | Statement::GAttribute(_) => {}
        }
    }
    Ok(())
}

#[test]
fn test_dot_nested_clusters_and_original_dependency_endpoints() -> Result<()> {
    let fixture = hierarchy_fixture()?;
    let system_under_test = Format::Dot.exporter();
    let mut flat_output = Vec::new();

    system_under_test.write(&fixture, &mut flat_output)?;
    let grouped_output = render_grouped(system_under_test.as_ref(), &fixture)?;
    let flat_statements = dot_statements(&String::from_utf8(flat_output)?)?;
    let grouped_statements = dot_statements(&grouped_output)?;
    let flat_edges: Vec<_> = flat_statements.iter().filter(|statement| matches!(**statement, Statement::Edge(_))).collect();
    let grouped_edges: Vec<_> = grouped_statements.iter().filter(|statement| matches!(**statement, Statement::Edge(_))).collect();
    let mut membership = BTreeMap::new();
    dot_membership(&grouped_statements, &[], &mut membership)?;
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
    Ok(())
}

#[test]
fn test_mermaid_nested_subgraphs_and_original_dependency_endpoints() -> Result<()> {
    let fixture = hierarchy_fixture()?;
    let system_under_test = Format::Mermaid.exporter();
    let mut flat_output = Vec::new();

    system_under_test.write(&fixture, &mut flat_output)?;
    let grouped_output = render_grouped(system_under_test.as_ref(), &fixture)?;
    let flat_output = String::from_utf8(flat_output)?;
    let flat_edges: Vec<_> = flat_output.lines().filter(|line| line.contains("-->")).collect();
    let grouped_edges: Vec<_> = grouped_output.lines().filter(|line| line.contains("-->")).collect();

    assert_eq!(flat_edges, grouped_edges, "grouping changed Mermaid dependency endpoints or weights");
    assert!(grouped_output.starts_with(concat!(
        "flowchart LR\n",
        "    n0[\".\"]\n",
        "    subgraph g1[\"src\"]\n",
        "    n1[\"src\"]\n",
        "    subgraph g2[\"src/api\"]\n",
        "    n2[\"src/api\"]\n",
        "    n3[\"src/api/http\"]\n",
        "    end\n",
        "    n4[\"src/storage\"]\n",
        "    end\n",
        "    n5[\"tests\"]\n",
    )), "Mermaid folder hierarchy is incorrect: {grouped_output}");
    Ok(())
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
fn test_missing_ancestors_are_containers_without_dependency_nodes(#[case] format: Format) -> Result<()> {
    let fixture = FolderGraph::new(
        ["src/deep/leaf", "tests"].map(String::from).map(FolderNode::new).into(),
        BTreeMap::from([(
            FolderEdge::new(FolderNode::new("src/deep/leaf".into()), FolderNode::new("tests".into()), DependencyKind::Production),
            DependencyCount::new(5),
        )]),
    )?;
    let system_under_test = format.exporter();

    let output = render_grouped(system_under_test.as_ref(), &fixture)?;

    match format {
        Format::Dot => {
            let statements = dot_statements(&output)?;
            let mut membership = BTreeMap::new();
            dot_membership(&statements, &[], &mut membership)?;

            assert_eq!(membership.len(), 2, "synthetic ancestors added dependency nodes");
            assert_eq!(membership.get("n0").context("leaf dependency node is missing")?.len(), 2, "DOT omitted a missing ancestor");
            assert!(membership.get("n1").context("tests dependency node is missing")?.is_empty(), "DOT retained the synthetic root");
            assert!(output.contains("label=\"src\""), "DOT omitted the src container label");
            assert!(output.contains("label=\"src/deep\""), "DOT omitted the deep container label");
        }
        Format::Mermaid => {
            let node_count = output.lines().filter(|line| line.trim_start().starts_with('n') && line.contains("[\"")).count();
            let group_count = output.lines().filter(|line| line.trim_start().starts_with("subgraph ")).count();

            assert_eq!(node_count, 2, "synthetic ancestors added dependency node declarations");
            assert_eq!(group_count, 2, "Mermaid retained the common root or omitted a missing ancestor");
            assert!(output.contains("[\"src\"]"), "Mermaid omitted the src container label");
            assert!(output.contains("[\"src/deep\"]"), "Mermaid omitted the deep container label");
        }
        Format::Graphml => bail!("GraphML is covered by its own grouping tests"),
    }
    Ok(())
}

#[rstest]
#[case(Format::Dot, "\"src/a\\\"<&\\\\#`\\n\\r\u{43f}\u{430}\u{43f}\u{43a}\u{430}\"")]
#[case(Format::Mermaid, "\"src/a#34;#60;#38;#92;#35;#96;#10;#13;\u{43f}\u{430}\u{43f}\u{43a}\u{430}\"")]
fn test_group_label_escaping(#[case] format: Format, #[case] escaped: &str) -> Result<()> {
    const PUNCTUATION_LABEL: &str = "src/a\"<&\\#`\n\r\u{43f}\u{430}\u{43f}\u{43a}\u{430}";
    let fixture = FolderGraph::new(
        [PUNCTUATION_LABEL.into(), format!("{PUNCTUATION_LABEL}/leaf"), "tests".into()].map(FolderNode::new).into(),
        BTreeMap::new(),
    )?;
    let system_under_test = format.exporter();

    let output = render_grouped(system_under_test.as_ref(), &fixture)?;

    assert_eq!(output.matches(escaped).count(), 2, "container and dependency label escaping differ: {output}");
    if matches!(format, Format::Dot) {
        dot_statements(&output)?;
    }
    Ok(())
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
fn test_common_path_labels_and_standalone_original_ancestors(#[case] format: Format) -> Result<()> {
    let fixture = FolderGraph::new(
        ["src", "src/engine", "src/engine/api", "src/engine/api/http", "src/engine/storage"]
            .map(String::from).map(FolderNode::new).into(),
        BTreeMap::from([
            (FolderEdge::new(FolderNode::new("src/engine".into()), FolderNode::new("src/engine/api".into()), DependencyKind::Production), DependencyCount::new(2)),
            (FolderEdge::new(FolderNode::new("src/engine/api".into()), FolderNode::new("src/engine/api/http".into()), DependencyKind::Production), DependencyCount::new(3)),
            (FolderEdge::new(FolderNode::new("src/engine/api/http".into()), FolderNode::new("src/engine/storage".into()), DependencyKind::Production), DependencyCount::new(5)),
        ]),
    )?;
    let system_under_test = format.exporter();
    let mut flat_output = Vec::new();

    system_under_test.write(&fixture, &mut flat_output)?;
    let flat_output = String::from_utf8(flat_output)?;
    let grouped_output = render_grouped(system_under_test.as_ref(), &fixture)?;

    match format {
        Format::Dot => {
            let flat_statements = dot_statements(&flat_output)?;
            let grouped_statements = dot_statements(&grouped_output)?;
            let flat_edges: Vec<_> = flat_statements.iter().filter(|statement| matches!(**statement, Statement::Edge(_))).collect();
            let grouped_edges: Vec<_> = grouped_statements.iter().filter(|statement| matches!(**statement, Statement::Edge(_))).collect();
            let mut membership = BTreeMap::new();
            dot_membership(&grouped_statements, &[], &mut membership)?;
            let expected = BTreeMap::from([
                ("n0".into(), vec![]), ("n1".into(), vec![]),
                ("n2".into(), vec!["cluster_n2".into()]),
                ("n3".into(), vec!["cluster_n2".into()]), ("n4".into(), vec![]),
            ]);

            assert_eq!(membership, expected, "Common ancestors were lost or retained as DOT groups");
            assert_eq!(flat_edges, grouped_edges, "Common-path removal changed DOT dependency endpoints or weights");
            assert!(grouped_output.contains("label=\"api\"") && grouped_output.contains("label=\"api/http\"") && grouped_output.contains("label=\"storage\""), "DOT descendant labels retain the common prefix");
            assert!(!grouped_output.contains("label=\"src/engine/api"), "DOT descendant labels retain the common prefix");
            assert!(grouped_output.contains("label=\"src\"") && grouped_output.contains("label=\"src/engine\""), "DOT detached ancestor labels changed");
        }
        Format::Mermaid => {
            let flat_edges: Vec<_> = flat_output.lines().filter(|line| line.contains("-->")).collect();
            let grouped_edges: Vec<_> = grouped_output.lines().filter(|line| line.contains("-->")).collect();

            assert_eq!(flat_edges, grouped_edges, "Common-path removal changed Mermaid dependency endpoints or weights");
            assert!(grouped_output.starts_with(concat!(
                "flowchart LR\n", "    n0[\"src\"]\n", "    n1[\"src/engine\"]\n",
                "    subgraph g2[\"api\"]\n", "    n2[\"api\"]\n", "    n3[\"api/http\"]\n",
                "    end\n", "    n4[\"storage\"]\n",
            )), "Common-path removal lost an original folder or retained Mermaid wrapper groups: {grouped_output}");
        }
        Format::Graphml => bail!("GraphML is covered by its own grouping tests"),
    }
    Ok(())
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
fn test_grouping_empty_graph_preserves_empty_output(#[case] format: Format) -> Result<()> {
    let fixture = FolderGraph::default();
    let system_under_test = format.exporter();
    let mut flat_output = Vec::new();

    system_under_test.write(&fixture, &mut flat_output)?;
    let grouped_output = render_grouped(system_under_test.as_ref(), &fixture)?;

    assert_eq!(grouped_output.as_bytes(), flat_output, "empty grouping created synthetic nodes or containers");
    Ok(())
}
