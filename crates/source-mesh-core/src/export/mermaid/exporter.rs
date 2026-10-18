use std::io::Write;

use anyhow::Result;
use askama::Template;

use crate::cycle_presentation::group_color;
use crate::export::rendering::edge_style::CYCLE_EDGE_WIDTH;
use crate::export::rendering::edge_style::TEST_EDGE_COLOR;
use crate::export::rendering::edge_style::edge_label;
use crate::export::rendering::exporter::Exporter;
use crate::export::rendering::exporter::Options;
use crate::export::rendering::structure::FolderEvent;
use crate::export::rendering::structure::Hierarchy;
use crate::export::rendering::structure::IndexedGraph;
use crate::model::DependencyKind;
use crate::model::FolderGraph;

pub struct Mermaid;

struct NodePresentation {
    index: usize,
    label: String,
}

struct GroupPresentation {
    node: NodePresentation,
    dependency: bool,
}

enum NodeEvent {
    Leaf(NodePresentation),
    EnterGroup(GroupPresentation),
    LeaveGroup,
}

struct EdgePresentation {
    source: usize,
    target: usize,
    label: String,
    color: Option<String>,
    test: bool,
    cycle: bool,
}

#[derive(Template)]
#[template(path = "export/mermaid/template.jinja2", escape = "none")]
struct MermaidTemplate {
    node_events: Vec<NodeEvent>,
    edges: Vec<EdgePresentation>,
}

impl MermaidTemplate {
    #[must_use]
    fn new(graph: &IndexedGraph<'_>, hierarchy: Option<&Hierarchy<'_>>) -> Self {
        let node_events = hierarchy.map_or_else(
            || graph.nodes().iter().map(|(label, &index)| {
                NodeEvent::Leaf(NodePresentation { index, label: escaped_label(label) })
            }).collect(),
            |hierarchy| hierarchy.events().map(|event| match event {
                FolderEvent::Enter(folder) => {
                    let node = NodePresentation { index: folder.index(), label: escaped_label(folder.display_label()) };
                    if folder.children().is_empty() {
                        NodeEvent::Leaf(node)
                    } else {
                        NodeEvent::EnterGroup(GroupPresentation { node, dependency: folder.dependency() })
                    }
                }
                FolderEvent::Leave => NodeEvent::LeaveGroup,
            }).collect(),
        );
        let edges = graph.edges().iter().map(|edge| {
            let test = edge.kind() == DependencyKind::Test;
            let cycle_color = edge.cycle_group().map(|group| group_color(group, graph.cycle_group_count()));
            EdgePresentation {
                source: edge.source(),
                target: edge.target(),
                label: edge_label(edge.kind(), edge.weight()),
                color: cycle_color.or_else(|| test.then(|| TEST_EDGE_COLOR.to_owned())),
                test,
                cycle: edge.cycle_group().is_some(),
            }
        }).collect();
        Self { node_events, edges }
    }
}

impl Exporter for Mermaid {
    fn write(&self, graph: &FolderGraph, output: &mut dyn Write) -> Result<()> {
        self.write_with_options(graph, &Options::default(), output)
    }

    fn write_grouped(&self, graph: &FolderGraph, output: &mut dyn Write) -> Result<()> {
        self.write_with_options(graph, &Options::builder().group_folders(true).build(), output)
    }

    fn write_with_options(&self, graph: &FolderGraph, options: &Options<'_>, output: &mut dyn Write) -> Result<()> {
        let indexed_graph = IndexedGraph::with_cycles(graph, options.cycles())?;
        let hierarchy = if options.group_folders() { Some(Hierarchy::new(&indexed_graph)?) } else { None };
        let template = MermaidTemplate::new(&indexed_graph, hierarchy.as_ref());
        template.write_into(output)?;
        Ok(())
    }
}

#[must_use]
fn escaped_label(label: &str) -> String {
    let mut escaped = String::with_capacity(label.len());
    for character in label.chars() {
        if character.is_alphanumeric() || matches!(character, ' ' | '/' | '.' | '_' | '-' | ':') {
            escaped.push(character);
        } else {
            escaped.push('#');
            escaped.push_str(&u32::from(character).to_string());
            escaped.push(';');
        }
    }
    escaped
}

#[cfg(test)]
#[expect(clippy::panic_in_result_fn, reason = "Tests use assertions for expectations while returning Result for fallible setup.")]
mod tests {
    use anyhow::Result;
    use pretty_assertions::assert_eq;

    use super::Mermaid;
    use crate::export::rendering::exporter::Exporter as _;
    use crate::export::rendering::grouping_fixture::hierarchy_fixture;
    use crate::export::rendering::render;
    use crate::export::rendering::render_grouped;
    use crate::export::rendering::serialization_fixture::graph_fixture;

    #[test]
    fn test_mermaid_punctuation_escaping() -> Result<()> {
        let fixture = graph_fixture()?;
        let system_under_test = Mermaid;

        let output = render(&system_under_test, &fixture)?;

        assert_eq!(
            output.as_str(),
            "flowchart LR\n    n0[\"src/a#34;#60;#38;#92;#35;#96;#10;#13;\u{43f}\u{430}\u{43f}\u{43a}\u{430}\"]\n    \
             n1[\"src/b\"]\n    n2[\"src/isolated\"]\n    n0 -->|7| n1\n",
            "Mermaid punctuation escaping or graph structure is incorrect"
        );
        Ok(())
    }

    #[test]
    fn test_mermaid_nested_subgraphs_and_original_dependency_endpoints() -> Result<()> {
        let fixture = hierarchy_fixture()?;
        let system_under_test = Mermaid;
        let mut flat_output = Vec::new();

        system_under_test.write(&fixture, &mut flat_output)?;
        let grouped_output = render_grouped(&system_under_test, &fixture)?;
        let flat_output = String::from_utf8(flat_output)?;
        let flat_edges: Vec<_> = flat_output.lines().filter(|line| line.contains("-->")).collect();
        let grouped_edges: Vec<_> = grouped_output.lines().filter(|line| line.contains("-->")).collect();

        assert_eq!(flat_edges, grouped_edges, "grouping changed Mermaid dependency endpoints or weights");
        assert!(
            grouped_output.starts_with(concat!(
                "flowchart LR\n",
                "    n0[\".\"]\n",
                "    subgraph g1[\"src\"]\n",
                "    n1[\"src\"]\n",
                "    subgraph g2[\"src/api\"]\n",
                "    n2[\"src/api\"]\n",
                "    n3[\"src/api/http\"]\n",
                "    end\n",
                "    n4[\"src/storage\"]\n",
                "    end\n",
                "    n5[\"tests\"]\n",
            )),
            "Mermaid folder hierarchy is incorrect: {grouped_output}"
        );
        Ok(())
    }
}
