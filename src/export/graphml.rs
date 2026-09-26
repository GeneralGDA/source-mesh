use std::io::Write;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;

use super::edge_style::CYCLE_EDGE_WIDTH;
use super::edge_style::TEST_EDGE_COLOR;
use super::edge_style::edge_label;
use super::edge_style::kind_name;
use super::exporter::Exporter;
use super::exporter::Options;
use super::structure::Folder;
use super::structure::IndexedGraph;
use crate::cycle_presentation::group_color;
use crate::model::DependencyKind;
use crate::model::FolderGraph;

pub struct GraphMl;

struct Position {
    horizontal: usize,
    vertical: usize,
}

struct NodeVisual<'label> {
    index: usize,
    label: &'label str,
    display_label: &'label str,
    position: Position,
    width: usize,
    height: usize,
}

impl<'label> NodeVisual<'label> {
    #[must_use]
    fn new(index: usize, label: &'label str, position: Position) -> Self {
        Self {
            index,
            label,
            display_label: label,
            position,
            width: label.lines().map(|line| line.chars().count()).max().unwrap_or(0) * 8 + 40,
            height: label.lines().count().max(1) * 20 + 20,
        }
    }

    fn write_leaf(&self, output: &mut dyn Write) -> Result<()> {
        let index = self.index;
        let label = escaped_label(self.label);
        let display_label = escaped_label(self.display_label);
        let horizontal_position = self.position.horizontal;
        let vertical_position = self.position.vertical;
        let width = self.width;
        let height = self.height;
        writeln!(
            output,
            r##"    <node id="n{index}">
      <data key="label">{label}</data>
      <data key="ng">
        <y:ShapeNode>
          <y:Geometry x="{horizontal_position}" y="{vertical_position}" width="{width}" height="{height}"/>
          <y:Fill color="#E8F0FE" transparent="false"/>
          <y:BorderStyle color="#47699B" type="line" width="1.0"/>
          <y:NodeLabel modelName="internal" modelPosition="c" xml:space="preserve">{display_label}</y:NodeLabel>
          <y:Shape type="roundrectangle"/>
        </y:ShapeNode>
      </data>
    </node>"##
        )?;
        Ok(())
    }

    fn write_group_start(&self, output: &mut dyn Write) -> Result<()> {
        let index = self.index;
        let label = escaped_label(self.label);
        let display_label = escaped_label(self.display_label);
        let horizontal_position = self.position.horizontal;
        let vertical_position = self.position.vertical;
        writeln!(
            output,
            r#"    <node id="n{index}" yfiles.foldertype="group">
      <data key="label">{label}</data>
      <data key="ng">
        <y:ProxyAutoBoundsNode>
          <y:Realizers active="0">"#
        )?;
        for closed in [false, true] {
            let (width, height) = if closed { (160, 50) } else { (self.width, self.height) };
            writeln!(
                output,
                r##"            <y:GroupNode>
              <y:Geometry x="{horizontal_position}" y="{vertical_position}" width="{width}" height="{height}"/>
              <y:Fill color="#F5F7FB" transparent="false"/>
              <y:BorderStyle color="#47699B" type="line" width="1.0"/>
              <y:NodeLabel modelName="internal" modelPosition="t" fontStyle="bold" backgroundColor="#DCE6F5" autoSizePolicy="node_width" xml:space="preserve">{display_label}</y:NodeLabel>
              <y:Shape type="roundrectangle"/>
              <y:State closed="{closed}" innerGraphDisplayEnabled="false"/>
              <y:Insets top="20" bottom="20" left="20" right="20"/>
              <y:BorderInsets top="0" bottom="0" left="0" right="0"/>
            </y:GroupNode>"##
            )?;
        }
        writeln!(
            output,
            r#"          </y:Realizers>
        </y:ProxyAutoBoundsNode>
      </data>
      <graph id="n{index}:" edgedefault="directed">"#
        )?;
        Ok(())
    }
}

struct LayoutNode<'label> {
    visual: NodeVisual<'label>,
    children: Vec<Self>,
}

impl<'label> LayoutNode<'label> {
    #[must_use]
    fn new(folder: &Folder<'label>, position: Position) -> Self {
        let mut visual = NodeVisual::new(folder.index(), folder.display_label(), position);
        visual.label = folder.label();
        let mut children = Vec::new();
        if !folder.children().is_empty() {
            let mut child_vertical_position = visual.position.vertical + visual.height;
            for child in folder.children() {
                let child = Self::new(child, Position {
                    horizontal: visual.position.horizontal + 20,
                    vertical: child_vertical_position,
                });
                visual.width = visual.width.max(child.visual.width + 40);
                child_vertical_position += child.visual.height + 20;
                children.push(child);
            }
            visual.height = child_vertical_position - visual.position.vertical;
        }
        Self { visual, children }
    }

    fn write(&self, output: &mut dyn Write) -> Result<()> {
        if self.children.is_empty() {
            self.visual.write_leaf(output)?;
        } else {
            self.visual.write_group_start(output)?;
            for child in &self.children {
                child.write(output)?;
            }
            writeln!(output, "      </graph>\n    </node>")?;
        }
        Ok(())
    }
}

