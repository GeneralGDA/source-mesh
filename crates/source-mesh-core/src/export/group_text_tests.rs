use std::collections::BTreeMap;

use claims::assert_ok;
use claims::assert_some;
use graphviz_rust::dot_structures::Stmt as Statement;
use rstest::rstest;

use crate::export::Format;
use crate::export::rendering::grouping_fixture::dot_membership;
use crate::export::rendering::grouping_fixture::dot_statements;
use crate::export::rendering::render_grouped;
use crate::model::DependencyCount;
use crate::model::DependencyKind;
use crate::model::FolderEdge;
use crate::model::FolderGraph;
use crate::model::FolderNode;
use pretty_assertions::assert_eq;

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
fn test_missing_ancestor_containers(#[case] format: Format) {
    let fixture = FolderGraph::new(
        ["src/deep/leaf", "tests"].map(String::from).map(FolderNode::new).into(),
        BTreeMap::from([(
            FolderEdge::new(FolderNode::new("src/deep/leaf".into()), FolderNode::new("tests".into()), DependencyKind::Production),
            DependencyCount::new(5),
        )]),
    );
    let fixture = assert_ok!(fixture, "Missing-ancestor fixture must form a valid folder graph");
    let system_under_test = format.exporter();

    let output = render_grouped(system_under_test.as_ref(), &fixture);
    let output = assert_ok!(output, "Grouped export must render synthetic ancestor containers");

    match format {
        Format::Dot => {
            let statements = dot_statements(&output);
            let statements = assert_ok!(statements, "Grouped DOT output must parse as a directed graph");
            let mut membership = BTreeMap::new();
            dot_membership(&statements, &[], &mut membership);

            assert_eq!(membership.len(), 2, "synthetic ancestors added dependency nodes");
            let leaf_groups = membership.get("n0");
            let leaf_groups = assert_some!(leaf_groups, "DOT membership must contain the leaf dependency node");
            assert_eq!(leaf_groups.len(), 2, "DOT omitted a missing ancestor");
            let test_groups = membership.get("n1");
            let test_groups = assert_some!(test_groups, "DOT membership must contain the tests dependency node");
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
        Format::Graphml => unreachable!("Text-grouping cases must use DOT or Mermaid; GraphML has its own suite"),
    }
}

#[rstest]
#[case(Format::Dot, "\"src/a\\\"<&\\\\#`\\n\\r\u{43f}\u{430}\u{43f}\u{43a}\u{430}\"")]
#[case(Format::Mermaid, "\"src/a#34;#60;#38;#92;#35;#96;#10;#13;\u{43f}\u{430}\u{43f}\u{43a}\u{430}\"")]
fn test_group_label_escaping(#[case] format: Format, #[case] escaped: &str) {
    const LABEL_PUNCTUATION: &str = "src/a\"<&\\#`\n\r\u{43f}\u{430}\u{43f}\u{43a}\u{430}";
    let fixture = FolderGraph::new(
        [LABEL_PUNCTUATION.into(), format!("{LABEL_PUNCTUATION}/leaf"), "tests".into()].map(FolderNode::new).into(),
        BTreeMap::new(),
    );
    let fixture = assert_ok!(fixture, "Punctuation in group labels must form a valid folder graph");
    let system_under_test = format.exporter();

    let output = render_grouped(system_under_test.as_ref(), &fixture);
    let output = assert_ok!(output, "Grouped export must render escaped punctuation labels");
    if matches!(format, Format::Dot) {
        let statements = dot_statements(&output);
        assert_ok!(statements, "Escaped group labels must remain parseable DOT");
    }

    assert_eq!(output.matches(escaped).count(), 2, "container and dependency label escaping differ: {output}");
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
fn test_common_path_labels_and_standalone_original_ancestors(#[case] format: Format) {
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
    );
    let fixture = assert_ok!(fixture, "Common-path fixture must form a valid folder graph");
    let system_under_test = format.exporter();
    let mut flat_output = Vec::new();

    let result = system_under_test.write(&fixture, &mut flat_output);
    assert_ok!(result, "Flat common-path graph must serialize successfully");
    let flat_output = String::from_utf8(flat_output);
    let flat_output = assert_ok!(flat_output, "Flat common-path output must be UTF-8");
    let grouped_output = render_grouped(system_under_test.as_ref(), &fixture);
    let grouped_output = assert_ok!(grouped_output, "Grouped common-path graph must render successfully");

    match format {
        Format::Dot => {
            let flat_statements = dot_statements(&flat_output);
            let flat_statements = assert_ok!(flat_statements, "Flat common-path DOT must parse as a directed graph");
            let grouped_statements = dot_statements(&grouped_output);
            let grouped_statements = assert_ok!(grouped_statements, "Grouped common-path DOT must parse as a directed graph");
            let flat_edges = flat_statements.iter().filter(|statement| matches!(**statement, Statement::Edge(_)));
            let flat_edges: Vec<_> = flat_edges.collect();
            let grouped_edges = grouped_statements.iter().filter(|statement| matches!(**statement, Statement::Edge(_)));
            let grouped_edges: Vec<_> = grouped_edges.collect();
            let mut membership = BTreeMap::new();
            dot_membership(&grouped_statements, &[], &mut membership);
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
        Format::Graphml => unreachable!("Text-grouping cases must use DOT or Mermaid; GraphML has its own suite"),
    }
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
fn test_empty_graph_grouping(#[case] format: Format) {
    let fixture = FolderGraph::default();
    let system_under_test = format.exporter();
    let mut flat_output = Vec::new();

    let result = system_under_test.write(&fixture, &mut flat_output);
    assert_ok!(result, "Flat empty graph must serialize successfully");
    let grouped_output = render_grouped(system_under_test.as_ref(), &fixture);
    let grouped_output = assert_ok!(grouped_output, "Grouped empty graph must render successfully");

    assert_eq!(grouped_output.as_bytes(), flat_output, "empty grouping created synthetic nodes or containers");
}
