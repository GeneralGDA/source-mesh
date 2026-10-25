use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use anyhow::Result;
use anyhow::ensure;
use getset::CopyGetters;
use getset::Getters;

use crate::cycles::CycleAnalysis;
use crate::cycles::CycleGroupId;
use crate::cycles::validate_folder_analysis;
use crate::model::DependencyCount;
use crate::model::DependencyKind;
use crate::model::FolderGraph;
use crate::model::FolderNode;

/* getset parses visibility as one space-delimited token; the types limit these accessors to export. */
#[derive(CopyGetters)]
#[getset(get_copy = "pub(crate)")]
pub(in crate::export) struct IndexedEdge {
    source: usize,
    target: usize,
    weight: DependencyCount,
    kind: DependencyKind,
    cycle_group: Option<CycleGroupId>,
}

#[derive(CopyGetters, Getters)]
pub(in crate::export) struct IndexedGraph<'graph> {
    #[getset(get = "pub(crate)")]
    nodes: BTreeMap<&'graph str, usize>,
    #[getset(get = "pub(crate)")]
    edges: Vec<IndexedEdge>,
    #[getset(get_copy = "pub(crate)")]
    cycle_group_count: usize,
}

impl<'graph> IndexedGraph<'graph> {
    #[must_use]
    pub(in crate::export) fn new(graph: &'graph FolderGraph) -> Self {
        let indexed_nodes = graph.nodes().iter().enumerate().map(|(index, node)| (node.label().as_str(), index));
        let nodes: BTreeMap<_, _> = indexed_nodes.collect();
        let edges = graph.edges().iter().map(|(endpoints, &weight)| {
            let node_indices = (nodes.get(endpoints.source().label().as_str()), nodes.get(endpoints.target().label().as_str()));
            let (Some(&source), Some(&target)) = node_indices else {
                unreachable!("Validated folder graph endpoints must belong to its nodes");
            };
            IndexedEdge { source, target, weight, kind: endpoints.kind(), cycle_group: None }
        }).collect();
        Self { nodes, edges, cycle_group_count: 0 }
    }

    pub(in crate::export) fn with_cycles(graph: &'graph FolderGraph, cycles: Option<&CycleAnalysis<FolderNode>>) -> Result<Self> {
        if let Some(cycles) = cycles {
            validate_folder_analysis(cycles, graph)?;
        }
        let mut indexed_graph = Self::new(graph);
        if let Some(cycles) = cycles {
            indexed_graph.cycle_group_count = cycles.group_count();
            for (edge, indexed_edge) in graph.edges().keys().zip(&mut indexed_graph.edges) {
                indexed_edge.cycle_group = cycles.edge_group(edge);
            }
        }
        Ok(indexed_graph)
    }
}

#[derive(CopyGetters, Getters)]
pub(in crate::export) struct Folder<'graph> {
    #[getset(get_copy = "pub(crate)")]
    label: &'graph str,
    #[getset(get_copy = "pub(crate)")]
    display_label: &'graph str,
    #[getset(get_copy = "pub(crate)")]
    index: usize,
    #[getset(get_copy = "pub(crate)")]
    dependency: bool,
    #[getset(get = "pub(crate)")]
    children: Vec<usize>,
}

impl Folder<'_> {
    fn make_relative_to(&mut self, common_root: &str) {
        if common_root != "." {
            let relative = self.label.strip_prefix(common_root).and_then(|relative| relative.strip_prefix('/'));
            let Some(relative) = relative else {
                unreachable!("Constructed descendants must belong to their common root");
            };
            self.display_label = relative;
        }
    }
}

#[derive(Default)]
pub(in crate::export) struct Hierarchy<'graph> {
    folders: Vec<Folder<'graph>>,
    roots: Vec<usize>,
}

impl<'graph> Hierarchy<'graph> {
    const INDEX_INVARIANT: &'static str = "Hierarchy indices must reference constructed folders";

