use std::collections::BTreeMap;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::bail;
use graphviz_rust::dot_structures::Stmt as Statement;
use rstest::rstest;

use crate::export::Format;
use crate::export::dot::dot_membership;
use crate::export::dot::dot_statements;
use crate::export::rendering::grouping_fixture::render_grouped;
use crate::model::DependencyCount;
use crate::model::DependencyKind;
use crate::model::FolderEdge;
use crate::model::FolderGraph;
use crate::model::FolderNode;
use pretty_assertions::assert_eq;

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
fn test_missing_ancestor_containers(#[case] format: Format) -> Result<()> {
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
            assert_eq!(
                membership.get("n0").context("leaf dependency node is missing")?.len(),
                2,
                "DOT omitted a missing ancestor"
            );
            let test_groups = membership.get("n1").context("tests dependency node is missing")?;
            assert!(test_groups.is_empty(), "DOT retained the synthetic root");
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
    if matches!(format, Format::Dot) {
        dot_statements(&output)?;
    }

    assert_eq!(output.matches(escaped).count(), 2, "container and dependency label escaping differ: {output}");
    Ok(())
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
fn test_common_path_labels_and_standalone_original_ancestors(#[case] format: Format) -> Result<()> {
    let labels = ["src", "src/engine", "src/engine/api", "src/engine/api/http", "src/engine/storage"];
    let fixture = FolderGraph::new(
        labels.map(String::from).map(FolderNode::new).into(),
        BTreeMap::from([
            (
                FolderEdge::new(
                    FolderNode::new("src/engine".into()),
                    FolderNode::new("src/engine/api".into()),
                    DependencyKind::Production,
                ),
                DependencyCount::new(2),
            ),
            (
                FolderEdge::new(
                    FolderNode::new("src/engine/api".into()),
                    FolderNode::new("src/engine/api/http".into()),
                    DependencyKind::Production,
                ),
                DependencyCount::new(3),
            ),
            (
                FolderEdge::new(
                    FolderNode::new("src/engine/api/http".into()),
                    FolderNode::new("src/engine/storage".into()),
                    DependencyKind::Production,
                ),
                DependencyCount::new(5),
            ),
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
            let flat_edges = flat_statements.iter().filter(|statement| matches!(**statement, Statement::Edge(_)));
            let flat_edges: Vec<_> = flat_edges.collect();
            let grouped_edges = grouped_statements.iter().filter(|statement| matches!(**statement, Statement::Edge(_)));
            let grouped_edges: Vec<_> = grouped_edges.collect();
            let mut membership = BTreeMap::new();
            dot_membership(&grouped_statements, &[], &mut membership)?;
            let expected = BTreeMap::from([
                ("n0".into(), vec![]),
                ("n1".into(), vec![]),
                ("n2".into(), vec!["cluster_n2".into()]),
                ("n3".into(), vec!["cluster_n2".into()]),
                ("n4".into(), vec![]),
            ]);

            assert_eq!(membership, expected, "Common ancestors were lost or retained as DOT groups");
            assert_eq!(flat_edges, grouped_edges, "Common-path removal changed DOT dependency endpoints or weights");
            let has_api_labels = grouped_output.contains("label=\"api\"") && grouped_output.contains("label=\"api/http\"");
            assert!(
                has_api_labels && grouped_output.contains("label=\"storage\""),
                "DOT descendant labels retain the common prefix"
            );
            assert!(!grouped_output.contains("label=\"src/engine/api"), "DOT descendant labels retain the common prefix");
            let has_ancestor_labels = grouped_output.contains("label=\"src\"") && grouped_output.contains("label=\"src/engine\"");
            assert!(has_ancestor_labels, "DOT detached ancestor labels changed");
        }
        Format::Mermaid => {
            let flat_edges: Vec<_> = flat_output.lines().filter(|line| line.contains("-->")).collect();
            let grouped_edges: Vec<_> = grouped_output.lines().filter(|line| line.contains("-->")).collect();

            assert_eq!(flat_edges, grouped_edges, "Common-path removal changed Mermaid dependency endpoints or weights");
            assert!(
                grouped_output.starts_with(concat!(
                    "flowchart LR\n",
                    "    n0[\"src\"]\n",
                    "    n1[\"src/engine\"]\n",
                    "    subgraph g2[\"api\"]\n",
                    "    n2[\"api\"]\n",
                    "    n3[\"api/http\"]\n",
                    "    end\n",
                    "    n4[\"storage\"]\n",
                )),
                "Common-path removal lost an original folder or retained Mermaid wrapper groups: {grouped_output}"
            );
        }
        Format::Graphml => bail!("GraphML is covered by its own grouping tests"),
    }
    Ok(())
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
fn test_empty_graph_grouping(#[case] format: Format) -> Result<()> {
    let fixture = FolderGraph::default();
    let system_under_test = format.exporter();
    let mut flat_output = Vec::new();

    system_under_test.write(&fixture, &mut flat_output)?;
    let grouped_output = render_grouped(system_under_test.as_ref(), &fixture)?;

    assert_eq!(grouped_output.as_bytes(), flat_output, "empty grouping created synthetic nodes or containers");
    Ok(())
}
