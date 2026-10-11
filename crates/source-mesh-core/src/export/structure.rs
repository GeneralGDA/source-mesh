use std::collections::BTreeMap;

use anyhow::Context as _;
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

#[derive(CopyGetters)]
#[getset(get_copy = "pub(super)")]
pub(super) struct IndexedEdge {
    source: usize,
    target: usize,
    weight: DependencyCount,
    kind: DependencyKind,
    cycle_group: Option<CycleGroupId>,
}

#[derive(CopyGetters, Getters)]
pub(super) struct IndexedGraph<'graph> {
    #[getset(get = "pub(super)")]
    nodes: BTreeMap<&'graph str, usize>,
    #[getset(get = "pub(super)")]
    edges: Vec<IndexedEdge>,
    #[getset(get_copy = "pub(super)")]
    cycle_group_count: usize,
}

impl<'graph> IndexedGraph<'graph> {
    pub(super) fn new(graph: &'graph FolderGraph) -> Result<Self> {
        let nodes: BTreeMap<_, _> = graph
            .nodes()
            .iter()
            .enumerate()
            .map(|(index, node)| (node.label().as_str(), index))
            .collect();
        let edges = graph
            .edges()
            .iter()
            .map(|(endpoints, &weight)| {
                Ok(IndexedEdge {
                    source: *nodes.get(endpoints.source().label().as_str()).context("edge source is absent from graph nodes")?,
                    target: *nodes.get(endpoints.target().label().as_str()).context("edge target is absent from graph nodes")?,
                    weight,
                    kind: endpoints.kind(),
                    cycle_group: None,
                })
            })
            .collect::<Result<_>>()?;
        Ok(Self { nodes, edges, cycle_group_count: 0 })
    }

    pub(super) fn with_cycles(graph: &'graph FolderGraph, cycles: Option<&CycleAnalysis<FolderNode>>) -> Result<Self> {
        if let Some(cycles) = cycles {
            validate_folder_analysis(cycles, graph)?;
        }
        let mut indexed_graph = Self::new(graph)?;
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
pub(super) struct Folder<'graph> {
    #[getset(get_copy = "pub(super)")]
    label: &'graph str,
    #[getset(get_copy = "pub(super)")]
    display_label: &'graph str,
    #[getset(get_copy = "pub(super)")]
    index: usize,
    #[getset(get_copy = "pub(super)")]
    dependency: bool,
    #[getset(get = "pub(super)")]
    children: Vec<Self>,
}

impl<'graph> Folder<'graph> {
    pub(super) fn hierarchy(graph: &IndexedGraph<'graph>) -> Result<Vec<Self>> {
        ensure!(graph.nodes.keys().all(|label| {
            *label == "." || label.split('/').all(|component| !matches!(component, "" | "." | ".."))
        }), "Grouped folder labels must be '.' or slash-separated relative paths without empty, '.' or '..' components");
        if graph.nodes.is_empty() {
            return Ok(Vec::new());
        }
        let mut indices = graph.nodes.clone();
        for &label in graph.nodes.keys() {
            let mut ancestor = label;
            while ancestor != "." {
                ancestor = parent_label(ancestor);
                let next_index = indices.len();
                indices.entry(ancestor).or_insert(next_index);
            }
        }
        let mut folders: BTreeMap<_, _> = indices
            .iter()
            .map(|(&label, &index)| (label, Self {
                label,
                display_label: label,
                index,
                dependency: graph.nodes.contains_key(label),
                children: Vec::new(),
            }))
            .collect();
        let descendants_before_parents = indices.keys().rev().filter(|&&label| label != ".");
        for &label in descendants_before_parents {
            let mut folder = folders.remove(label).context("Hierarchy folder is missing")?;
            folder.children.reverse();
            folders.get_mut(parent_label(label)).context("Hierarchy parent is missing")?.children.push(folder);
        }
        let mut root = folders.remove(".").context("Hierarchy root is missing")?;
        root.children.reverse();
        root.without_common_root()
    }

    fn without_common_root(mut self) -> Result<Vec<Self>> {
        let mut roots = Vec::new();
        let mut common_root = ".";
        while !self.children.is_empty() {
            common_root = self.label();
            let mut children = std::mem::take(&mut self.children);
            if self.dependency {
                roots.push(self);
            }
            if children.len() == 1 {
                self = children.pop().context("Common hierarchy descendant is missing")?;
            } else {
                for child in &mut children {
                    child.make_relative_to(common_root)?;
                }
                roots.extend(children);
                return Ok(roots);
            }
        }
        self.make_relative_to(common_root)?;
        roots.push(self);
        Ok(roots)
    }

