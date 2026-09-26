use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::io::Write;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::bail;
use graphviz_rust::dot_structures::EdgeTy;
use graphviz_rust::dot_structures::Graph;
use graphviz_rust::dot_structures::Id;
use graphviz_rust::dot_structures::Stmt as Statement;
use graphviz_rust::dot_structures::Vertex;
use rstest::rstest;

use pretty_assertions::assert_eq;
use crate::export::Exporter;
use crate::export::Format;
use crate::export::GraphMl;
use crate::model::DependencyCount;
use crate::model::DependencyKind;
use crate::model::FolderEdge;
use crate::model::FolderGraph;
use crate::model::FolderNode;

fn graph_fixture() -> Result<FolderGraph> {
    const PUNCTUATION_LABEL: &str = "src/a\"<&\\#`\n\r\u{43f}\u{430}\u{43f}\u{43a}\u{430}";
    FolderGraph::new(
        [PUNCTUATION_LABEL, "src/b", "src/isolated"]
            .map(String::from)
            .map(FolderNode::new)
            .into(),
        BTreeMap::from([(
            FolderEdge::new(FolderNode::new(PUNCTUATION_LABEL.into()), FolderNode::new("src/b".into()), DependencyKind::Production),
            DependencyCount::new(7),
        )]),
    )
}

fn render(exporter: &dyn Exporter, graph: &FolderGraph) -> Result<String> {
    let mut output = Vec::new();
    exporter.write(graph, &mut output)?;
    Ok(String::from_utf8(output)?)
}

#[test]
fn test_dot_labels_direction_weights_and_isolated_nodes() -> Result<()> {
    let fixture = graph_fixture()?;
    let system_under_test = Format::Dot.exporter();

    let output = render(system_under_test.as_ref(), &fixture)?;
    let Graph::DiGraph { strict, stmts: statements, .. } =
        graphviz_rust::parse(&output).map_err(anyhow::Error::msg)? else {
        bail!("expected directed graph");
    };
    let nodes: Vec<_> = statements
        .iter()
        .filter_map(|statement| {
            if let Statement::Node(ref node) = *statement { Some(node) } else { None }
        })
        .collect();
    let edges: Vec<_> = statements
        .iter()
        .filter_map(|statement| {
            if let Statement::Edge(ref edge) = *statement { Some(edge) } else { None }
        })
        .collect();
    let first_node = nodes.first().context("first DOT node is missing")?;
    let isolated_node = nodes.last().context("isolated DOT node is missing")?;
    let edge = edges.first().context("DOT edge is missing")?;
    let EdgeTy::Pair(Vertex::N(ref source), Vertex::N(ref target)) = edge.ty else {
        bail!("expected a simple directed edge");
    };

    assert!(strict, "DOT graph must be strict");
    assert_eq!(nodes.len(), 3, "Expected three DOT nodes");
    assert_eq!(
        &first_node.attributes.first().context("DOT label is missing")?.1,
        &Id::Escaped("\"src/a\\\"<&\\\\#`\\n\\r\u{43f}\u{430}\u{43f}\u{43a}\u{430}\"".into()),
        "DOT punctuation label was not preserved or escaped correctly"
    );
    assert_eq!(
        &isolated_node.attributes.first().context("isolated DOT label is missing")?.1,
        &Id::Escaped("\"src/isolated\"".into()),
        "DOT isolated node label was not preserved"
    );
    assert_eq!(edges.len(), 1, "Expected one DOT edge");
    assert_eq!(&source.0, &Id::Plain("n0".into()), "DOT edge source is incorrect");
    assert_eq!(&target.0, &Id::Plain("n1".into()), "DOT edge target is incorrect");
    assert_eq!(
        &edge.attributes.first().context("DOT edge weight is missing")?.1,
        &Id::Escaped("\"7\"".into()),
        "DOT edge weight is incorrect"
    );
    Ok(())
}

