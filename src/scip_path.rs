use std::path::Component;
use std::path::PathBuf;

use anyhow::Result;
use anyhow::ensure;

use crate::model::FileNode;

pub(crate) fn source_path(value: &str) -> Result<FileNode> {
    let normalized = value.replace('\\', "/");
    let path = PathBuf::from(&normalized);
    ensure!(
        !normalized.is_empty()
            && !normalized.contains('\0')
            && !normalized.starts_with('/')
            && !normalized.ends_with('/')
            && !normalized
                .split('/')
                .next()
                .is_some_and(|part| part.contains(':'))
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
            && path
                .components()
                .any(|part| matches!(part, Component::Normal(_))),
        "SCIP document path must be a nonempty relative file path without '..': {value:?}"
    );
    FileNode::new(path
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name),
            Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir => None,
        })
        .collect())
}
