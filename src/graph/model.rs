use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use derive_new::new;
use getset::CopyGetters;
use getset::Getters;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Getters)]
#[getset(get = "pub")]
pub struct FileNode {
    path: PathBuf,
}

impl FileNode {
    pub fn new(path: PathBuf) -> Result<Self> {
        ensure!(
            path.components().all(|component| matches!(component, Component::Normal(_)))
                && path.file_name().is_some(),
            "File graph requires nonempty relative source paths with normal path components"
        );
        Ok(Self { path })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Getters, new)]
#[getset(get = "pub")]
pub struct FolderNode {
    label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DependencyKind {
    Production,
    Test,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, CopyGetters, Getters, new)]
pub struct Edge<Node> {
    #[getset(get = "pub")]
    source: Node,
    #[getset(get = "pub")]
    target: Node,
    #[getset(get_copy = "pub")]
    kind: DependencyKind,
}

pub type FileEdge = Edge<FileNode>;
pub type FolderEdge = Edge<FolderNode>;

/** Counts distinct directed file pairs of the same dependency kind.

```compile_fail
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use source_mesh::model::DependencyKind;
use source_mesh::model::FolderEdge;
use source_mesh::model::FolderGraph;
use source_mesh::model::FolderNode;

let folder = FolderNode::new("src".into());
let edge = FolderEdge::new(folder.clone(), folder.clone(), DependencyKind::Production);
let depth = 2_usize;
let graph = FolderGraph::new(BTreeSet::from([folder]), BTreeMap::from([(edge, depth)]));
```
*/
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, CopyGetters, new)]
#[getset(get_copy = "pub")]
pub struct DependencyCount {
    value: usize,
}

impl DependencyCount {
    fn increment(&mut self) -> Result<()> {
        self.value = self.value.checked_add(1).context("Dependency count overflow")?;
        Ok(())
    }
}

#[derive(Default, PartialEq, Getters)]
#[getset(get = "pub")]
pub struct FolderGraph {
    nodes: BTreeSet<FolderNode>,
    edges: BTreeMap<FolderEdge, DependencyCount>,
}

impl FolderGraph {
    pub fn new(nodes: BTreeSet<FolderNode>, edges: BTreeMap<FolderEdge, DependencyCount>) -> Result<Self> {
        ensure!(
            edges.keys().all(|edge| nodes.contains(edge.source()) && nodes.contains(edge.target())),
            "Folder graph has an unknown dependency endpoint"
        );
        Ok(Self { nodes, edges })
    }
}

/**

```compile_fail
use std::collections::BTreeSet;
use std::path::PathBuf;
use source_mesh::model::FileGraph;

let files = BTreeSet::from([PathBuf::from("lib.rs")]);
let graph = FileGraph::new(files, BTreeSet::new());
```
*/
#[derive(PartialEq, Getters)]
#[getset(get = "pub")]
pub struct FileGraph {
    files: BTreeSet<FileNode>,
    edges: BTreeSet<FileEdge>,
}

impl FileGraph {
    pub fn new(files: BTreeSet<FileNode>, edges: BTreeSet<FileEdge>) -> Result<Self> {
        ensure!(
            edges.iter().all(|edge| files.contains(edge.source()) && files.contains(edge.target())),
            "File graph has an unknown dependency endpoint"
        );
        Ok(Self { files, edges })
    }

    #[must_use]
    pub fn without_test_dependencies(mut self) -> Self {
        self.edges.retain(|edge| edge.kind() == DependencyKind::Production);
        self
    }

    pub fn within_root(mut self, root: &Path) -> Result<Self> {
        let root = relative_directory_root(root)?;
        self.files.retain(|file| file.path().parent().is_some_and(|parent| parent.starts_with(&root)));
        self.edges.retain(|edge| self.files.contains(edge.source()) && self.files.contains(edge.target()));
        Ok(self)
    }