    pub(in crate::export) fn new(graph: &IndexedGraph<'graph>) -> Result<Self> {
        ensure!(
            graph.nodes.keys().all(|label| {
                *label == "." || label.split('/').all(|component| !matches!(component, "" | "." | ".."))
            }),
            "Grouped folder labels must be '.' or slash-separated relative paths without empty, '.' or '..' components"
        );
        Ok(Self::from_validated_labels(graph))
    }

    #[must_use]
    fn from_validated_labels(graph: &IndexedGraph<'graph>) -> Self {
        let mut hierarchy = Self::default();
        if graph.nodes.is_empty() {
            return hierarchy;
        }
        let mut indices = graph.nodes.clone();
        hierarchy.folders = graph.nodes.iter().map(|(&label, &index)| {
            Folder { label, display_label: label, index, dependency: true, children: Vec::new() }
        }).collect();
        for &label in graph.nodes.keys() {
            let mut ancestor = label;
            while ancestor != "." {
                ancestor = parent_label(ancestor);
                if let Entry::Vacant(entry) = indices.entry(ancestor) {
                    let index = hierarchy.folders.len();
                    entry.insert(index);
                    hierarchy.folders.push(Folder {
                        label: ancestor,
                        display_label: ancestor,
                        index,
                        dependency: false,
                        children: Vec::new(),
                    });
                }
            }
        }
        for (&label, &index) in &indices {
            if label != "." {
                let Some(&parent) = indices.get(parent_label(label)) else {
                    unreachable!("Every constructed folder must have an ancestor entry");
                };
                hierarchy.folder_mut(parent).children.push(index);
            }
        }
        let Some(&root) = indices.get(".") else {
            unreachable!("A nonempty hierarchy must contain its constructed root");
        };
        hierarchy.remove_common_root(root);
        hierarchy
    }

    #[must_use]
    pub(in crate::export) fn folder(&self, index: usize) -> &Folder<'graph> {
        let Some(folder) = self.folders.get(index) else {
            unreachable!("{}", Self::INDEX_INVARIANT);
        };
        folder
    }

    fn folder_mut(&mut self, index: usize) -> &mut Folder<'graph> {
        let Some(folder) = self.folders.get_mut(index) else {
            unreachable!("{}", Self::INDEX_INVARIANT);
        };
        folder
    }

    pub(in crate::export) fn roots(&self) -> impl DoubleEndedIterator<Item = &Folder<'graph>> {
        self.roots.iter().map(|&index| self.folder(index))
    }

    pub(in crate::export) fn events(&self) -> impl Iterator<Item = FolderEvent<'_, 'graph>> {
        let mut work: Vec<_> = self.roots().rev().map(FolderEvent::Enter).collect();
        std::iter::from_fn(move || {
            let event = work.pop()?;
            if let FolderEvent::Enter(folder) = event && !folder.children.is_empty() {
                work.push(FolderEvent::Leave);
                work.extend(folder.children.iter().rev().map(|&index| FolderEvent::Enter(self.folder(index))));
            }
            Some(event)
        })
    }

    fn remove_common_root(&mut self, mut index: usize) {
        let mut common_root = ".";
        loop {
            let folder = self.folder_mut(index);
            let children = std::mem::take(&mut folder.children);
            if children.is_empty() {
                folder.make_relative_to(common_root);
                self.roots.push(index);
                return;
            }
            common_root = folder.label;
            if folder.dependency {
                self.roots.push(index);
            }
            if let &[child] = children.as_slice() {
                index = child;
            } else {
                let mut work = children.clone();
                while let Some(descendant_index) = work.pop() {
                    let descendant = self.folder_mut(descendant_index);
                    descendant.make_relative_to(common_root);
                    work.extend(&descendant.children);
                }
                self.roots.extend(children);
                return;
            }
        }
    }
}

pub(in crate::export) enum FolderEvent<'hierarchy, 'graph> {
    Enter(&'hierarchy Folder<'graph>),
    Leave,
}