    fn make_relative_to(&mut self, common_root: &str) -> Result<()> {
        if common_root != "." {
            self.display_label = self.label.strip_prefix(common_root)
                .and_then(|relative| relative.strip_prefix('/'))
                .context("Visible hierarchy folder is outside its common root")?;
        }
        for child in &mut self.children {
            child.make_relative_to(common_root)?;
        }
        Ok(())
    }
}

#[must_use]
fn parent_label(label: &str) -> &str {
    label.rsplit_once('/').map_or(".", |(parent, _)| parent)
}

#[cfg(test)]
#[expect(clippy::panic_in_result_fn, reason = "Tests use assertions for expectations while returning Result for fallible setup.")]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

    use anyhow::Context as _;
    use anyhow::Result;
    use rstest::rstest;

    use pretty_assertions::assert_eq;
    use super::Folder;
    use super::IndexedGraph;
    use crate::model::DependencyCount;
    use crate::model::DependencyKind;
    use crate::model::FolderEdge;
    use crate::model::FolderGraph;
    use crate::model::FolderNode;

    #[test]
    fn test_hierarchy_ancestors_and_original_node_identifiers() -> Result<()> {
        let fixture = FolderGraph::new(
            BTreeSet::from([FolderNode::new("src/api/v1".into()), FolderNode::new("src/store".into())]),
            BTreeMap::from([(
                FolderEdge::new(FolderNode::new("src/api/v1".into()), FolderNode::new("src/store".into()), DependencyKind::Production),
                DependencyCount::new(7),
            )]),
        )?;
        let indexed_graph = IndexedGraph::new(&fixture)?;

        let system_under_test = Folder::hierarchy(&indexed_graph)?;
        let api = system_under_test.first().context("api is missing")?;
        let version = api.children.first().context("v1 is missing")?;
        let storage = system_under_test.last().context("store is missing")?;
        let edge = indexed_graph.edges.first().context("Dependency is missing")?;

        assert_eq!(system_under_test.len(), 2, "Common root containers must be omitted");
        assert_eq!((api.label, api.display_label, api.dependency), ("src/api", "api", false),
            "An ancestor below the common root must remain a relative container");
        assert_eq!((version.label, version.display_label, version.index, version.dependency),
            ("src/api/v1", "api/v1", 0, true), "Original v1 endpoint changed");
        assert_eq!((storage.label, storage.display_label, storage.index, storage.dependency),
            ("src/store", "store", 1, true), "Original storage endpoint changed");
        assert_eq!(indexed_graph.edges.len(), 1, "Hierarchy changed the edge count");
        assert_eq!((edge.source, edge.target, edge.weight), (0, 1, DependencyCount::new(7)), "Hierarchy changed the dependency");
        Ok(())
    }

    #[test]
    fn test_hierarchy_common_ancestor_dependencies() -> Result<()> {
        let fixture = FolderGraph::new(
            [".", "src", "src/api", "src/api/v1", "src/api/v2"].map(String::from).map(FolderNode::new).into(),
            BTreeMap::from([
                (FolderEdge::new(FolderNode::new("src".into()), FolderNode::new("src/api".into()), DependencyKind::Production), DependencyCount::new(2)),
                (FolderEdge::new(FolderNode::new("src/api".into()), FolderNode::new("src/api/v1".into()), DependencyKind::Production), DependencyCount::new(3)),
                (FolderEdge::new(FolderNode::new("src/api/v2".into()), FolderNode::new("src".into()), DependencyKind::Production), DependencyCount::new(5)),
            ]),
        )?;
        let indexed_graph = IndexedGraph::new(&fixture)?;

        let system_under_test = Folder::hierarchy(&indexed_graph)?;

        assert_eq!(system_under_test.len(), 5, "All original folders, including isolated ancestors, must remain");
        assert!(system_under_test.iter().all(|folder| folder.children.is_empty() && folder.dependency),
            "Common ancestors must become separate dependency leaves");
        for (folder, (canonical, display)) in system_under_test.iter().zip([
            (".", "."), ("src", "src"), ("src/api", "src/api"), ("src/api/v1", "v1"), ("src/api/v2", "v2"),
        ]) {
            assert!(folder.label == canonical && folder.display_label == display,
                "Unexpected common ancestor or descendant label: {}", folder.label);
            assert_eq!(indexed_graph.nodes.get(canonical), Some(&folder.index), "Dependency endpoint was renumbered");
        }
        assert_eq!(indexed_graph.edges.len(), fixture.edges().len(), "Dependencies were removed");
        for ((endpoints, weight), edge) in fixture.edges().iter().zip(&indexed_graph.edges) {
            assert!(indexed_graph.nodes.get(endpoints.source().label().as_str()) == Some(&edge.source)
                && indexed_graph.nodes.get(endpoints.target().label().as_str()) == Some(&edge.target)
                && *weight == edge.weight, "Dependency endpoint or weight changed");
        }
        Ok(())
    }

