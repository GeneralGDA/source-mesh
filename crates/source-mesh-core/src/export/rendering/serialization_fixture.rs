use std::collections::BTreeMap;

use anyhow::Result;

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

pub(in crate::export) fn cycle_graph_fixture() -> Result<FolderGraph> {
    FolderGraph::new(
        ["src/first/a", "src/first/b", "src/isolated", "src/second/a", "src/second/b", "src/third/self"]
            .map(String::from).map(FolderNode::new).into(),
        [
            ("src/first/a", "src/first/b", DependencyKind::Production, 2),
            ("src/first/a", "src/first/b", DependencyKind::Test, 3),
            ("src/first/b", "src/first/a", DependencyKind::Production, 4),
            ("src/first/b", "src/second/a", DependencyKind::Production, 5),
            ("src/first/b", "src/second/a", DependencyKind::Test, 9),
            ("src/second/a", "src/second/b", DependencyKind::Production, 6),
            ("src/second/b", "src/second/a", DependencyKind::Production, 7),
            ("src/third/self", "src/third/self", DependencyKind::Production, 8),
        ].into_iter().map(|(source, target, kind, weight)| (
            FolderEdge::new(FolderNode::new(source.into()), FolderNode::new(target.into()), kind),
            DependencyCount::new(weight),
        )).collect(),
    )
}
