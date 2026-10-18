use std::path::Path;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use path_clean::PathClean as _;
use typed_path::Utf8WindowsPath;

use crate::SourcePath;
use crate::model::FileNode;

pub(crate) fn source_path(relative_path: &str) -> Result<FileNode> {
    let path = Utf8WindowsPath::new(relative_path);
    ensure!(
        !path.has_root() && !path.components().has_prefix(),
        "SCIP document path must be relative to the project root: {relative_path:?}"
    );
    let path = Path::new(path.with_unix_encoding().as_str()).clean();
    SourcePath::new(path).with_context(|| format!("Invalid SCIP document path: {relative_path:?}")).map(FileNode::new)
}
