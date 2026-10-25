use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::bail;
use claims::assert_ok;
use claims::assert_some;
use roxmltree::Document;
use roxmltree::Node;
use rstest::rstest;

use super::GraphMl;
use crate::SourcePath;
use crate::export::graphml::NAMESPACE_GRAPHML;
use crate::export::graphml::NAMESPACE_YWORKS;
use crate::export::rendering::edge_style::kind_name;
use crate::export::rendering::exporter::Exporter as _;
use crate::export::rendering::render;
use crate::export::rendering::serialization_fixture::graph_fixture as serialization_fixture;
use crate::export::rendering::serialization_fixture::parallel_dependency_fixture;
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
fn test_nested_folder_groups_and_missing_ancestors() {
    let fixture = graph_fixture();
    let fixture = assert_ok!(fixture, "Nested-folder fixture must form a valid graph");
    let system_under_test = GraphMl;
    let mut output = Vec::new();

    let result = system_under_test.write_grouped(&fixture, &mut output);
    assert_ok!(result, "GraphML must serialize nested folders and missing ancestors");
    let output = String::from_utf8(output);
    let output = assert_ok!(output, "Nested-folder GraphML must be UTF-8");
    let document = Document::parse(&output);
    let document = assert_ok!(document, "Nested-folder GraphML must be valid XML");
    let parents: BTreeMap<_, _> = document.descendants().filter(|node| node.has_tag_name("node")).map(|node| {
        let label = node_label(node);
        let label = assert_some!(label, "Every GraphML folder must have a canonical label");
        let parent = node.parent().and_then(|graph| graph.parent()).and_then(node_label).unwrap_or("");
        (label, parent)
    }).collect();
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
            let realizers = assert_some!(realizers, "GraphML folder group must have yEd realizers");
            let states = realizers.descendants().filter(|child| child.has_tag_name("State"));
            let states: Vec<_> = states.filter_map(|state| state.attribute("closed")).collect();
            let parent_geometry = geometry(node);
            let parent_geometry = assert_ok!(parent_geometry, "Parent folder must have valid yEd geometry");
            let [parent_left, parent_top, parent_width, parent_height] = parent_geometry;

            assert_eq!(node.attribute("yfiles.foldertype"), Some("group"), "Parent folder is not a yEd group");
            assert_eq!(realizers.attribute("active"), Some("0"), "Group must initially be expanded");
            assert_eq!(states, ["false", "true"], "Group must support expanded and collapsed views");
            for child in nested_graph.children().filter(|child| child.has_tag_name("node")) {
                let child_geometry = geometry(child);
                let child_geometry = assert_ok!(child_geometry, "Nested child folder must have valid yEd geometry");
                let [child_left, child_top, child_width, child_height] = child_geometry;

                assert!(child_left > parent_left && child_top > parent_top, "Child folder starts outside its group");
                assert!(child_left + child_width < parent_left + parent_width, "Child folder exceeds group width");
                assert!(child_top + child_height < parent_top + parent_height, "Child folder exceeds group height");
            }
        } else {
            assert!(node.attribute("yfiles.foldertype").is_none(), "Leaf folder was marked as a group");
            assert!(node.descendants().any(|child| child.has_tag_name("ShapeNode")), "Leaf folder has no yEd shape");
        }
    }
}

#[test]
fn test_nested_layout_and_serialization_order() {
    let fixture = graph_fixture();
    let fixture = assert_ok!(fixture, "Nested-layout fixture must form a valid graph");
    let system_under_test = GraphMl;
    let mut output = Vec::new();

    let result = system_under_test.write_grouped(&fixture, &mut output);
    assert_ok!(result, "GraphML must serialize the nested layout");
    let output = String::from_utf8(output);
    let output = assert_ok!(output, "Nested-layout GraphML must be UTF-8");
    let document = Document::parse(&output);
    let document = assert_ok!(document, "Nested-layout GraphML must be valid XML");
    let nodes: Vec<_> = document.descendants().filter(|node| node.has_tag_name("node")).map(|node| {
        let identifier = node.attribute("id");
        let identifier = assert_some!(identifier, "Every layout folder must have an identifier");
        let label = node_label(node);
        let label = assert_some!(label, "Layout folder {identifier} must have a label");
        let geometry = geometry(node);
        let geometry = assert_ok!(geometry, "Layout folder {identifier} must have valid geometry");
        (identifier, label, geometry)
    }).collect();

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
}

