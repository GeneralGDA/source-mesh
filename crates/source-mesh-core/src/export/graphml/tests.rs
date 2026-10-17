use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::bail;
use roxmltree::Document;
use roxmltree::Node;
use rstest::rstest;

use super::GraphMl;
use crate::SourcePath;
use crate::export::graphml::GRAPHML_NAMESPACE;
use crate::export::graphml::YWORKS_NAMESPACE;
use crate::export::rendering::edge_style::kind_name;
use crate::export::rendering::exporter::Exporter as _;
use crate::export::rendering::serialization_fixture::graph_fixture as serialization_fixture;
use crate::export::rendering::serialization_fixture::parallel_dependency_fixture;
use crate::export::rendering::serialization_fixture::render;
use crate::model::DependencyCount;
use crate::model::DependencyKind;
use crate::model::FileGraph;
use crate::model::FileNode;
use crate::model::FolderEdge;
use crate::model::FolderGraph;
use crate::model::FolderNode;
use pretty_assertions::assert_eq;

fn graph_fixture() -> Result<FolderGraph> {
    FolderGraph::new(
        ["src", "src/api/http", "src/domain", "tests/unit"].map(String::from).map(FolderNode::new).into(),
        BTreeMap::from([
            (
                FolderEdge::new(FolderNode::new("src".into()), FolderNode::new("src".into()), DependencyKind::Production),
                DependencyCount::new(2),
            ),
            (
                FolderEdge::new(
                    FolderNode::new("src".into()),
                    FolderNode::new("src/api/http".into()),
                    DependencyKind::Production,
                ),
                DependencyCount::new(3),
            ),
            (
                FolderEdge::new(
                    FolderNode::new("src/api/http".into()),
                    FolderNode::new("src".into()),
                    DependencyKind::Production,
                ),
                DependencyCount::new(4),
            ),
            (
                FolderEdge::new(
                    FolderNode::new("src/api/http".into()),
                    FolderNode::new("tests/unit".into()),
                    DependencyKind::Production,
                ),
                DependencyCount::new(7),
            ),
            (
                FolderEdge::new(
                    FolderNode::new("src/domain".into()),
                    FolderNode::new("src/api/http".into()),
                    DependencyKind::Production,
                ),
                DependencyCount::new(5),
            ),
        ]),
    )
}

#[must_use]
fn node_label<'document>(node: Node<'document, '_>) -> Option<&'document str> {
    node.children().find(|child| child.attribute("key") == Some("label"))?.text()
}

fn dependencies(document: &Document<'_>) -> Result<BTreeMap<FolderEdge, DependencyCount>> {
    let labels: BTreeMap<_, _> = document.descendants().filter(|node| node.has_tag_name("node")).map(|node| {
        Ok((
            node.attribute("id").context("GraphML node has no identifier")?,
            node_label(node).context("GraphML node has no label")?,
        ))
    }).collect::<Result<_>>()?;
    let edges = document.descendants().filter(|node| node.has_tag_name("edge"));
    edges.map(|edge| {
        let source = edge.attribute("source").context("GraphML edge has no source")?;
        let target = edge.attribute("target").context("GraphML edge has no target")?;
        let weight = edge.children().find(|child| child.attribute("key") == Some("weight")).and_then(|child| child.text());
        let weight = weight.context("GraphML edge has no weight")?.parse()?;
        let kind = edge.children().find(|child| child.attribute("key") == Some("kind")).and_then(|child| child.text());
        let kind = match kind.context("GraphML edge has no dependency kind")? {
            kind if kind == kind_name(DependencyKind::Production) => DependencyKind::Production,
            kind if kind == kind_name(DependencyKind::Test) => DependencyKind::Test,
            kind => bail!("Unknown GraphML dependency kind: {kind}"),
        };
        Ok((
            FolderEdge::new(
                FolderNode::new((*labels.get(source).context("GraphML source has no matching node")?).to_owned()),
                FolderNode::new((*labels.get(target).context("GraphML target has no matching node")?).to_owned()),
                kind,
            ),
            DependencyCount::new(weight),
        ))
    }).collect()
}

