use std::collections::BTreeMap;

use anyhow::Context as _;
use anyhow::Error;
use anyhow::Result;
use anyhow::bail;
use graphviz_rust::dot_structures::EdgeTy;
use graphviz_rust::dot_structures::Graph;
use graphviz_rust::dot_structures::Id;
use graphviz_rust::dot_structures::Stmt as Statement;
use graphviz_rust::dot_structures::Vertex;
use pretty_assertions::assert_eq;

use super::Dot;
use crate::export::rendering::exporter::Exporter as _;
use crate::export::rendering::grouping_fixture::hierarchy_fixture;
use crate::export::rendering::grouping_fixture::render_grouped;
use crate::export::rendering::serialization_fixture::graph_fixture;
use crate::export::rendering::serialization_fixture::render;

pub(in crate::export) fn dot_statements(output: &str) -> Result<Vec<Statement>> {
    let Graph::DiGraph { stmts: statements, .. } = graphviz_rust::parse(output).map_err(Error::msg)? else {
        bail!("expected a directed DOT graph");
    };
    Ok(statements)
}

pub(in crate::export) fn dot_membership(
    root_statements: &[Statement],
    root_groups: &[String],
    membership: &mut BTreeMap<String, Vec<String>>,
) -> Result<()> {
    let mut work = vec![(root_statements, root_groups.to_vec())];
    while let Some((statements, groups)) = work.pop() {
        for statement in statements {
            match *statement {
                Statement::Node(ref node) => {
                    let Id::Plain(ref identifier) = node.id.0 else {
                        bail!("expected plain DOT node identifier");
                    };
                    let previous = membership.insert(identifier.clone(), groups.clone());
                    assert!(previous.is_none(), "DOT node was declared more than once");
                }
                Statement::Subgraph(ref group) => {
                    let Id::Plain(ref identifier) = group.id else {
                        bail!("expected plain DOT group identifier");
                    };
                    let mut ancestors = groups.clone();
                    ancestors.push(identifier.clone());
                    work.push((&group.stmts, ancestors));
                }
                Statement::Edge(_) => assert!(groups.is_empty(), "dependency edges must stay at graph scope"),
                Statement::Attribute(_) | Statement::GAttribute(_) => {}
            }
        }
    }
    Ok(())
}

#[test]
fn test_dot_labels_direction_weights_and_isolated_nodes() -> Result<()> {
    let fixture = graph_fixture()?;
    let system_under_test = Dot;

    let output = render(&system_under_test, &fixture)?;
    let Graph::DiGraph { strict, stmts: statements, .. } = graphviz_rust::parse(&output).map_err(Error::msg)? else {
        bail!("expected directed graph");
    };
    let nodes: Vec<_> = statements.iter().filter_map(|statement| {
        if let Statement::Node(ref node) = *statement { Some(node) } else { None }
    }).collect();
    let edges: Vec<_> = statements.iter().filter_map(|statement| {
        if let Statement::Edge(ref edge) = *statement { Some(edge) } else { None }
    }).collect();
    let first_node = nodes.first().context("first DOT node is missing")?;
    let isolated_node = nodes.last().context("isolated DOT node is missing")?;
    let edge = edges.first().context("DOT edge is missing")?;
    let EdgeTy::Pair(Vertex::N(ref source), Vertex::N(ref target)) = edge.ty else {
        bail!("expected a simple directed edge");
    };

    assert!(strict, "DOT graph must be strict");
    assert_eq!(nodes.len(), 3, "Expected three DOT nodes");
    assert_eq!(
        &first_node.attributes.first().context("DOT label is missing")?.1,
        &Id::Escaped("\"src/a\\\"<&\\\\#`\\n\\r\u{43f}\u{430}\u{43f}\u{43a}\u{430}\"".into()),
        "DOT punctuation label was not preserved or escaped correctly"
    );
    assert_eq!(
        &isolated_node.attributes.first().context("isolated DOT label is missing")?.1,
        &Id::Escaped("\"src/isolated\"".into()),
        "DOT isolated node label was not preserved"
    );
    assert_eq!(edges.len(), 1, "Expected one DOT edge");
    assert_eq!(&source.0, &Id::Plain("n0".into()), "DOT edge source is incorrect");
    assert_eq!(&target.0, &Id::Plain("n1".into()), "DOT edge target is incorrect");
    assert_eq!(
        &edge.attributes.first().context("DOT edge weight is missing")?.1,
        &Id::Escaped("\"7\"".into()),
        "DOT edge weight is incorrect"
    );
    Ok(())
}

#[test]
fn test_dot_nested_clusters_and_original_dependency_endpoints() -> Result<()> {
    let fixture = hierarchy_fixture()?;
    let system_under_test = Dot;
    let mut flat_output = Vec::new();

    system_under_test.write(&fixture, &mut flat_output)?;
    let grouped_output = render_grouped(&system_under_test, &fixture)?;
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