#[must_use]
fn parent_label(label: &str) -> &str {
    label.rsplit_once('/').map_or(".", |(parent, _)| parent)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

    use claims::assert_ok;
    use claims::assert_some;
    use rstest::rstest;

    use super::FolderEvent;
    use super::Hierarchy;
    use super::IndexedGraph;
    use crate::model::DependencyCount;
    use crate::model::DependencyKind;
    use crate::model::FolderEdge;
    use crate::model::FolderGraph;
    use crate::model::FolderNode;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_hierarchy_ancestors_and_original_node_identifiers() {
        let fixture = FolderGraph::new(
            BTreeSet::from([FolderNode::new("src/api/v1".into()), FolderNode::new("src/store".into())]),
            BTreeMap::from([(
                FolderEdge::new(
                    FolderNode::new("src/api/v1".into()),
                    FolderNode::new("src/store".into()),
                    DependencyKind::Production,
                ),
                DependencyCount::new(7),
            )]),
        );
        let fixture = assert_ok!(fixture, "Ancestor fixture must form a valid folder graph");
        let indexed_graph = IndexedGraph::new(&fixture);

        let system_under_test = Hierarchy::new(&indexed_graph);
        let system_under_test = assert_ok!(system_under_test, "Valid folder labels must form a hierarchy");
        let api = system_under_test.roots().next();
        let api = assert_some!(api, "Hierarchy must contain the API ancestor");
        let version = api.children.first();
        let version = assert_some!(version, "API ancestor must contain its v1 child");
        let version = system_under_test.folder(*version);
        let storage = system_under_test.roots().next_back();
        let storage = assert_some!(storage, "Hierarchy must contain the storage folder");
        let edge = indexed_graph.edges.first();
        let edge = assert_some!(edge, "Indexed graph must retain the original dependency");

        assert_eq!(system_under_test.roots.len(), 2, "Common root containers must be omitted");
        assert_eq!(
            (api.label, api.display_label, api.dependency),
            ("src/api", "api", false),
            "An ancestor below the common root must remain a relative container"
        );
        assert_eq!(
            (version.label, version.display_label, version.index, version.dependency),
            ("src/api/v1", "api/v1", 0, true),
            "Original v1 endpoint changed"
        );
        assert_eq!(
            (storage.label, storage.display_label, storage.index, storage.dependency),
            ("src/store", "store", 1, true),
            "Original storage endpoint changed"
        );
        assert_eq!(indexed_graph.edges.len(), 1, "Hierarchy changed the edge count");
        assert_eq!((edge.source, edge.target, edge.weight), (0, 1, DependencyCount::new(7)), "Hierarchy changed the dependency");
    }

    #[test]
    fn test_hierarchy_common_ancestor_dependencies() {
        let fixture = FolderGraph::new(
            [".", "src", "src/api", "src/api/v1", "src/api/v2"].map(String::from).map(FolderNode::new).into(),
            BTreeMap::from([
                (
                    FolderEdge::new(FolderNode::new("src".into()), FolderNode::new("src/api".into()), DependencyKind::Production),
                    DependencyCount::new(2),
                ),
                (
                    FolderEdge::new(
                        FolderNode::new("src/api".into()),
                        FolderNode::new("src/api/v1".into()),
                        DependencyKind::Production,
                    ),
                    DependencyCount::new(3),
                ),
                (
                    FolderEdge::new(
                        FolderNode::new("src/api/v2".into()),
                        FolderNode::new("src".into()),
                        DependencyKind::Production,
                    ),
                    DependencyCount::new(5),
                ),
            ]),
        );
        let fixture = assert_ok!(fixture, "Common-ancestor dependencies must form a valid folder graph");
        let indexed_graph = IndexedGraph::new(&fixture);

        let system_under_test = Hierarchy::new(&indexed_graph);
        let system_under_test = assert_ok!(system_under_test, "Common-ancestor dependencies must support hierarchy creation");

        assert_eq!(system_under_test.roots.len(), 5, "All original folders, including isolated ancestors, must remain");
        assert!(
            system_under_test.roots().all(|folder| folder.children.is_empty() && folder.dependency),
            "Common ancestors must become separate dependency leaves"
        );
        for (folder, (canonical, display)) in system_under_test.roots().zip([
            (".", "."),
            ("src", "src"),
            ("src/api", "src/api"),
            ("src/api/v1", "v1"),
            ("src/api/v2", "v2"),
        ]) {
            assert!(folder.label == canonical && folder.display_label == display, "Unexpected folder label: {}", folder.label);
            assert_eq!(indexed_graph.nodes.get(canonical), Some(&folder.index), "Dependency endpoint was renumbered");
        }
        assert_eq!(indexed_graph.edges.len(), fixture.edges().len(), "Dependencies were removed");
        for ((endpoints, weight), edge) in fixture.edges().iter().zip(&indexed_graph.edges) {
            assert!(
                indexed_graph.nodes.get(endpoints.source().label().as_str()) == Some(&edge.source)
                    && indexed_graph.nodes.get(endpoints.target().label().as_str()) == Some(&edge.target)
                    && *weight == edge.weight,
                "Dependency endpoint or weight changed"
            );
        }
    }

    #[rstest]
    #[case(".", ".")]
    #[case("src", "src")]
    #[case("src/api/v1", "v1")]
    fn test_hierarchy_single_folder(#[case] label: &str, #[case] display_label: &str) {
        let fixture = FolderGraph::new(
            BTreeSet::from([FolderNode::new(label.into())]),
            BTreeMap::from([(
                FolderEdge::new(FolderNode::new(label.into()), FolderNode::new(label.into()), DependencyKind::Production),
                DependencyCount::new(4),
            )]),
        );
        let fixture = assert_ok!(fixture, "Single self-dependent folder must form a valid graph");
        let indexed_graph = IndexedGraph::new(&fixture);

        let system_under_test = Hierarchy::new(&indexed_graph);
        let system_under_test = assert_ok!(system_under_test, "Single folder must form a hierarchy: {label}");
        let folder = system_under_test.roots().next();
        let folder = assert_some!(folder, "Hierarchy must retain the only folder");
        let edge = indexed_graph.edges.first();
        let edge = assert_some!(edge, "Indexed graph must retain the self-dependency");

        assert_eq!(system_under_test.roots.len(), 1, "The only source folder must remain visible");
        assert_eq!(
            (folder.label, folder.display_label, folder.index, folder.dependency),
            (label, display_label, 0, true),
            "The only source folder changed"
        );
        assert!(folder.children.is_empty(), "The only source folder retained redundant containers");
        assert_eq!((edge.source, edge.target, edge.weight), (0, 0, DependencyCount::new(4)), "Self-dependency changed");
    }

    #[test]
    fn test_hierarchy_multiple_top_level_folders() {
        let fixture = FolderGraph::new(
            BTreeSet::from([FolderNode::new("src/api".into()), FolderNode::new("tests/api".into())]),
            BTreeMap::new(),
        );
        let fixture = assert_ok!(fixture, "Independent top-level folders must form a valid graph");
        let indexed_graph = IndexedGraph::new(&fixture);

        let system_under_test = Hierarchy::new(&indexed_graph);
        let system_under_test = assert_ok!(system_under_test, "Independent top-level folders must form a hierarchy");

        assert_eq!(system_under_test.roots.len(), 2, "Independent top-level folders must remain separate");
        for (folder, expected_label) in system_under_test.roots().zip(["src", "tests"]) {
            assert!(
                folder.label == expected_label && folder.display_label == expected_label && !folder.dependency,
                "A top-level folder was removed or relabeled"
            );
            let child = folder.children.first();
            let child = assert_some!(child, "Top-level folder {expected_label} must have a descendant");
            let child = system_under_test.folder(*child);
            assert!(child.label == child.display_label && child.dependency, "Descendant without a common prefix was relabeled");
        }
    }

    #[test]
    fn test_hierarchy_common_prefix_components() {
        let fixture = FolderGraph::new(
            BTreeSet::from([FolderNode::new("src/api".into()), FolderNode::new("src/api_extra".into())]),
            BTreeMap::new(),
        );
        let fixture = assert_ok!(fixture, "Folders with a shared textual prefix must form a valid graph");
        let indexed_graph = IndexedGraph::new(&fixture);

        let system_under_test = Hierarchy::new(&indexed_graph);
        let system_under_test = assert_ok!(system_under_test, "Shared textual prefixes must support hierarchy creation");

        let display_labels: Vec<_> = system_under_test.roots().map(|folder| folder.display_label).collect();

        assert_eq!(display_labels, ["api", "api_extra"], "A textual prefix inside a folder component was removed");
    }

    #[test]
    fn test_hierarchy_empty_graph() {
        let fixture = FolderGraph::default();
        let indexed_graph = IndexedGraph::new(&fixture);

        let system_under_test = Hierarchy::new(&indexed_graph);
        let system_under_test = assert_ok!(system_under_test, "An empty graph must support hierarchy creation");

        assert!(system_under_test.roots.is_empty(), "An empty graph must have no hierarchy roots");
    }

    #[rstest]
    #[case("")]
    #[case("/src")]
    #[case("src/")]
    #[case("src//api")]
    #[case("src/./api")]
    #[case("src/../api")]
    fn test_hierarchy_path_preconditions(#[case] label: &str) {
        let fixture = FolderGraph::new(BTreeSet::from([FolderNode::new(label.into())]), BTreeMap::new());
        let fixture = assert_ok!(fixture, "Folder graph must accept labels before hierarchy path validation: {label:?}");
        let indexed_graph = IndexedGraph::new(&fixture);

        let system_under_test = Hierarchy::new(&indexed_graph);

        assert!(system_under_test.is_err(), "Invalid hierarchy path was accepted: {label}");
    }

    #[rstest]
    #[case(false)]
    #[case(true)]
    fn test_deep_hierarchy_traversal_and_destruction(#[case] common_prefix: bool) {
        const DEPTH: usize = 4096;
        const COMMON_PREFIX: &str = "common/";
        let prefix = if common_prefix { COMMON_PREFIX } else { "" };
        let fixture = FolderGraph::new(
            [format!("{prefix}{}leaf", "a/".repeat(DEPTH)), format!("{prefix}z")].map(FolderNode::new).into(),
            BTreeMap::new(),
        );
        let fixture = assert_ok!(fixture, "Deep folder paths must form a valid graph");
        let indexed_graph = IndexedGraph::new(&fixture);

        {
            let system_under_test = Hierarchy::new(&indexed_graph);
            let system_under_test = assert_ok!(system_under_test, "Deep hierarchy creation must not exhaust the stack");
            let mut depth: usize = 0;
            let mut maximum_depth = 0;
            let mut leaves = Vec::new();
            for event in system_under_test.events() {
                match event {
                    FolderEvent::Enter(folder) if folder.children.is_empty() => {
                        leaves.push(folder.index);
                    }
                    FolderEvent::Enter(_) => {
                        depth += 1;
                        maximum_depth = maximum_depth.max(depth);
                    }
                    FolderEvent::Leave => depth -= 1,
                }
            }

            assert_eq!(system_under_test.roots.len(), 2, "Separate roots must prevent deep-chain flattening");
            assert_eq!((maximum_depth, depth), (DEPTH, 0), "Traversal lost or unbalanced nested groups");
            assert_eq!(leaves, [0, 1], "Deep traversal changed endpoint order");
            assert!(
                system_under_test.events().all(|event| match event {
                    FolderEvent::Enter(folder) => !folder.display_label.starts_with(COMMON_PREFIX),
                    FolderEvent::Leave => true,
                }),
                "Deep relative-label traversal retained the common root"
            );
        }
    }
}