#[test]
fn test_group_endpoint_cross_group_self_dependencies_and_weights() {
    let fixture = graph_fixture();
    let fixture = assert_ok!(fixture, "Cross-group dependency fixture must form a valid graph");
    let system_under_test = GraphMl;
    let mut flat_output = Vec::new();
    let mut grouped_output = Vec::new();

    let flat_write_result = system_under_test.write(&fixture, &mut flat_output);
    assert_ok!(flat_write_result, "Flat GraphML must serialize cross-group and self-dependencies");
    let grouped_write_result = system_under_test.write_grouped(&fixture, &mut grouped_output);
    assert_ok!(grouped_write_result, "Grouped GraphML must serialize cross-group and self-dependencies");
    let flat_output = String::from_utf8(flat_output);
    let flat_output = assert_ok!(flat_output, "Flat GraphML dependency output must be UTF-8");
    let grouped_output = String::from_utf8(grouped_output);
    let grouped_output = assert_ok!(grouped_output, "Grouped GraphML dependency output must be UTF-8");
    let flat_document = Document::parse(&flat_output);
    let flat_document = assert_ok!(flat_document, "Flat GraphML dependency output must be valid XML");
    let grouped_document = Document::parse(&grouped_output);
    let grouped_document = assert_ok!(grouped_document, "Grouped GraphML dependency output must be valid XML");
    let grouped_dependencies = dependencies(&grouped_document);
    let grouped_dependencies = assert_ok!(grouped_dependencies, "Grouped GraphML dependencies must be complete and parseable");
    let flat_dependencies = dependencies(&flat_document);
    let flat_dependencies = assert_ok!(flat_dependencies, "Flat GraphML dependencies must be complete and parseable");
    let flat_edges = flat_document.descendants().filter(|node| node.has_tag_name("edge"));
    let flat_edges: Vec<_> = flat_edges.map(|edge| {
        let markup = flat_output.get(edge.range());
        assert_some!(markup, "Flat GraphML edge range must refer to its serialized XML")
    }).collect();
    let grouped_edges = grouped_document.descendants().filter(|node| node.has_tag_name("edge"));
    let grouped_edges: Vec<_> = grouped_edges.map(|edge| {
        let markup = grouped_output.get(edge.range());
        assert_some!(markup, "Grouped GraphML edge range must refer to its serialized XML")
    }).collect();

    assert_eq!(&grouped_dependencies, fixture.edges(), "Grouping changed dependency endpoints or weights");
    assert_eq!(grouped_dependencies, flat_dependencies, "Grouping changed flat graph dependencies");
    assert_eq!(grouped_edges, flat_edges, "Grouping changed GraphML edge elements or their identifiers");
    for edge in grouped_document.descendants().filter(|node| node.has_tag_name("edge")) {
        let parent_identifier = edge.parent().and_then(|parent| parent.attribute("id"));
        assert_eq!(parent_identifier, Some("folders"), "Edge is not in a common ancestor graph");
    }
}

#[rstest]
#[case(false)]
#[case(true)]
fn test_parallel_dependency_kinds_and_weights(#[case] grouped: bool) {
    let fixture = parallel_dependency_fixture();
    let fixture = assert_ok!(fixture, "Parallel-dependency fixture must form a valid graph");
    let system_under_test = GraphMl;
    let mut output = Vec::new();

    let result = if grouped {
        system_under_test.write_grouped(&fixture, &mut output)
    } else {
        system_under_test.write(&fixture, &mut output)
    };
    assert_ok!(result, "GraphML must serialize parallel dependency kinds, grouped={grouped}");
    let output = String::from_utf8(output);
    let output = assert_ok!(output, "Parallel-dependency GraphML must be UTF-8");
    let document = Document::parse(&output);
    let document = assert_ok!(document, "Parallel-dependency GraphML must be valid XML");
    let parsed_dependencies = dependencies(&document);
    let parsed_dependencies = assert_ok!(parsed_dependencies, "Parallel dependency kinds and weights must be parseable");
    let edges = document.descendants().filter(|node| node.has_tag_name("edge"));
    let identifiers: BTreeSet<_> = edges.filter_map(|edge| edge.attribute("id")).collect();

    assert_eq!(&parsed_dependencies, fixture.edges(), "Dependency kinds or their separate weights changed");
    assert_eq!(identifiers.len(), 2, "Parallel dependency kinds share an edge identifier");
}

