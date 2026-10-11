use std::io::Write;

use anyhow::Result;

use super::edge_style::CYCLE_EDGE_WIDTH;
use super::edge_style::TEST_EDGE_COLOR;
use super::edge_style::edge_label;
use super::exporter::Exporter;
use super::exporter::Options;
use super::structure::Folder;
use super::structure::IndexedGraph;
use crate::cycle_presentation::group_color;
use crate::model::DependencyKind;
use crate::model::FolderGraph;

pub struct Mermaid;

impl Exporter for Mermaid {
    fn write(&self, graph: &FolderGraph, output: &mut dyn Write) -> Result<()> {
        self.write_with_options(graph, &Options::new(false, None), output)
    }

    fn write_grouped(&self, graph: &FolderGraph, output: &mut dyn Write) -> Result<()> {
        self.write_with_options(graph, &Options::new(true, None), output)
    }

    fn write_with_options(&self, graph: &FolderGraph, options: &Options<'_>, output: &mut dyn Write) -> Result<()> {
        let indexed_graph = IndexedGraph::with_cycles(graph, options.cycles())?;
        let hierarchy = if options.group_folders() { Folder::hierarchy(&indexed_graph)? } else { Vec::new() };
        writeln!(output, "flowchart LR")?;
        if options.group_folders() {
            for folder in hierarchy {
                write_folder(&folder, output)?;
            }
        } else {
            for (label, &index) in indexed_graph.nodes() {
                write_node(label, index, output)?;
            }
        }
        write_edges(&indexed_graph, output)
    }
}

fn write_escaped_label(label: &str, output: &mut dyn Write) -> Result<()> {
    for character in label.chars() {
        if character.is_alphanumeric() || matches!(character, ' ' | '/' | '.' | '_' | '-' | ':') {
            write!(output, "{character}")?;
        } else {
            write!(output, "#{};", u32::from(character))?;
        }
    }
    Ok(())
}

fn write_node(label: &str, index: usize, output: &mut dyn Write) -> Result<()> {
    write!(output, "    n{index}[\"")?;
    write_escaped_label(label, output)?;
    writeln!(output, "\"]")?;
    Ok(())
}

fn write_folder(folder: &Folder<'_>, output: &mut dyn Write) -> Result<()> {
    if folder.children().is_empty() {
        return write_node(folder.display_label(), folder.index(), output);
    }
    write!(output, "    subgraph g{}[\"", folder.index())?;
    write_escaped_label(folder.display_label(), output)?;
    writeln!(output, "\"]")?;
    if folder.dependency() {
        write_node(folder.display_label(), folder.index(), output)?;
    }
    for child in folder.children() {
        write_folder(child, output)?;
    }
    writeln!(output, "    end")?;
    Ok(())
}

fn write_edges(graph: &IndexedGraph<'_>, output: &mut dyn Write) -> Result<()> {
    for (index, edge) in graph.edges().iter().enumerate() {
        let arrow = match edge.kind() {
            DependencyKind::Production => "-->",
            DependencyKind::Test => "-.->",
        };
        writeln!(output, "    n{} {arrow}|{}| n{}", edge.source(), edge_label(edge.kind(), edge.weight()), edge.target())?;
        if let Some(group) = edge.cycle_group() {
            let color = group_color(group, graph.cycle_group_count());
            writeln!(output, "    linkStyle {index} stroke:{color},color:{color},stroke-width:{CYCLE_EDGE_WIDTH}px")?;
        }
        if edge.cycle_group().is_none() && edge.kind() == DependencyKind::Test {
            writeln!(output, "    linkStyle {index} stroke:{TEST_EDGE_COLOR},color:{TEST_EDGE_COLOR}")?;
        }
    }
    Ok(())
}
