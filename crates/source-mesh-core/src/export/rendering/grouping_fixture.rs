use std::collections::BTreeMap;

use anyhow::Error;
use anyhow::Result;
use anyhow::bail;
use claims::assert_some;
use graphviz_rust::dot_structures::Graph;
use graphviz_rust::dot_structures::Id;
use graphviz_rust::dot_structures::Stmt as Statement;

use crate::model::DependencyCount;
use crate::model::DependencyKind;
use crate::model::FolderEdge;
use crate::model::FolderGraph;
use crate::model::FolderNode;

pub(in crate::export) fn hierarchy_fixture() -> Result<FolderGraph> {
    FolderGraph::new(
        [".", "src", "src/api", "src/api/http", "src/storage", "tests"].map(String::from).map(FolderNode::new).into(),
        BTreeMap::from([
            (
                FolderEdge::new(FolderNode::new(".".into()), FolderNode::new("src".into()), DependencyKind::Production),
                DependencyCount::new(2),
            ),
            (
                FolderEdge::new(FolderNode::new("src".into()), FolderNode::new("src/api".into()), DependencyKind::Production),
                DependencyCount::new(3),
            ),
            (
                FolderEdge::new(FolderNode::new("src/api".into()), FolderNode::new("src/api".into()), DependencyKind::Production),
                DependencyCount::new(11),
            ),
            (
                FolderEdge::new(
                    FolderNode::new("src/api/http".into()),
                    FolderNode::new("src/storage".into()),
                    DependencyKind::Production,
                ),
                DependencyCount::new(7),
            ),
        ]),
    )
}

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
) {
    let mut work = vec![(root_statements, root_groups.to_vec())];
    while let Some((statements, groups)) = work.pop() {
        for statement in statements {
            match *statement {
                Statement::Node(ref node) => {
                    let identifier = if let Id::Plain(ref identifier) = node.id.0 { Some(identifier) } else { None };
                    let identifier = assert_some!(identifier, "DOT dependency nodes must have plain identifiers");
                    let previous = membership.insert(identifier.clone(), groups.clone());
                    assert!(previous.is_none(), "DOT node was declared more than once");
                }
                Statement::Subgraph(ref group) => {
                    let identifier = if let Id::Plain(ref identifier) = group.id { Some(identifier) } else { None };
                    let identifier = assert_some!(identifier, "DOT folder groups must have plain identifiers");
                    let mut ancestors = groups.clone();
                    ancestors.push(identifier.clone());
                    work.push((&group.stmts, ancestors));
                }
                Statement::Edge(_) => assert!(groups.is_empty(), "dependency edges must stay at graph scope"),
                Statement::Attribute(_) | Statement::GAttribute(_) => {}
            }
        }
    }
}