#[test]
fn test_common_path_labels_original_ancestors_and_top_level_layout() {
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
    );
    let fixture = assert_ok!(fixture, "Common-path layout fixture must form a valid graph");
    let system_under_test = GraphMl;
    let mut output = Vec::new();

    let result = system_under_test.write_grouped(&fixture, &mut output);
    assert_ok!(result, "GraphML must serialize the common-path hierarchy");
    let output = String::from_utf8(output);
    let output = assert_ok!(output, "Common-path GraphML must be UTF-8");
    let document = Document::parse(&output);
    let document = assert_ok!(document, "Common-path GraphML must be valid XML");
    let nodes: BTreeMap<_, _> = document.descendants().filter(|node| node.has_tag_name("node")).map(|node| {
        let label = node_label(node);
        let label = assert_some!(label, "GraphML folder must have a canonical label");
        let identifier = node.attribute("id");
        let identifier = assert_some!(identifier, "GraphML folder {label} must have an identifier");
        let graphics = node.children().find(|child| child.attribute("key") == Some("ng"));
        let visual = graphics.and_then(|graphics| graphics.descendants().find(|child| child.has_tag_name("NodeLabel")));
        let visual = visual.and_then(|label_node| label_node.text());
        let visual = assert_some!(visual, "GraphML folder {label} must have a visual label");
        let parent = node.parent().and_then(|graph| graph.parent()).and_then(node_label).unwrap_or("");
        (label, (identifier, visual, parent))
    }).collect();
    let expected = BTreeMap::from([
        ("src", ("n0", "src", "")),
        ("src/engine", ("n1", "src/engine", "")),
        ("src/engine/api", ("n2", "api", "")),
        ("src/engine/api/http", ("n3", "api/http", "src/engine/api")),
        ("src/engine/storage", ("n4", "storage", "")),
    ]);
    let grouped_dependencies = dependencies(&document);
    let grouped_dependencies = assert_ok!(grouped_dependencies, "Common-path GraphML dependencies must be parseable");
    let graph = document.root_element().children().find(|node| node.has_tag_name("graph"));
    let graph = assert_some!(graph, "Common-path GraphML must contain its top-level graph");

    assert_eq!(nodes, expected, "Common-path removal changed node identifiers, canonical labels, visible labels, or hierarchy");
    assert_eq!(&grouped_dependencies, fixture.edges(), "Common-path removal changed dependencies or weights");
    let mut previous_bottom = None;
    for node in graph.children().filter(|node| node.has_tag_name("node")) {
        let node_geometry = geometry(node);
        let node_geometry = assert_ok!(node_geometry, "Top-level folder must have valid geometry");
        let [_, top, _, height] = node_geometry;
        if let Some(previous_bottom) = previous_bottom {
            assert!(top >= previous_bottom + 40, "Top-level folder geometry overlaps another folder");
        }
        previous_bottom = Some(top + height);
    }
}

#[test]
fn test_group_and_leaf_label_escaping() {
    const PARENT: &str = "a\"<&\\#`\n\r\u{43f}\u{430}\u{43f}\u{43a}\u{430}";
    let child = format!("{PARENT}/nested");
    let nodes = [PARENT.to_owned(), child.clone(), "tests".into()].map(FolderNode::new).into();
    let fixture = FolderGraph::new(nodes, BTreeMap::new());
    let fixture = assert_ok!(fixture, "Group and leaf punctuation labels must form a valid graph");
    let system_under_test = GraphMl;
    let mut output = Vec::new();

    let result = system_under_test.write_grouped(&fixture, &mut output);
    assert_ok!(result, "GraphML must serialize punctuation in group and leaf labels");
    let output = String::from_utf8(output);
    let output = assert_ok!(output, "Escaped GraphML labels must be UTF-8");
    let document = Document::parse(&output);
    let document = assert_ok!(document, "Escaped GraphML labels must remain valid XML");
    let labels: BTreeSet<_> = document.descendants().filter(|node| node.has_tag_name("node")).filter_map(node_label).collect();
    let visuals = document.descendants().filter(|node| node.has_tag_name("NodeLabel"));
    let visuals: BTreeSet<_> = visuals.filter_map(|node| node.text()).collect();
    let expected = BTreeSet::from([PARENT, child.as_str(), "tests"]);

    assert_eq!(labels, expected, "GraphML grouping changed punctuation in folder labels");
    assert_eq!(visuals, expected, "yEd grouping changed punctuation in visual labels");
}

