use std::collections::BTreeMap;

use anyhow::Result;

use super::exporter::Exporter;
use crate::model::DependencyCount;
use crate::model::DependencyKind;
use crate::model::FolderEdge;
use crate::model::FolderGraph;
use crate::model::FolderNode;

pub(in crate::export) fn graph_fixture() -> Result<FolderGraph> {
    const PUNCTUATION_LABEL: &str = "src/a\"<&\\#`\n\r\u{43f}\u{430}\u{43f}\u{43a}\u{430}";
    FolderGraph::new(
        [PUNCTUATION_LABEL, "src/b", "src/isolated"].map(String::from).map(FolderNode::new).into(),
        BTreeMap::from([(
            FolderEdge::new(
                FolderNode::new(PUNCTUATION_LABEL.into()), FolderNode::new("src/b".into()), DependencyKind::Production,
            ),
            DependencyCount::new(7),
        )]),
    )
}

pub(in crate::export) fn parallel_dependency_fixture() -> Result<FolderGraph> {
    let source = FolderNode::new("src/api/http".into());
    let target = FolderNode::new("src/storage".into());
    FolderGraph::new(
        [source.clone(), target.clone()].into(),
        BTreeMap::from([
            (FolderEdge::new(source.clone(), target.clone(), DependencyKind::Production), DependencyCount::new(2)),
            (FolderEdge::new(source, target, DependencyKind::Test), DependencyCount::new(3)),
        ]),
    )
}

pub(in crate::export) fn render(exporter: &dyn Exporter, graph: &FolderGraph) -> Result<String> {
    let mut output = Vec::new();
    exporter.write(graph, &mut output)?;
    Ok(String::from_utf8(output)?)
}