    pub fn folders(
        &self,
        root: &Path,
        depth: Option<usize>,
        include_self: bool,
    ) -> Result<FolderGraph> {
        let root = relative_directory_root(root)?;
        let mut folder_graph = FolderGraph::default();
        let mut file_folders = BTreeMap::new();
        for file in &self.files {
            let parent = file.path().parent().context("Indexed source file has no parent")?;
            let Ok(relative) = parent.strip_prefix(&root) else {
                continue;
            };
            let parts = relative
                .components()
                .take(depth.unwrap_or(usize::MAX))
                .map(|part| {
                    part.as_os_str()
                        .to_str()
                        .context("Folder path is not UTF-8")
                })
                .collect::<Result<Vec<_>>>()?;
            let label = if parts.is_empty() {
                ".".into()
            } else {
                parts.join("/")
            };
            let folder = FolderNode::new(label);
            folder_graph.nodes.insert(folder.clone());
            file_folders.insert(file, folder);
        }
        for edge in &self.edges {
            let (source, target) = (edge.source(), edge.target());
            if source == target {
                continue;
            }
            if let (Some(source_folder), Some(target_folder)) = (file_folders.get(source), file_folders.get(target))
                && (include_self || source_folder != target_folder)
            {
                folder_graph.edges.entry(FolderEdge::new(source_folder.clone(), target_folder.clone(), edge.kind()))
                    .or_default().increment()?;
            }
        }
        Ok(folder_graph)
    }
}