#[rstest]
#[case(false)]
#[case(true)]
fn test_empty_and_depth_zero_grouping(#[case] include_files: bool) {
    let files = if include_files {
        let source_path = SourcePath::new(PathBuf::from_iter(["src", "api", "lib.rs"]));
        let source_path = assert_ok!(source_path, "Depth-zero fixture file must have a valid source path");
        [FileNode::new(source_path)].into()
    } else {
        BTreeSet::new()
    };
    let file_graph = FileGraph::new(files, BTreeSet::new());
    let file_graph = assert_ok!(file_graph, "Files without dependencies must form a valid graph");
    let fixture = file_graph.folders(Path::new("."), Some(0), false);
    let fixture = assert_ok!(fixture, "Depth-zero folder aggregation must succeed");
    let system_under_test = GraphMl;
    let mut flat_output = Vec::new();
    let mut grouped_output = Vec::new();

    let flat_write_result = system_under_test.write(&fixture, &mut flat_output);
    assert_ok!(flat_write_result, "Flat GraphML must serialize a graph without a hierarchy");
    let grouped_write_result = system_under_test.write_grouped(&fixture, &mut grouped_output);
    assert_ok!(grouped_write_result, "Grouped GraphML must serialize a graph without a hierarchy");
    let grouped_text = std::str::from_utf8(&grouped_output);
    let grouped_text = assert_ok!(grouped_text, "GraphML without a hierarchy must be UTF-8");
    let document = Document::parse(grouped_text);
    let document = assert_ok!(document, "GraphML without a hierarchy must be valid XML");

    assert_eq!(flat_output, grouped_output, "Grouping changed a graph with no folder hierarchy");
    let node_count = document.descendants().filter(|node| node.has_tag_name("node")).count();
    assert_eq!(node_count, usize::from(include_files), "Grouping introduced an empty artificial group");
}

#[test]
fn test_grouped_xml_character_validation_before_writing() {
    let fixture = FolderGraph::new([FolderNode::new("bad\u{1}/nested".into())].into(), BTreeMap::new());
    let fixture = assert_ok!(fixture, "Graph must accept folder labels before XML character validation");
    let system_under_test = GraphMl;
    let mut output = Vec::new();

    let result = system_under_test.write_grouped(&fixture, &mut output);

    assert!(result.is_err(), "Grouped GraphML accepted an XML-forbidden character");
    assert!(output.is_empty(), "Grouped GraphML wrote output before XML character validation");
}