fn geometry(node: Node<'_, '_>) -> Result<[usize; 4]> {
    let graphics = node.children().find(|child| child.attribute("key") == Some("ng"));
    let geometry = graphics.and_then(|graphics| graphics.descendants().find(|child| child.has_tag_name("Geometry")));
    let geometry = geometry.context("Folder has no yEd geometry")?;
    let coordinate = |attribute| -> Result<usize> {
        let value = geometry.attribute(attribute).context("yEd geometry attribute is missing")?;
        Ok(value.parse()?)
    };
    Ok([coordinate("x")?, coordinate("y")?, coordinate("width")?, coordinate("height")?])
}

#[test]
fn test_nested_folder_groups_and_missing_ancestors() -> Result<()> {
    let fixture = graph_fixture()?;
    let system_under_test = GraphMl;
    let mut output = Vec::new();

    system_under_test.write_grouped(&fixture, &mut output)?;
    let output = String::from_utf8(output)?;
    let document = Document::parse(&output)?;
    let parents: BTreeMap<_, _> = document.descendants().filter(|node| node.has_tag_name("node")).map(|node| {
        Ok((
            node_label(node).context("GraphML folder has no label")?,
            node.parent().and_then(|graph| graph.parent()).and_then(node_label).unwrap_or(""),
        ))
    }).collect::<Result<_>>()?;
    let expected_parents = BTreeMap::from([
        ("src", ""),
        ("src/api", "src"),
        ("src/api/http", "src/api"),
        ("src/domain", "src"),
        ("tests", ""),
        ("tests/unit", "tests"),
    ]);

    assert_eq!(parents, expected_parents, "GraphML hierarchy differs from physical folder parents");
    for node in document.descendants().filter(|node| node.has_tag_name("node")) {
        let nested_graph = node.children().find(|child| child.has_tag_name("graph"));
        if let Some(nested_graph) = nested_graph {
            let graphics = node.children().find(|child| child.attribute("key") == Some("ng"));
            let realizers = graphics.and_then(|graphics| graphics.descendants().find(|child| child.has_tag_name("Realizers")));
            let realizers = realizers.context("Group has no yEd realizers")?;
            let states = realizers.descendants().filter(|child| child.has_tag_name("State"));
            let states: Vec<_> = states.filter_map(|state| state.attribute("closed")).collect();
            let [parent_left, parent_top, parent_width, parent_height] = geometry(node)?;

            assert_eq!(node.attribute("yfiles.foldertype"), Some("group"), "Parent folder is not a yEd group");
            assert_eq!(realizers.attribute("active"), Some("0"), "Group must initially be expanded");
            assert_eq!(states, ["false", "true"], "Group must support expanded and collapsed views");
            for child in nested_graph.children().filter(|child| child.has_tag_name("node")) {
                let [child_left, child_top, child_width, child_height] = geometry(child)?;

                assert!(child_left > parent_left && child_top > parent_top, "Child folder starts outside its group");
                assert!(child_left + child_width < parent_left + parent_width, "Child folder exceeds group width");
                assert!(child_top + child_height < parent_top + parent_height, "Child folder exceeds group height");
            }
        } else {
            assert!(node.attribute("yfiles.foldertype").is_none(), "Leaf folder was marked as a group");
            assert!(node.descendants().any(|child| child.has_tag_name("ShapeNode")), "Leaf folder has no yEd shape");
        }
    }
    Ok(())
}

#[test]
fn test_nested_layout_and_serialization_order() -> Result<()> {
    let fixture = graph_fixture()?;
    let system_under_test = GraphMl;
    let mut output = Vec::new();

    system_under_test.write_grouped(&fixture, &mut output)?;
    let output = String::from_utf8(output)?;
    let document = Document::parse(&output)?;
    let nodes: Vec<_> = document.descendants().filter(|node| node.has_tag_name("node")).map(|node| {
        Ok((
            node.attribute("id").context("Folder identifier is missing")?,
            node_label(node).context("Folder label is missing")?,
            geometry(node)?,
        ))
    }).collect::<Result<_>>()?;

    assert_eq!(
        nodes,
        [
            ("n0", "src", [0, 0, 216, 220]),
            ("n5", "src/api", [20, 40, 176, 100]),
            ("n1", "src/api/http", [40, 80, 136, 40]),
            ("n2", "src/domain", [20, 160, 120, 40]),
            ("n6", "tests", [0, 260, 160, 100]),
            ("n3", "tests/unit", [20, 300, 120, 40]),
        ],
        "Iterative layout changed depth-first ordering, identifiers, or exact geometry"
    );
    Ok(())
}