#[test]
fn test_graphml_text_yed_graphics_and_references() -> Result<()> {
    const GRAPHML_NAMESPACE: &str = "http://graphml.graphdrawing.org/xmlns";
    const YWORKS_NAMESPACE: &str = "http://www.yworks.com/xml/graphml";
    let fixture = graph_fixture()?;
    let system_under_test = Format::Graphml.exporter();

    let output = render(system_under_test.as_ref(), &fixture)?;
    let xml = roxmltree::Document::parse(&output)?;
    let graph_element = xml
        .descendants()
        .find(|node| node.has_tag_name((GRAPHML_NAMESPACE, "graph")))
        .context("GraphML graph element is missing")?;
    let nodes: Vec<_> = graph_element
        .children()
        .filter(|node| node.has_tag_name((GRAPHML_NAMESPACE, "node")))
        .collect();
    let node_identifiers: BTreeSet<_> = nodes
        .iter()
        .map(|node| node.attribute("id").context("GraphML node identifier is missing"))
        .collect::<Result<_>>()?;
    let mut labels = BTreeSet::new();

    assert!(xml.root_element().has_tag_name((GRAPHML_NAMESPACE, "graphml")), "GraphML root or namespace is incorrect");
    assert_eq!(graph_element.attribute("edgedefault"), Some("directed"), "GraphML graph must be directed");
    assert_eq!(node_identifiers.len(), 3, "GraphML must contain three distinct node identifiers");
    for node in &nodes {
        let label = node
            .children()
            .find(|child| child.attribute("key") == Some("label"))
            .context("GraphML label element is missing")?
            .text()
            .context("GraphML label text is missing")?;
        let visual = node
            .descendants()
            .find(|child| child.has_tag_name((YWORKS_NAMESPACE, "NodeLabel")))
            .context("yEd visual label is missing")?;

        assert_eq!(visual.text(), Some(label), "yEd visual label differs from GraphML label");
        assert!(node.descendants().any(|child| child.has_tag_name((YWORKS_NAMESPACE, "ShapeNode"))), "yEd node graphics are missing");
        labels.insert(FolderNode::new(label.to_owned()));
    }
    assert_eq!(&labels, fixture.nodes(), "GraphML labels differ from folder names");
    let edges: Vec<_> = graph_element
        .children()
        .filter(|node| node.has_tag_name((GRAPHML_NAMESPACE, "edge")))
        .collect();
    let edge = edges.first().context("GraphML edge is missing")?;
    let weight = edge
        .children()
        .find(|node| node.attribute("key") == Some("weight"))
        .context("GraphML edge weight is missing")?;
    let arrows = edge
        .descendants()
        .find(|node| node.has_tag_name((YWORKS_NAMESPACE, "Arrows")))
        .context("yEd arrows are missing")?;
    let source = edge.attribute("source").context("GraphML source is missing")?;
    let target = edge.attribute("target").context("GraphML target is missing")?;

    assert_eq!(edges.len(), 1, "Expected one GraphML edge");
    assert_eq!(source, "n0", "GraphML edge source is incorrect");
    assert_eq!(target, "n1", "GraphML edge target is incorrect");
    assert!(node_identifiers.contains(source), "GraphML edge source has no corresponding node");
    assert!(node_identifiers.contains(target), "GraphML edge target has no corresponding node");
    assert_eq!(weight.text(), Some("7"), "GraphML edge weight is incorrect");
    assert_eq!(arrows.attribute("source"), Some("none"), "yEd source must have no arrow");
    assert_eq!(arrows.attribute("target"), Some("standard"), "yEd target must have a standard arrow");
    Ok(())
}

#[test]
fn test_mermaid_punctuation_escaping() -> Result<()> {
    let fixture = graph_fixture()?;
    let system_under_test = Format::Mermaid.exporter();

    let output = render(system_under_test.as_ref(), &fixture)?;

    assert_eq!(
        output.as_str(),
        "flowchart LR\n    n0[\"src/a#34;#60;#38;#92;#35;#96;#10;#13;\u{43f}\u{430}\u{43f}\u{43a}\u{430}\"]\n    n1[\"src/b\"]\n    n2[\"src/isolated\"]\n    n0 -->|7| n1\n",
        "Mermaid punctuation escaping or graph structure is incorrect"
    );
    Ok(())
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
#[case(Format::Graphml)]
fn test_export_determinism(#[case] format: Format) -> Result<()> {
    let fixture = graph_fixture()?;
    let system_under_test = format.exporter();

    let first_output = render(system_under_test.as_ref(), &fixture)?;
    let second_output = render(system_under_test.as_ref(), &fixture)?;

    assert_eq!(first_output, second_output, "Repeated export changed output for the same graph");
    Ok(())
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
#[case(Format::Graphml)]
fn test_empty_graph_serialization(#[case] format: Format) -> Result<()> {
    let fixture = FolderGraph::default();
    let system_under_test = format.exporter();

    let output = render(system_under_test.as_ref(), &fixture)?;

    match format {
        Format::Dot => {
            graphviz_rust::parse(&output).map_err(anyhow::Error::msg)?;
        }
        Format::Mermaid => assert_eq!(output.as_str(), "flowchart LR\n", "Empty Mermaid graph must contain only its header"),
        Format::Graphml => {
            roxmltree::Document::parse(&output)?;
        }
    }
    Ok(())
}

#[test]
fn test_graphml_xml_character_validation_before_writing() -> Result<()> {
    let fixture = FolderGraph::new(
        BTreeSet::from([FolderNode::new("bad\u{1}name".into())]),
        BTreeMap::new(),
    )?;
    let mut output = Vec::new();
    let system_under_test = GraphMl;

    let result = system_under_test.write(&fixture, &mut output);

    assert!(result.is_err(), "GraphML accepted an XML-forbidden character");
    assert!(output.is_empty(), "GraphML wrote output before validating XML characters");
    Ok(())
}

#[test]
fn test_graphml_signed_long_weight_validation_before_writing() -> Result<()> {
    if i64::try_from(usize::MAX).is_ok() {
        return Ok(());
    }
    let fixture = FolderGraph::new(
        BTreeSet::from([FolderNode::new("source".into()), FolderNode::new("target".into())]),
        BTreeMap::from([(
            FolderEdge::new(FolderNode::new("source".into()), FolderNode::new("target".into()), DependencyKind::Production),
            DependencyCount::new(usize::MAX),
        )]),
    )?;
    let mut output = Vec::new();
    let system_under_test = GraphMl;

    let result = system_under_test.write(&fixture, &mut output);

    assert!(result.is_err(), "GraphML accepted a weight outside the signed long range");
    assert!(output.is_empty(), "GraphML wrote output before validating its signed long range");
    Ok(())
}

struct BrokenWriter;

impl Write for BrokenWriter {
    fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
        Err(ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        unreachable!("BrokenWriter::flush must not be called by an exporter");
    }
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
#[case(Format::Graphml)]
fn test_export_write_error_propagation(#[case] format: Format) -> Result<()> {
    let fixture = graph_fixture()?;
    let system_under_test = format.exporter();

    let result = system_under_test.write(&fixture, &mut BrokenWriter);

    assert!(result.is_err(), "Exporter did not propagate its writer's failure");
    Ok(())
}
