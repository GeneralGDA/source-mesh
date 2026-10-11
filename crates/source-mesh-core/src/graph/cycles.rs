use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::hash_map::RandomState;
use std::iter;
use std::num::NonZeroUsize;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use getset::CopyGetters;
use getset::Getters;
use petgraph::algo::all_simple_paths;
use petgraph::algo::kosaraju_scc;
use petgraph::graph::DiGraph;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeFiltered;
use petgraph::visit::EdgeRef as _;
use petgraph::visit::IntoNeighbors as _;

use crate::model::Edge;
use crate::model::FileGraph;
use crate::model::FileNode;
use crate::model::FolderGraph;
use crate::model::FolderNode;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, CopyGetters)]
#[getset(get_copy = "pub")]
pub struct CycleGroupId {
    number: NonZeroUsize,
}

#[derive(Getters, CopyGetters)]
pub struct Cycle<Node> {
    #[getset(get = "pub")]
    nodes: Vec<Node>,
    #[getset(get_copy = "pub")]
    group: CycleGroupId,
}

struct CyclicComponent {
    group: CycleGroupId,
    nodes: BTreeSet<NodeIndex>,
}

pub struct CycleAnalysis<Node> {
    topology: DiGraph<Node, ()>,
    components: Vec<CyclicComponent>,
    edge_groups: BTreeMap<Edge<Node>, Option<CycleGroupId>>,
}

impl<Node: Clone + Ord> CycleAnalysis<Node> {
    fn new<'a>(nodes: &BTreeSet<Node>, edges: impl Iterator<Item = &'a Edge<Node>>) -> Result<Self>
    where
        Node: 'a,
    {
        let edges: Vec<_> = edges.collect();
        ensure!(
            edges.iter().all(|edge| nodes.contains(edge.source()) && nodes.contains(edge.target())),
            "Cycle analysis has an unknown dependency endpoint"
        );
        let mut topology = DiGraph::new();
        let node_indices: BTreeMap<_, _> = nodes.iter()
            .map(|node| (node, topology.add_node(node.clone())))
            .collect();
        for edge in &edges {
            topology.update_edge(
                *node_indices.get(edge.source()).context("Missing cycle source node")?,
                *node_indices.get(edge.target()).context("Missing cycle target node")?,
                (),
            );
        }
        let mut components = kosaraju_scc(&topology);
        components.retain(|component| {
            component.len() > 1
                || component.first().is_some_and(|node| topology.contains_edge(*node, *node))
        });
        for component in &mut components {
            component.sort_unstable();
        }
        components.sort_unstable();
        let mut cyclic_components = Vec::new();
        let mut node_groups = BTreeMap::new();
        for (index, component) in components.into_iter().enumerate() {
            let group = CycleGroupId {
                number: index.checked_add(1).and_then(NonZeroUsize::new)
                    .context("Cycle group count overflow")?,
            };
            for node in &component {
                node_groups.insert(*node, group);
            }
            cyclic_components.push(CyclicComponent {
                group,
                nodes: component.into_iter().collect(),
            });
        }
        let edge_groups = edges.into_iter().map(|edge| {
            let source = node_indices.get(edge.source()).and_then(|node| node_groups.get(node));
            let target = node_indices.get(edge.target()).and_then(|node| node_groups.get(node));
            let group = match (source, target) {
                (Some(source_group), Some(target_group)) if source_group == target_group => {
                    Some(*source_group)
                }
                _ => None,
            };
            (Edge::new(edge.source().clone(), edge.target().clone(), edge.kind()), group)
        }).collect();
        Ok(Self {
            topology,
            components: cyclic_components,
            edge_groups,
        })
    }

    #[must_use]
    pub fn edge_group(&self, edge: &Edge<Node>) -> Option<CycleGroupId> {
        self.edge_groups.get(edge).copied().flatten()
    }

    #[must_use]
    pub const fn group_count(&self) -> usize {
        self.components.len()
    }

    pub fn cycles(&self, limit: NonZeroUsize) -> Result<Vec<Cycle<Node>>> {
        let mut cycles = Vec::new();
        for component in &self.components {
            for &minimum_cycle_node in &component.nodes {
                let cycle_subgraph = EdgeFiltered::from_fn(&self.topology, |edge| {
                    [edge.source(), edge.target()].into_iter()
                        .all(|node| node >= minimum_cycle_node && component.nodes.contains(&node))
                });
                for first_successor in cycle_subgraph.neighbors(minimum_cycle_node) {
                    let paths: Box<dyn Iterator<Item = Vec<NodeIndex>> + '_> = if first_successor == minimum_cycle_node {
                        Box::new(iter::once(vec![minimum_cycle_node]))
                    } else {
                        Box::new(all_simple_paths::<Vec<_>, _, RandomState>(&cycle_subgraph, first_successor, minimum_cycle_node, 0, None))
                    };
                    for path in paths {
                        ensure!(
                            cycles.len() < limit.get(),
                            "Cycle count exceeds the configured limit of {limit}; increase the limit to enumerate every cycle"
                        );
                        let nodes = iter::once(minimum_cycle_node).chain(path)
                            .map(|node| self.topology.node_weight(node).cloned().context("Missing cycle path node"))
                            .collect::<Result<Vec<_>>>()?;
                        cycles.push(Cycle { nodes, group: component.group });
                    }
                }
            }
        }
        cycles.sort_by(|left, right| (left.group, &left.nodes).cmp(&(right.group, &right.nodes)));
        Ok(cycles)
    }
}