#[test]
fn test_group_endpoint_cross_group_self_dependencies_and_weights() -> Result<()> {
    let fixture = graph_fixture()?;
    let system_under_test = GraphMl;
    let mut flat_output = Vec::new();
    let mut grouped_output = Vec::new();

    system_under_test.write(&fixture, &mut flat_output)?;
    system_under_test.write_grouped(&fixture, &mut grouped_output)?;
    let flat_output = String::from_utf8(flat_output)?;
    let grouped_output = String::from_utf8(grouped_output)?;
    let flat_document = Document::parse(&flat_output)?;
    let grouped_document = Document::parse(&grouped_output)?;
    let grouped_dependencies = dependencies(&grouped_document)?;
    let flat_dependencies = dependencies(&flat_document)?;
    let flat_edges = flat_document.descendants().filter(|node| node.has_tag_name("edge"));
    let flat_edges = flat_edges.map(|edge| flat_output.get(edge.range()).context("Flat edge XML range is invalid"));
    let flat_edges: Vec<_> = flat_edges.collect::<Result<_>>()?;
    let grouped_edges = grouped_document.descendants().filter(|node| node.has_tag_name("edge"));
    let grouped_edges = grouped_edges.map(|edge| grouped_output.get(edge.range()).context("Grouped edge XML range is invalid"));
    let grouped_edges: Vec<_> = grouped_edges.collect::<Result<_>>()?;

    assert_eq!(&grouped_dependencies, fixture.edges(), "Grouping changed dependency endpoints or weights");
    assert_eq!(grouped_dependencies, flat_dependencies, "Grouping changed flat graph dependencies");
    assert_eq!(grouped_edges, flat_edges, "Grouping changed GraphML edge elements or their identifiers");
    for edge in grouped_document.descendants().filter(|node| node.has_tag_name("edge")) {
        let parent_identifier = edge.parent().and_then(|parent| parent.attribute("id"));
        assert_eq!(parent_identifier, Some("folders"), "Edge is not in a common ancestor graph");
    }
    Ok(())
}

#[rstest]
#[case(false)]
#[case(true)]
fn test_parallel_dependency_kinds_and_weights(#[case] grouped: bool) -> Result<()> {
    let fixture = parallel_dependency_fixture()?;
    let system_under_test = GraphMl;
    let mut output = Vec::new();

    if grouped {
        system_under_test.write_grouped(&fixture, &mut output)?;
    } else {
        system_under_test.write(&fixture, &mut output)?;
    }
    let output = String::from_utf8(output)?;
    let document = Document::parse(&output)?;
    let parsed_dependencies = dependencies(&document)?;
    let edges = document.descendants().filter(|node| node.has_tag_name("edge"));
    let identifiers: BTreeSet<_> = edges.filter_map(|edge| edge.attribute("id")).collect();

    assert_eq!(&parsed_dependencies, fixture.edges(), "Dependency kinds or their separate weights changed");
    assert_eq!(identifiers.len(), 2, "Parallel dependency kinds share an edge identifier");
    Ok(())
}