#[test]
fn test_graphml_text_yed_graphics_and_references() {
    let fixture = serialization_fixture();
    let fixture = assert_ok!(fixture, "GraphML graphics fixture must form a valid folder graph");
    let system_under_test = GraphMl;

    let output = render(&system_under_test, &fixture);
    let output = assert_ok!(output, "GraphML must render labels, yEd graphics, and dependency references");
    let xml = Document::parse(&output);
    let xml = assert_ok!(xml, "GraphML graphics output must be valid XML");
    let graph_element = xml.descendants().find(|node| node.has_tag_name((NAMESPACE_GRAPHML, "graph")));
    let graph_element = assert_some!(graph_element, "GraphML output must contain a namespaced graph element");
    let nodes: Vec<_> = graph_element.children().filter(|node| node.has_tag_name((NAMESPACE_GRAPHML, "node"))).collect();
    let node_identifiers: BTreeSet<_> = nodes.iter().map(|node| {
        let identifier = node.attribute("id");
        assert_some!(identifier, "Every GraphML dependency node must have an identifier")
    }).collect();
    let mut labels = BTreeSet::new();

    assert!(xml.root_element().has_tag_name((NAMESPACE_GRAPHML, "graphml")), "GraphML root or namespace is incorrect");
    assert_eq!(graph_element.attribute("edgedefault"), Some("directed"), "GraphML graph must be directed");
    assert_eq!(node_identifiers.len(), 3, "GraphML must contain three distinct node identifiers");
    for node in &nodes {
        let label = node.children().find(|child| child.attribute("key") == Some("label"));
        let label = assert_some!(label, "GraphML dependency node must have a label element");
        let label = label.text();
        let label = assert_some!(label, "GraphML dependency label must contain text");
        let visual = node.descendants().find(|child| child.has_tag_name((NAMESPACE_YWORKS, "NodeLabel")));
        let visual = assert_some!(visual, "GraphML dependency node must have a yEd visual label");

        assert_eq!(visual.text(), Some(label), "yEd visual label differs from GraphML label");
        let has_shape = node.descendants().any(|child| child.has_tag_name((NAMESPACE_YWORKS, "ShapeNode")));
        assert!(has_shape, "yEd node graphics are missing");
        labels.insert(FolderNode::new(label.to_owned()));
    }
    assert_eq!(&labels, fixture.nodes(), "GraphML labels differ from folder names");
    let edges: Vec<_> = graph_element.children().filter(|node| node.has_tag_name((NAMESPACE_GRAPHML, "edge"))).collect();
    let edge = edges.first();
    let edge = assert_some!(edge, "GraphML must contain the fixture dependency edge");
    let weight = edge.children().find(|node| node.attribute("key") == Some("weight"));
    let weight = assert_some!(weight, "GraphML dependency edge must contain weight data");
    let arrows = edge.descendants().find(|node| node.has_tag_name((NAMESPACE_YWORKS, "Arrows")));
    let arrows = assert_some!(arrows, "GraphML dependency edge must have yEd arrows");
    let source = edge.attribute("source");
    let source = assert_some!(source, "GraphML dependency edge must have a source reference");
    let target = edge.attribute("target");
    let target = assert_some!(target, "GraphML dependency edge must have a target reference");

    assert_eq!(edges.len(), 1, "Expected one GraphML edge");
    assert_eq!(source, "n0", "GraphML edge source is incorrect");
    assert_eq!(target, "n1", "GraphML edge target is incorrect");
    assert!(node_identifiers.contains(source), "GraphML edge source has no corresponding node");
    assert!(node_identifiers.contains(target), "GraphML edge target has no corresponding node");
    assert_eq!(weight.text(), Some("7"), "GraphML edge weight is incorrect");
    assert_eq!(arrows.attribute("source"), Some("none"), "yEd source must have no arrow");
    assert_eq!(arrows.attribute("target"), Some("standard"), "yEd target must have a standard arrow");
}

#[test]
fn test_graphml_xml_character_validation_before_writing() {
    let fixture = FolderGraph::new(BTreeSet::from([FolderNode::new("bad\u{1}name".into())]), BTreeMap::new());
    let fixture = assert_ok!(fixture, "Graph must accept raw folder labels before XML character validation");
    let mut output = Vec::new();
    let system_under_test = GraphMl;

    let result = system_under_test.write(&fixture, &mut output);

    assert!(result.is_err(), "GraphML accepted an XML-forbidden character");
    assert!(output.is_empty(), "GraphML wrote output before validating XML characters");
}

#[test]
fn test_graphml_signed_long_weight_validation_before_writing() {
    if i64::try_from(usize::MAX).is_ok() {
        return;
    }
    let fixture = FolderGraph::new(
        BTreeSet::from([FolderNode::new("source".into()), FolderNode::new("target".into())]),
        BTreeMap::from([(
            FolderEdge::new(FolderNode::new("source".into()), FolderNode::new("target".into()), DependencyKind::Production),
            DependencyCount::new(usize::MAX),
        )]),
    );
    let fixture = assert_ok!(fixture, "Graph must accept native dependency counts before GraphML signed-long validation");
    let mut output = Vec::new();
    let system_under_test = GraphMl;

    let result = system_under_test.write(&fixture, &mut output);

    assert!(result.is_err(), "GraphML accepted a weight outside the signed long range");
    assert!(output.is_empty(), "GraphML wrote output before validating its signed long range");
}