#[must_use]
fn escaped_label(label: &str) -> String {
    // Preserve CR as an entity: XML readers normalize literal CR to LF.
    quick_xml::escape::escape(label).replace('\r', "&#13;")
}

fn validate_graph(graph: &FolderGraph) -> Result<()> {
    ensure!(graph.nodes().iter().all(|node| node.label().chars().all(|character| matches!(character,
        '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}'
    ))), "a folder label contains a character forbidden in XML 1.0");
    for weight in graph.edges().values() {
        i64::try_from(weight.value()).context("edge weight exceeds GraphML's signed long range")?;
    }
    Ok(())
}

fn write_header(output: &mut dyn Write, has_cycle_groups: bool) -> Result<()> {
    writeln!(
        output,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<graphml xmlns="http://graphml.graphdrawing.org/xmlns"
         xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
         xmlns:y="http://www.yworks.com/xml/graphml"
         xsi:schemaLocation="http://graphml.graphdrawing.org/xmlns http://www.yworks.com/xml/schema/graphml/1.1/ygraphml.xsd">
  <key id="label" for="node" attr.name="label" attr.type="string"/>
  <key id="weight" for="edge" attr.name="dependencies" attr.type="long"/>
  <key id="kind" for="edge" attr.name="dependency_kind" attr.type="string"/>"#
    )?;
    if has_cycle_groups {
        writeln!(output, r#"  <key id="cycle_group" for="edge" attr.name="cycle_group" attr.type="int"/>"#)?;
    }
    writeln!(
        output,
        r#"  <key id="ng" for="node" yfiles.type="nodegraphics"/>
  <key id="eg" for="edge" yfiles.type="edgegraphics"/>
  <graph id="folders" edgedefault="directed">"#
    )?;
    Ok(())
}

fn write_edges_and_close(graph: &IndexedGraph<'_>, output: &mut dyn Write) -> Result<()> {
    for (index, edge) in graph.edges().iter().enumerate() {
        let source = edge.source();
        let target = edge.target();
        let weight = edge.weight().value();
        let label = edge_label(edge.kind(), edge.weight());
        let kind = kind_name(edge.kind());
        let (color, line_style) = match edge.kind() {
            DependencyKind::Production => ("#47699B", "line"),
            DependencyKind::Test => (TEST_EDGE_COLOR, "dashed"),
        };
        let cycle_color = edge.cycle_group().map(|group| group_color(group, graph.cycle_group_count()));
        let color = cycle_color.as_deref().unwrap_or(color);
        let width = if edge.cycle_group().is_some() { CYCLE_EDGE_WIDTH } else { "1.0" };
        writeln!(
            output,
            r#"    <edge id="e{index}" source="n{source}" target="n{target}">
      <data key="weight">{weight}</data>
      <data key="kind">{kind}</data>"#
        )?;
        if let Some(group) = edge.cycle_group() {
            writeln!(output, "      <data key=\"cycle_group\">{}</data>", group.number())?;
        }
        writeln!(
            output,
            r#"      <data key="eg">
        <y:PolyLineEdge>
          <y:LineStyle color="{color}" type="{line_style}" width="{width}"/>
          <y:Arrows source="none" target="standard"/>
          <y:EdgeLabel>{label}</y:EdgeLabel>
          <y:BendStyle smoothed="false"/>
        </y:PolyLineEdge>
      </data>
    </edge>"#
        )?;
    }
    writeln!(output, "  </graph>\n</graphml>")?;
    Ok(())
}

impl Exporter for GraphMl {
    fn write(&self, graph: &FolderGraph, output: &mut dyn Write) -> Result<()> {
        self.write_with_options(graph, &Options::new(false, None), output)
    }

    fn write_grouped(&self, graph: &FolderGraph, output: &mut dyn Write) -> Result<()> {
        self.write_with_options(graph, &Options::new(true, None), output)
    }

    fn write_with_options(&self, graph: &FolderGraph, options: &Options<'_>, output: &mut dyn Write) -> Result<()> {
        validate_graph(graph)?;
        let indexed_graph = IndexedGraph::with_cycles(graph, options.cycles())?;
        let hierarchy = if options.group_folders() { Folder::hierarchy(&indexed_graph)? } else { Vec::new() };
        write_header(output, indexed_graph.cycle_group_count() != 0)?;
        let mut vertical_position = 0;
        if options.group_folders() {
            for folder in hierarchy {
                let layout = LayoutNode::new(&folder, Position {
                    horizontal: 0,
                    vertical: vertical_position,
                });
                layout.write(output)?;
                vertical_position += layout.visual.height + 40;
            }
        } else {
            for (label, &index) in indexed_graph.nodes() {
                let visual = NodeVisual::new(index, label, Position {
                    horizontal: 0,
                    vertical: vertical_position,
                });
                visual.write_leaf(output)?;
                vertical_position += visual.height + 40;
            }
        }
        write_edges_and_close(&indexed_graph, output)
    }
}