#[test]
fn test_common_path_labels_original_ancestors_and_top_level_layout() -> Result<()> {
    let labels = ["src", "src/engine", "src/engine/api", "src/engine/api/http", "src/engine/storage"];
    let fixture = FolderGraph::new(
        labels.map(String::from).map(FolderNode::new).into(),
        BTreeMap::from([
            (
                FolderEdge::new(
                    FolderNode::new("src/engine".into()),
                    FolderNode::new("src/engine/api".into()),
                    DependencyKind::Production,
                ),
                DependencyCount::new(2),
            ),
            (
                FolderEdge::new(
                    FolderNode::new("src/engine/api".into()),
                    FolderNode::new("src/engine/api/http".into()),
                    DependencyKind::Production,
                ),
                DependencyCount::new(3),
            ),
            (
                FolderEdge::new(
                    FolderNode::new("src/engine/api/http".into()),
                    FolderNode::new("src/engine/storage".into()),
                    DependencyKind::Production,
                ),
                DependencyCount::new(5),
            ),
        ]),
    )?;
    let system_under_test = GraphMl;
    let mut output = Vec::new();

    system_under_test.write_grouped(&fixture, &mut output)?;
    let output = String::from_utf8(output)?;
    let document = Document::parse(&output)?;
    let nodes: BTreeMap<_, _> = document.descendants().filter(|node| node.has_tag_name("node")).map(|node| {
        let label = node_label(node).context("GraphML folder has no canonical label")?;
        let identifier = node.attribute("id").context("GraphML folder has no identifier")?;
        let graphics = node.children().find(|child| child.attribute("key") == Some("ng"));
        let visual = graphics.and_then(|graphics| graphics.descendants().find(|child| child.has_tag_name("NodeLabel")));
        let visual = visual.and_then(|label_node| label_node.text()).context("GraphML folder has no visual label")?;
        let parent = node.parent().and_then(|graph| graph.parent()).and_then(node_label).unwrap_or("");
        Ok((label, (identifier, visual, parent)))
    }).collect::<Result<_>>()?;
    let expected = BTreeMap::from([
        ("src", ("n0", "src", "")),
        ("src/engine", ("n1", "src/engine", "")),
        ("src/engine/api", ("n2", "api", "")),
        ("src/engine/api/http", ("n3", "api/http", "src/engine/api")),
        ("src/engine/storage", ("n4", "storage", "")),
    ]);
    let grouped_dependencies = dependencies(&document)?;
    let graph = document.root_element().children().find(|node| node.has_tag_name("graph")).context("Top-level graph is missing")?;

    assert_eq!(nodes, expected, "Common-path removal changed node identifiers, canonical labels, visible labels, or hierarchy");
    assert_eq!(&grouped_dependencies, fixture.edges(), "Common-path removal changed dependencies or weights");
    let mut previous_bottom = None;
    for node in graph.children().filter(|node| node.has_tag_name("node")) {
        let [_, top, _, height] = geometry(node)?;
        if let Some(previous_bottom) = previous_bottom {
            assert!(top >= previous_bottom + 40, "Top-level folder geometry overlaps another folder");
        }
        previous_bottom = Some(top + height);
    }
    Ok(())
}

#[test]
fn test_group_and_leaf_label_escaping() -> Result<()> {
    const PARENT: &str = "a\"<&\\#`\n\r\u{43f}\u{430}\u{43f}\u{43a}\u{430}";
    let child = format!("{PARENT}/nested");
    let nodes = [PARENT.to_owned(), child.clone(), "tests".into()].map(FolderNode::new).into();
    let fixture = FolderGraph::new(nodes, BTreeMap::new())?;
    let system_under_test = GraphMl;
    let mut output = Vec::new();

    system_under_test.write_grouped(&fixture, &mut output)?;
    let output = String::from_utf8(output)?;
    let document = Document::parse(&output)?;
    let labels: BTreeSet<_> = document.descendants().filter(|node| node.has_tag_name("node")).filter_map(node_label).collect();
    let visuals = document.descendants().filter(|node| node.has_tag_name("NodeLabel"));
    let visuals: BTreeSet<_> = visuals.filter_map(|node| node.text()).collect();
    let expected = BTreeSet::from([PARENT, child.as_str(), "tests"]);

    assert_eq!(labels, expected, "GraphML grouping changed punctuation in folder labels");
    assert_eq!(visuals, expected, "yEd grouping changed punctuation in visual labels");
    Ok(())
}