fn relative_directory_root(root: &Path) -> Result<PathBuf> {
    ensure!(root.components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir)),
        "--root must be a relative directory inside the indexed project (no '..')");
    Ok(root.components().filter(|component| !matches!(component, Component::CurDir)).collect())
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Tests use assertions for expectations while returning Result for fallible setup."
)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::path::PathBuf;

    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::DependencyCount;
    use super::DependencyKind;
    use super::FileEdge;
    use super::FileGraph;
    use super::FileNode;
    use super::FolderEdge;
    use super::FolderGraph;
    use super::FolderNode;

    #[rstest]
    #[case(0, 1)]
    #[case(7, 8)]
    #[case(usize::MAX - 1, usize::MAX)]
    fn test_dependency_count_increment(#[case] initial: usize, #[case] expected: usize) -> Result<()> {
        let mut system_under_test = DependencyCount::new(initial);

        system_under_test.increment()?;

        assert_eq!(expected, system_under_test.value(), "Dependency count did not increase by one");
        Ok(())
    }

    #[test]
    fn test_dependency_count_overflow() -> Result<()> {
        let mut system_under_test = DependencyCount::new(usize::MAX);

        let result = system_under_test.increment();

        assert!(result.is_err(), "Dependency count overflow was accepted");
        assert_eq!(usize::MAX, system_under_test.value(), "Overflow changed the dependency count");
        Ok(())
    }

    fn file_graph_fixture() -> Result<(BTreeSet<FileNode>, BTreeSet<FileEdge>)> {
        let source_directory = Path::new("src");
        let service_module = FileNode::new(source_directory.join("api").join("service.rs"))?;
        let http_module = FileNode::new(source_directory.join("api").join("http.rs"))?;
        let routes_module = FileNode::new(source_directory.join("api").join("v1").join("routes.rs"))?;
        let request_module = FileNode::new(source_directory.join("domain").join("request.rs"))?;
        let rules_module = FileNode::new(source_directory.join("domain").join("rules.rs"))?;
        let database_module = FileNode::new(source_directory.join("storage").join("db.rs"))?;
        let calls_module = FileNode::new(source_directory.join("body_only").join("calls.rs"))?;
        let files = BTreeSet::from([
            FileNode::new(source_directory.join("lib.rs"))?,
            FileNode::new(source_directory.join("api").join("mod.rs"))?,
            service_module.clone(),
            http_module.clone(),
            FileNode::new(source_directory.join("api").join("v1").join("mod.rs"))?,
            routes_module.clone(),
            FileNode::new(source_directory.join("domain").join("mod.rs"))?,
            request_module.clone(),
            rules_module.clone(),
            database_module.clone(),
            FileNode::new(source_directory.join("body_only").join("mod.rs"))?,
            calls_module.clone(),
            FileNode::new(source_directory.join("isolated").join("mod.rs"))?,
            FileNode::new(source_directory.join("isolated").join("sample.rs"))?,
        ]);
        let dependencies = BTreeSet::from([
            FileEdge::new(service_module.clone(), http_module.clone(), DependencyKind::Production),
            FileEdge::new(service_module, routes_module.clone(), DependencyKind::Production),
            FileEdge::new(http_module.clone(), request_module.clone(), DependencyKind::Production),
            FileEdge::new(http_module, database_module.clone(), DependencyKind::Production),
            FileEdge::new(routes_module.clone(), request_module.clone(), DependencyKind::Production),
            FileEdge::new(routes_module, database_module.clone(), DependencyKind::Production),
            FileEdge::new(request_module, rules_module.clone(), DependencyKind::Production),
            FileEdge::new(rules_module, database_module.clone(), DependencyKind::Production),
            FileEdge::new(calls_module, database_module, DependencyKind::Production),
        ]);
        Ok((files, dependencies))
    }

    #[must_use]
    fn folder_edges(values: &[(&str, &str, usize)], kind: DependencyKind) -> BTreeMap<FolderEdge, DependencyCount> {
        values
            .iter()
            .map(|&(source, destination, weight)| (
                FolderEdge::new(FolderNode::new(source.into()), FolderNode::new(destination.into()), kind),
                DependencyCount::new(weight),
            ))
            .collect()
    }

    #[must_use]
    fn folder_nodes(values: &[&str]) -> BTreeSet<FolderNode> {
        values.iter().map(|&label| FolderNode::new(label.into())).collect()
    }

    fn mixed_dependency_fixture() -> Result<(BTreeSet<FileNode>, BTreeSet<FileEdge>)> {
        let source_directory = Path::new("src");
        let http_module = FileNode::new(source_directory.join("api").join("http.rs"))?;
        let routes_module = FileNode::new(source_directory.join("api").join("v1").join("routes.rs"))?;
        let request_module = FileNode::new(source_directory.join("domain").join("request.rs"))?;
        let checks_module = FileNode::new(source_directory.join("tests").join("checks.rs"))?;
        let files = BTreeSet::from([
            http_module.clone(), routes_module.clone(), request_module.clone(), checks_module.clone(),
            FileNode::new(source_directory.join("isolated").join("sample.rs"))?,
        ]);
        let dependencies = BTreeSet::from([
            FileEdge::new(http_module.clone(), request_module.clone(), DependencyKind::Production),
            FileEdge::new(routes_module.clone(), request_module.clone(), DependencyKind::Production),
            FileEdge::new(http_module.clone(), request_module.clone(), DependencyKind::Test),
            FileEdge::new(http_module.clone(), request_module.clone(), DependencyKind::Test),
            FileEdge::new(routes_module, request_module.clone(), DependencyKind::Test),
            FileEdge::new(request_module.clone(), http_module, DependencyKind::Test),
            FileEdge::new(checks_module, request_module, DependencyKind::Test),
        ]);
        Ok((files, dependencies))
    }

    #[test]
    fn test_physical_directories_and_isolated_files() -> Result<()> {
        let (files, dependencies) = file_graph_fixture()?;
        let system_under_test = FileGraph::new(files, dependencies)?;
        let expected_nodes = folder_nodes(&[
            ".", "api", "api/v1", "body_only", "domain", "isolated", "storage",
        ]);
        let expected_edges = folder_edges(&[
            ("api", "api/v1", 1),
            ("api", "domain", 1),
            ("api", "storage", 1),
            ("api/v1", "domain", 1),
            ("api/v1", "storage", 1),
            ("body_only", "storage", 1),
            ("domain", "storage", 1),
        ], DependencyKind::Production);

        let graph = system_under_test.folders(Path::new("src"), None, false)?;

        assert_eq!(&expected_nodes, graph.nodes(), "Physical directory nodes differ");
        assert_eq!(&expected_edges, graph.edges(), "Physical directory dependencies differ");
        assert!(!graph.nodes().contains(&FolderNode::new("persistence".into())), "A semantic module name was substituted for the physical folder");
        Ok(())
    }

    #[rstest]
    #[case(None)]
    #[case(Some(2))]
    #[case(Some(10))]
    fn test_depth_beyond_existing_folders(#[case] depth: Option<usize>) -> Result<()> {
        let (files, dependencies) = file_graph_fixture()?;
        let system_under_test = FileGraph::new(files, dependencies)?;
        let complete_graph = system_under_test.folders(Path::new("src"), None, false)?;

        let graph = system_under_test.folders(Path::new("src"), depth, false)?;

        assert_eq!(complete_graph.nodes(), graph.nodes(), "Depth {depth:?} changed nodes in a graph shallower than the requested limit");
        assert_eq!(complete_graph.edges(), graph.edges(), "Depth {depth:?} changed dependencies in a graph shallower than the requested limit");
        Ok(())
    }

    #[rstest]
    #[case(0, &["."], &[])]
    #[case(1, &[".", "api", "body_only", "domain", "isolated", "storage"], &[
        ("api", "domain", 2), ("api", "storage", 2),
        ("body_only", "storage", 1), ("domain", "storage", 1),
    ])]
    fn test_depth_grouping_and_distinct_file_pair_weights(
        #[case] depth: usize,
        #[case] expected_nodes: &[&str],
        #[case] expected_edges: &[(&str, &str, usize)],
    ) -> Result<()> {
        let (files, dependencies) = file_graph_fixture()?;
        let system_under_test = FileGraph::new(files, dependencies)?;

        let graph = system_under_test.folders(Path::new("src"), Some(depth), false)?;

        assert_eq!(&folder_nodes(expected_nodes), graph.nodes(), "Depth {depth} grouped nodes incorrectly");
        assert_eq!(&folder_edges(expected_edges, DependencyKind::Production), graph.edges(), "Depth {depth} grouped dependencies incorrectly");
        Ok(())
    }

    #[rstest]
    #[case(None, &[
        ("api", "api", 1), ("api", "api/v1", 1),
        ("api", "domain", 1), ("api", "storage", 1),
        ("api/v1", "domain", 1), ("api/v1", "storage", 1),
        ("body_only", "storage", 1), ("domain", "domain", 1),
        ("domain", "storage", 1),
    ])]
    #[case(Some(0), &[(".", ".", 9)])]
    #[case(Some(1), &[
        ("api", "api", 2), ("api", "domain", 2),
        ("api", "storage", 2), ("body_only", "storage", 1),
        ("domain", "domain", 1), ("domain", "storage", 1),
    ])]
    fn test_self_edges_between_distinct_files(
        #[case] depth: Option<usize>,
        #[case] expected_edges: &[(&str, &str, usize)],
    ) -> Result<()> {
        let (files, mut dependencies) = file_graph_fixture()?;
        let request_module = FileNode::new(Path::new("src").join("domain").join("request.rs"))?;
        dependencies.insert(FileEdge::new(request_module.clone(), request_module, DependencyKind::Production));
        let system_under_test = FileGraph::new(files, dependencies)?;

        let graph = system_under_test.folders(Path::new("src"), depth, true)?;

        assert_eq!(&folder_edges(expected_edges, DependencyKind::Production), graph.edges(), "Self-edge handling at depth {depth:?} differs");
        Ok(())
    }

    #[test]
    fn test_scope_boundaries_use_path_components() -> Result<()> {
        let (mut files, mut dependencies) = file_graph_fixture()?;
        let outside_module = FileNode::new(Path::new("src2").join("outside.rs"))?;
        files.insert(outside_module.clone());
        dependencies.insert(FileEdge::new(
            outside_module, FileNode::new(Path::new("src").join("api").join("service.rs"))?, DependencyKind::Production,
        ));
        let system_under_test = FileGraph::new(files, dependencies)?;

        let api_graph = system_under_test.folders(&Path::new("src").join("api"), None, false)?;
        let source_graph = system_under_test.folders(Path::new("src"), None, false)?;

        assert_eq!(&folder_nodes(&[".", "v1"]), api_graph.nodes(), "API scope contains unexpected folders");
        assert_eq!(&folder_edges(&[(".", "v1", 1)], DependencyKind::Production), api_graph.edges(), "API scope contains unexpected dependencies");
        assert!(!source_graph.nodes().contains(&FolderNode::new("src2".into())), "Scope included a directory with a shared prefix outside the source tree");
        assert_eq!(7, source_graph.edges().len(), "Scope changed the seven dependencies internal to the source tree");
        Ok(())
    }

    #[rstest]
    #[case(1, &["src"], &[])]
    #[case(2, &["src", "src/api", "src/body_only", "src/domain", "src/isolated", "src/storage"], &[
        ("src/api", "src/domain", 2), ("src/api", "src/storage", 2),
        ("src/body_only", "src/storage", 1), ("src/domain", "src/storage", 1),
    ])]
    fn test_default_depth_origin(
        #[case] depth: usize,
        #[case] expected_nodes: &[&str],
        #[case] expected_edges: &[(&str, &str, usize)],
    ) -> Result<()> {
        let (files, dependencies) = file_graph_fixture()?;
        let system_under_test = FileGraph::new(files, dependencies)?;

        let graph = system_under_test.folders(Path::new(""), Some(depth), false)?;

        assert_eq!(&folder_nodes(expected_nodes), graph.nodes(), "Default root produced incorrect nodes at depth {depth}");
        assert_eq!(&folder_edges(expected_edges, DependencyKind::Production), graph.edges(), "Default root produced incorrect dependencies at depth {depth}");
        Ok(())
    }

    #[test]
    fn test_isolated_folder_scope() -> Result<()> {
        let (files, dependencies) = file_graph_fixture()?;
        let system_under_test = FileGraph::new(files, dependencies)?;

        let graph = system_under_test.folders(&Path::new("src").join("isolated"), None, true)?;

        assert_eq!(&folder_nodes(&["."]), graph.nodes(), "Isolated scope contains unexpected folders");
        assert!(graph.edges().is_empty(), "An isolated folder acquired dependencies");
        Ok(())
    }

    #[rstest]
    #[case(None, &[("api", "domain", 1), ("api/v1", "domain", 1)], &[
        ("api", "domain", 1), ("api/v1", "domain", 1), ("domain", "api", 1), ("tests", "domain", 1),
    ])]
    #[case(Some(1), &[("api", "domain", 2)], &[
        ("api", "domain", 2), ("domain", "api", 1), ("tests", "domain", 1),
    ])]
    #[case(Some(0), &[(".", ".", 2)], &[(".", ".", 4)])]
    fn test_dependency_kinds_deduplication_and_depth_aggregation(
        #[case] depth: Option<usize>,
        #[case] expected_production: &[(&str, &str, usize)],
        #[case] expected_tests: &[(&str, &str, usize)],
    ) -> Result<()> {
        let (files, dependencies) = mixed_dependency_fixture()?;
        let system_under_test = FileGraph::new(files, dependencies)?;
        let mut expected_edges = folder_edges(expected_production, DependencyKind::Production);
        expected_edges.extend(folder_edges(expected_tests, DependencyKind::Test));

        let graph = system_under_test.folders(Path::new("src"), depth, true)?;

        assert_eq!(6, system_under_test.edges().len(), "Duplicate occurrences were retained or distinct dependency kinds were merged");
        assert_eq!(&expected_edges, graph.edges(), "Depth {depth:?} changed dependency kinds or distinct file-pair counts");
        Ok(())
    }

    #[test]
    fn test_dependency_kind_filtering() -> Result<()> {
        let (files, dependencies) = mixed_dependency_fixture()?;
        let expected_files = files.clone();
        let system_under_test = FileGraph::new(files, dependencies)?;
        let expected_edges = folder_edges(&[("api", "domain", 1), ("api/v1", "domain", 1)], DependencyKind::Production);

        let graph = system_under_test.without_test_dependencies();
        let folders = graph.folders(Path::new("src"), None, false)?;

        assert_eq!(&expected_files, graph.files(), "Excluding test dependencies removed source files");
        assert_eq!(2, graph.edges().len(), "Excluding test dependencies lost production edges or retained test edges");
        assert_eq!(&expected_edges, folders.edges(), "Excluding test dependencies changed production dependencies or counts");
        assert_eq!(&folder_nodes(&["api", "api/v1", "domain", "isolated", "tests"]), folders.nodes(),
            "Excluding test dependencies removed isolated or test-only source folders");
        Ok(())
    }

    #[rstest]
    #[case("")]
    #[case(".")]
    #[case("..")]
    #[case("../outside.rs")]
    #[case("./source.rs")]
    #[case("/absolute.rs")]
    fn test_file_node_source_path_preconditions(#[case] source_path: &str) {
        let path = PathBuf::from(source_path);

        let system_under_test = FileNode::new(path);

        assert!(system_under_test.is_err(), "Invalid file source path was accepted: {source_path}");
    }

    #[rstest]
    #[case("..")]
    #[case("../src")]
    #[case("/src")]
    fn test_folder_scope_preconditions(#[case] root: &str) -> Result<()> {
        let (files, dependencies) = file_graph_fixture()?;
        let system_under_test = FileGraph::new(files, dependencies)?;

        let graph = system_under_test.folders(Path::new(root), None, false);

        assert!(graph.is_err(), "Invalid folder scope was accepted: {root}");
        Ok(())
    }

    #[rstest]
    #[case("known.rs", "missing.rs")]
    #[case("missing.rs", "known.rs")]
    fn test_file_graph_endpoint_membership(#[case] source: &str, #[case] destination: &str) -> Result<()> {
        let files = BTreeSet::from([FileNode::new(PathBuf::from("known.rs"))?]);
        let dependencies = BTreeSet::from([FileEdge::new(
            FileNode::new(PathBuf::from(source))?, FileNode::new(PathBuf::from(destination))?,
            DependencyKind::Production,
        )]);

        let system_under_test = FileGraph::new(files, dependencies);

        assert!(system_under_test.is_err(), "File graph accepted an unknown dependency endpoint");
        Ok(())
    }

    #[rstest]
    #[case("known", "missing")]
    #[case("missing", "known")]
    fn test_folder_graph_endpoint_membership(#[case] source: &str, #[case] destination: &str) {
        let nodes = folder_nodes(&["known"]);
        let dependencies = folder_edges(&[(source, destination, 1)], DependencyKind::Production);

        let system_under_test = FolderGraph::new(nodes, dependencies);

        assert!(system_under_test.is_err(), "Folder graph accepted an unknown dependency endpoint");
    }
}