    #[rstest]
    #[case(".", ".")]
    #[case("src", "src")]
    #[case("src/api/v1", "v1")]
    fn test_hierarchy_single_folder(#[case] label: &str, #[case] display_label: &str) -> Result<()> {
        let fixture = FolderGraph::new(
            BTreeSet::from([FolderNode::new(label.into())]),
            BTreeMap::from([(
                FolderEdge::new(FolderNode::new(label.into()), FolderNode::new(label.into()), DependencyKind::Production),
                DependencyCount::new(4),
            )]),
        )?;
        let indexed_graph = IndexedGraph::new(&fixture)?;

        let system_under_test = Folder::hierarchy(&indexed_graph)?;
        let folder = system_under_test.first().context("Only folder is missing")?;
        let edge = indexed_graph.edges.first().context("Self-dependency is missing")?;

        assert_eq!(system_under_test.len(), 1, "The only source folder must remain visible");
        assert_eq!((folder.label, folder.display_label, folder.index, folder.dependency),
            (label, display_label, 0, true), "The only source folder changed");
        assert!(folder.children.is_empty(), "The only source folder retained redundant containers");
        assert_eq!((edge.source, edge.target, edge.weight), (0, 0, DependencyCount::new(4)), "Self-dependency changed");
        Ok(())
    }

    #[test]
    fn test_hierarchy_multiple_top_level_folders() -> Result<()> {
        let fixture = FolderGraph::new(
            BTreeSet::from([FolderNode::new("src/api".into()), FolderNode::new("tests/api".into())]), BTreeMap::new(),
        )?;
        let indexed_graph = IndexedGraph::new(&fixture)?;

        let system_under_test = Folder::hierarchy(&indexed_graph)?;

        assert_eq!(system_under_test.len(), 2, "Independent top-level folders must remain separate");
        for (folder, expected_label) in system_under_test.iter().zip(["src", "tests"]) {
            assert!(folder.label == expected_label && folder.display_label == expected_label && !folder.dependency,
                "A top-level folder was removed or relabeled");
            let child = folder.children.first().context("Top-level descendant is missing")?;
            assert!(child.label == child.display_label && child.dependency,
                "A descendant without a shared path prefix was relabeled");
        }
        Ok(())
    }

    #[test]
    fn test_hierarchy_common_prefix_components() -> Result<()> {
        let fixture = FolderGraph::new(
            BTreeSet::from([FolderNode::new("src/api".into()), FolderNode::new("src/api_extra".into())]), BTreeMap::new(),
        )?;
        let indexed_graph = IndexedGraph::new(&fixture)?;

        let system_under_test = Folder::hierarchy(&indexed_graph)?;

        let display_labels: Vec<_> = system_under_test.iter().map(|folder| folder.display_label).collect();

        assert_eq!(display_labels, ["api", "api_extra"], "A textual prefix inside a folder component was removed");
        Ok(())
    }

    #[test]
    fn test_hierarchy_empty_graph() -> Result<()> {
        let fixture = FolderGraph::default();
        let indexed_graph = IndexedGraph::new(&fixture)?;

        let system_under_test = Folder::hierarchy(&indexed_graph)?;

        assert!(system_under_test.is_empty(), "An empty graph must have no hierarchy roots");
        Ok(())
    }

    #[rstest]
    #[case("")]
    #[case("/src")]
    #[case("src/")]
    #[case("src//api")]
    #[case("src/./api")]
    #[case("src/../api")]
    fn test_hierarchy_path_preconditions(#[case] label: &str) -> Result<()> {
        let fixture = FolderGraph::new(BTreeSet::from([FolderNode::new(label.into())]), BTreeMap::new())?;
        let indexed_graph = IndexedGraph::new(&fixture)?;

        let system_under_test = Folder::hierarchy(&indexed_graph);

        assert!(system_under_test.is_err(), "Invalid hierarchy path was accepted: {label}");
        Ok(())
    }
}