pub fn analyze_folders(graph: &FolderGraph) -> Result<CycleAnalysis<FolderNode>> {
    CycleAnalysis::new(graph.nodes(), graph.edges().keys())
}

pub fn analyze_files(graph: &FileGraph) -> Result<CycleAnalysis<FileNode>> {
    CycleAnalysis::new(graph.files(), graph.edges().iter())
}

pub(crate) fn validate_folder_analysis(analysis: &CycleAnalysis<FolderNode>, graph: &FolderGraph) -> Result<()> {
    ensure!(
        analysis.topology.node_weights().eq(graph.nodes())
            && analysis.edge_groups.keys().eq(graph.edges().keys()),
        "Cycle analysis does not match the folder graph"
    );
    Ok(())
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Tests use assertions for expectations while returning Result for fallible setup."
)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;
    use std::iter;
    use std::num::NonZeroUsize;
    use std::path::Path;
    use std::path::PathBuf;

    use anyhow::Context as _;
    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::CycleAnalysis;
    use super::analyze_files;
    use super::analyze_folders;
    use super::validate_folder_analysis;
    use crate::model::DependencyCount;
    use crate::model::DependencyKind;
    use crate::model::DependencyKind::Production;
    use crate::model::DependencyKind::Test;
    use crate::model::FileEdge;
    use crate::model::FileGraph;
    use crate::model::FileNode;
    use crate::model::FolderEdge;
    use crate::model::FolderGraph;
    use crate::model::FolderNode;

    fn folder_fixture(dependencies: &[(&str, &str, DependencyKind)]) -> Result<FolderGraph> {
        let edges = folder_dependencies(dependencies);
        let nodes = edges.keys()
            .flat_map(|edge| [edge.source().clone(), edge.target().clone()])
            .collect();
        FolderGraph::new(nodes, edges)
    }

    #[must_use]
    fn folder_dependencies(dependencies: &[(&str, &str, DependencyKind)]) -> BTreeMap<FolderEdge, DependencyCount> {
        dependencies.iter().map(|&(source, target, kind)| {
            (FolderEdge::new(FolderNode::new(source.into()), FolderNode::new(target.into()), kind), DependencyCount::new(1))
        }).collect()
    }

    #[must_use]
    fn folder_path(labels: &[&str]) -> Vec<FolderNode> {
        labels.iter().map(|label| FolderNode::new((*label).into())).collect()
    }

    #[rstest]
    #[case(&[], vec![])]
    #[case(&[("alpha", "beta", Production), ("beta", "gamma", Production)], vec![])]
    #[case(&[("alpha", "beta", Production), ("gamma", "beta", Production)], vec![])]
    #[case(
        &[("alpha", "beta", Production), ("beta", "alpha", Production)],
        vec![vec!["alpha", "beta", "alpha"]]
    )]
    #[case(
        &[("alpha", "beta", Production), ("beta", "gamma", Production), ("gamma", "alpha", Production)],
        vec![vec!["alpha", "beta", "gamma", "alpha"]]
    )]
    #[case(
        &[("alpha", "beta", Production), ("beta", "alpha", Production), ("beta", "gamma", Production), ("gamma", "beta", Production)],
        vec![vec!["alpha", "beta", "alpha"], vec!["beta", "gamma", "beta"]]
    )]
    #[case(
        &[("alpha", "alpha", Production)],
        vec![vec!["alpha", "alpha"]]
    )]
    #[case(
        &[("alpha", "alpha", Production), ("alpha", "beta", Production), ("beta", "alpha", Production)],
        vec![vec!["alpha", "alpha"], vec!["alpha", "beta", "alpha"]]
    )]
    #[case(
        &[("alpha", "beta", Production), ("alpha", "beta", Test), ("beta", "alpha", Production), ("beta", "alpha", Test)],
        vec![vec!["alpha", "beta", "alpha"]]
    )]
    #[case(
        &[("alpha", "alpha", Production), ("alpha", "alpha", Test)],
        vec![vec!["alpha", "alpha"]]
    )]
    #[case(
        &[("alpha", "beta", Production), ("alpha", "gamma", Production), ("beta", "omega", Production), ("gamma", "omega", Production), ("omega", "alpha", Production)],
        vec![vec!["alpha", "beta", "omega", "alpha"], vec!["alpha", "gamma", "omega", "alpha"]]
    )]
    #[case(
        &[("alpha", "beta", Production), ("beta", "gamma", Production), ("gamma", "alpha", Production), ("alpha", "gamma", Production), ("gamma", "beta", Production), ("beta", "alpha", Production)],
        vec![vec!["alpha", "beta", "alpha"], vec!["alpha", "beta", "gamma", "alpha"], vec!["alpha", "gamma", "alpha"], vec!["alpha", "gamma", "beta", "alpha"], vec!["beta", "gamma", "beta"]]
    )]
    fn test_directed_cycle_routes(
        #[case] dependencies: &[(&str, &str, DependencyKind)],
        #[case] expected_paths: Vec<Vec<&str>>,
    ) -> Result<()> {
        let fixture = folder_fixture(dependencies)?;
        let system_under_test = analyze_folders(&fixture)?;
        let expected: Vec<_> = expected_paths.into_iter().map(|labels| folder_path(&labels)).collect();

        let cycles = system_under_test.cycles(NonZeroUsize::new(10).context("Invalid test cycle limit")?)?;
        let paths: Vec<_> = cycles.into_iter().map(|cycle| cycle.nodes).collect();

        assert_eq!(expected, paths, "Directed cycle routes or their canonical order differ");
        Ok(())
    }

    #[rstest]
    #[case("alpha", "beta", Production, Some(1))]
    #[case("beta", "alpha", Test, Some(1))]
    #[case("gamma", "omega", Production, Some(2))]
    #[case("omega", "gamma", Production, Some(2))]
    #[case("beta", "gamma", Production, None)]
    #[case("alpha", "omega", Production, None)]
    #[case("alpha", "beta", Test, None)]
    #[case("unknown", "alpha", Production, None)]
    fn test_component_edge_membership(
        #[case] source: &str,
        #[case] target: &str,
        #[case] kind: DependencyKind,
        #[case] expected_group: Option<usize>,
    ) -> Result<()> {
        let fixture = folder_fixture(&[
            ("omega", "gamma", Production),
            ("gamma", "omega", Production),
            ("beta", "gamma", Production),
            ("beta", "alpha", Test),
            ("alpha", "beta", Production),
        ])?;
        let system_under_test = analyze_folders(&fixture)?;
        let edge = FolderEdge::new(FolderNode::new(source.into()), FolderNode::new(target.into()), kind);

        let group = system_under_test.edge_group(&edge);

        assert_eq!(2, system_under_test.group_count(), "Independent cyclic components were merged");
        assert_eq!(expected_group, group.map(|identifier| identifier.number().get()), "Incorrect cycle edge group");
        Ok(())
    }

    #[test]
    fn test_cycle_group_assignment() -> Result<()> {
        let fixture = folder_fixture(&[
            ("gamma", "omega", Production),
            ("omega", "gamma", Production),
            ("alpha", "beta", Production),
            ("beta", "alpha", Production),
            ("beta", "gamma", Production),
        ])?;
        let system_under_test = analyze_folders(&fixture)?;
        let expected = [folder_path(&["alpha", "beta", "alpha"]), folder_path(&["gamma", "omega", "gamma"])];

        let cycles = system_under_test.cycles(NonZeroUsize::new(2).context("Invalid test cycle limit")?)?;
        let groups: Vec<_> = cycles.iter().map(|cycle| cycle.group().number().get()).collect();
        let paths: Vec<_> = cycles.into_iter().map(|cycle| cycle.nodes).collect();

        assert_eq!(groups, [1, 2], "Cycle group numbers differ");
        assert_eq!(paths, expected, "Independent cycle routes or their order differ");
        Ok(())
    }

    #[rstest]
    #[case(1, true)]
    #[case(2, false)]
    #[case(3, false)]
    fn test_cycle_enumeration_limit(#[case] limit: usize, #[case] exceeds_limit: bool) -> Result<()> {
        let fixture = folder_fixture(&[
            ("alpha", "beta", Production),
            ("beta", "alpha", Production),
            ("beta", "gamma", Production),
            ("gamma", "beta", Production),
        ])?;
        let system_under_test = analyze_folders(&fixture)?;

        let result = system_under_test.cycles(NonZeroUsize::new(limit).context("Invalid test cycle limit")?);

        assert_eq!(1, system_under_test.group_count(), "Overlapping cycles have separate groups");
        if exceeds_limit {
            assert!(result.is_err(), "Excess cycles were silently truncated");
        } else {
            assert_eq!(2, result?.len(), "Enumeration did not retain every cycle within the limit");
        }
        Ok(())
    }

    #[test]
    fn test_isolated_nodes() -> Result<()> {
        let fixture = FolderGraph::new(BTreeSet::from([FolderNode::new("isolated".into())]), BTreeMap::new())?;
        let system_under_test = analyze_folders(&fixture)?;

        let cycles = system_under_test.cycles(NonZeroUsize::MIN)?;

        assert_eq!(0, system_under_test.group_count(), "Isolated node became a cyclic component");
        assert!(cycles.is_empty(), "Isolated node produced a phantom cycle");
        Ok(())
    }

    #[rstest]
    #[case(Path::new("."), false, 1)]
    #[case(Path::new("."), true, 0)]
    #[case(Path::new("alpha"), false, 0)]
    #[case(Path::new("beta"), false, 0)]
    fn test_file_cycle_routes(
        #[case] root: &Path,
        #[case] exclude_tests: bool,
        #[case] expected_cycles: usize,
    ) -> Result<()> {
        let first_file = FileNode::new(PathBuf::from_iter(["alpha", "source.rs"]))?;
        let second_file = FileNode::new(PathBuf::from_iter(["beta", "source.rs"]))?;
        let fixture = FileGraph::new(
            BTreeSet::from([first_file.clone(), second_file.clone()]),
            BTreeSet::from([
                FileEdge::new(first_file.clone(), second_file.clone(), Production),
                FileEdge::new(second_file.clone(), first_file.clone(), Test),
            ]),
        )?;
        let fixture = if exclude_tests { fixture.without_test_dependencies() } else { fixture }.within_root(root)?;
        let system_under_test = analyze_files(&fixture)?;
        let expected = vec![first_file.clone(), second_file, first_file];

        let cycles = system_under_test.cycles(NonZeroUsize::MIN)?;

        assert_eq!(expected_cycles, system_under_test.group_count(), "File cycle components ignored root or test filtering");
        assert_eq!(expected_cycles, cycles.len(), "File cycle count differs");
        if expected_cycles != 0 {
            assert!(cycles.first().is_some_and(|cycle| cycle.nodes() == &expected), "File cycle route differs");
            assert!(fixture.edges().iter().all(|edge| system_under_test.edge_group(edge).is_some()), "File cycle edge was not grouped");
        }
        Ok(())
    }

    #[test]
    fn test_dependency_endpoint_membership() -> Result<()> {
        let nodes = BTreeSet::from([FolderNode::new("alpha".into())]);
        let edge = FolderEdge::new(FolderNode::new("alpha".into()), FolderNode::new("unknown".into()), Production);

        let system_under_test = CycleAnalysis::new(&nodes, iter::once(&edge));

        assert!(system_under_test.is_err(), "Unknown dependency endpoint was accepted");
        Ok(())
    }

    #[rstest]
    #[case(&[("alpha", "beta", Production), ("beta", "gamma", Production)])]
    #[case(&[("alpha", "beta", Production), ("beta", "alpha", Production), ("beta", "gamma", Production), ("gamma", "alpha", Production)])]
    #[case(&[("alpha", "beta", Test), ("beta", "alpha", Production), ("beta", "gamma", Production)])]
    #[case(&[("alpha", "beta", Production), ("beta", "alpha", Production)])]
    fn test_folder_analysis_dependencies(#[case] dependencies: &[(&str, &str, DependencyKind)]) -> Result<()> {
        let fixture = folder_fixture(&[
            ("alpha", "beta", Production),
            ("beta", "alpha", Production),
            ("beta", "gamma", Production),
        ])?;
        let system_under_test = analyze_folders(&fixture)?;
        let changed_graph = FolderGraph::new(fixture.nodes().clone(), folder_dependencies(dependencies))?;

        let result = validate_folder_analysis(&system_under_test, &changed_graph);

        assert!(result.is_err(), "Changed dependency topology or kind accepted an outdated cycle analysis");
        Ok(())
    }

    #[rstest]
    #[case(&["alpha", "beta"], false)]
    #[case(&["alpha", "beta", "isolated", "other"], false)]
    #[case(&["isolated", "beta", "alpha"], true)]
    fn test_folder_analysis_nodes(#[case] labels: &[&str], #[case] matches_analysis: bool) -> Result<()> {
        let dependencies = [("alpha", "beta", Production), ("beta", "alpha", Production)];
        let fixture = FolderGraph::new(
            folder_path(&["alpha", "beta", "isolated"]).into_iter().collect(),
            folder_dependencies(&dependencies),
        )?;
        let system_under_test = analyze_folders(&fixture)?;
        let changed_graph = FolderGraph::new(folder_path(labels).into_iter().collect(), folder_dependencies(&dependencies))?;

        let result = validate_folder_analysis(&system_under_test, &changed_graph);

        assert_eq!(matches_analysis, result.is_ok(), "Cycle analysis did not validate the full folder node set");
        Ok(())
    }

    #[rstest]
    #[case(1)]
    #[case(27)]
    fn test_folder_analysis_dependency_counts(#[case] count: usize) -> Result<()> {
        let dependencies = [("alpha", "beta", Production), ("beta", "alpha", Production)];
        let fixture = folder_fixture(&dependencies)?;
        let system_under_test = analyze_folders(&fixture)?;
        let mut edges = folder_dependencies(&dependencies);
        for weight in edges.values_mut() {
            *weight = DependencyCount::new(count);
        }
        let changed_graph = FolderGraph::new(fixture.nodes().clone(), edges)?;

        let result = validate_folder_analysis(&system_under_test, &changed_graph);

        assert!(result.is_ok(), "Dependency counts invalidated an unchanged cycle topology");
        Ok(())
    }
}
