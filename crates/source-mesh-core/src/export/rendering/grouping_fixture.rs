use std::collections::BTreeMap;

use anyhow::Result;

use super::exporter::Exporter;
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

pub(in crate::export) fn render_grouped(exporter: &dyn Exporter, graph: &FolderGraph) -> Result<String> {
    let mut output = Vec::new();
    exporter.write_grouped(graph, &mut output)?;
    Ok(String::from_utf8(output)?)
}