#[rstest]
#[case(false)]
#[case(true)]
fn test_empty_and_depth_zero_grouping(#[case] include_files: bool) -> Result<()> {
    let files = if include_files {
        [FileNode::new(SourcePath::new(PathBuf::from_iter(["src", "api", "lib.rs"]))?)].into()
    } else {
        BTreeSet::new()
    };
    let fixture = FileGraph::new(files, BTreeSet::new())?.folders(Path::new("."), Some(0), false)?;
    let system_under_test = GraphMl;
    let mut flat_output = Vec::new();
    let mut grouped_output = Vec::new();

    system_under_test.write(&fixture, &mut flat_output)?;
    system_under_test.write_grouped(&fixture, &mut grouped_output)?;
    let document = Document::parse(std::str::from_utf8(&grouped_output)?)?;

    assert_eq!(flat_output, grouped_output, "Grouping changed a graph with no folder hierarchy");
    let node_count = document.descendants().filter(|node| node.has_tag_name("node")).count();
    assert_eq!(node_count, usize::from(include_files), "Grouping introduced an empty artificial group");
    Ok(())
}

#[test]
fn test_grouped_xml_character_validation_before_writing() -> Result<()> {
    let fixture = FolderGraph::new([FolderNode::new("bad\u{1}/nested".into())].into(), BTreeMap::new())?;
    let system_under_test = GraphMl;
    let mut output = Vec::new();

    let result = system_under_test.write_grouped(&fixture, &mut output);

    assert!(result.is_err(), "Grouped GraphML accepted an XML-forbidden character");
    assert!(output.is_empty(), "Grouped GraphML wrote output before XML character validation");
    Ok(())
}

#[test]
fn test_graphml_text_yed_graphics_and_references() -> Result<()> {
    let fixture = serialization_fixture()?;
    let system_under_test = GraphMl;

    let output = render(&system_under_test, &fixture)?;
    let xml = Document::parse(&output)?;
    let graph_element = xml.descendants().find(|node| node.has_tag_name((GRAPHML_NAMESPACE, "graph")));
    let graph_element = graph_element.context("GraphML graph element is missing")?;
    let nodes: Vec<_> = graph_element.children().filter(|node| node.has_tag_name((GRAPHML_NAMESPACE, "node"))).collect();
    let node_identifiers = nodes.iter().map(|node| node.attribute("id").context("GraphML node identifier is missing"));
    let node_identifiers: BTreeSet<_> = node_identifiers.collect::<Result<_>>()?;
    let mut labels = BTreeSet::new();

    assert!(xml.root_element().has_tag_name((GRAPHML_NAMESPACE, "graphml")), "GraphML root or namespace is incorrect");
    assert_eq!(graph_element.attribute("edgedefault"), Some("directed"), "GraphML graph must be directed");
    assert_eq!(node_identifiers.len(), 3, "GraphML must contain three distinct node identifiers");
    for node in &nodes {
        let label = node.children().find(|child| child.attribute("key") == Some("label"));
        let label = label.context("GraphML label element is missing")?.text().context("GraphML label text is missing")?;
        let visual = node.descendants().find(|child| child.has_tag_name((YWORKS_NAMESPACE, "NodeLabel")));
        let visual = visual.context("yEd visual label is missing")?;

        assert_eq!(visual.text(), Some(label), "yEd visual label differs from GraphML label");
        let has_shape = node.descendants().any(|child| child.has_tag_name((YWORKS_NAMESPACE, "ShapeNode")));
        assert!(has_shape, "yEd node graphics are missing");
        labels.insert(FolderNode::new(label.to_owned()));
    }
    assert_eq!(&labels, fixture.nodes(), "GraphML labels differ from folder names");
    let edges: Vec<_> = graph_element.children().filter(|node| node.has_tag_name((GRAPHML_NAMESPACE, "edge"))).collect();
    let edge = edges.first().context("GraphML edge is missing")?;
    let weight = edge.children().find(|node| node.attribute("key") == Some("weight")).context("GraphML edge weight is missing")?;
    let arrows = edge.descendants().find(|node| node.has_tag_name((YWORKS_NAMESPACE, "Arrows")));
    let arrows = arrows.context("yEd arrows are missing")?;
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
fn test_graphml_xml_character_validation_before_writing() -> Result<()> {
    let fixture = FolderGraph::new(BTreeSet::from([FolderNode::new("bad\u{1}name".into())]), BTreeMap::new())?;
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
