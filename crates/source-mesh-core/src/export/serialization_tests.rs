use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::io::Write;
use std::io::sink;

use claims::assert_ok;
use roxmltree::Document;
use rstest::rstest;

use pretty_assertions::assert_eq;
use super::graphml::NAMESPACE_GRAPHML;
use super::graphml::NAMESPACE_YWORKS;
use crate::export::Format;
use crate::export::rendering::render;
use crate::export::rendering::serialization_fixture::graph_fixture;
use crate::model::FolderGraph;
use crate::model::FolderNode;

#[rstest]
#[case::dot(Format::Dot, "a{{ value }} {% if true %}{braces}{% endif %}\\\"'&<>\\n\\r\u{43f}\u{1f680}")]
#[case::mermaid(Format::Mermaid, concat!(
    "a#123;#123; value #125;#125; #123;#37; if true #37;#125;#123;braces#125;#123;#37; endif #37;#125;",
    "#34;#39;#38;#60;#62;#10;#13;\u{43f}#128640;",
))]
#[case::graphml(Format::Graphml, "a{{ value }} {% if true %}{braces}{% endif %}&quot;&apos;&amp;&lt;&gt;\n&#13;\u{43f}\u{1f680}")]
fn test_template_syntax_in_folder_labels(
    #[case] format: Format,
    #[case] escaped_label: &str,
    #[values(false, true)] grouped: bool,
) {
    const LABEL_TEMPLATE: &str = "a{{ value }} {% if true %}{braces}{% endif %}\"'&<>\n\r\u{43f}\u{1f680}";
    let parent = format!("shared/{LABEL_TEMPLATE}");
    let child = format!("{parent}/leaf");
    let fixture = FolderGraph::new([parent, child, "shared/z".into()].map(FolderNode::new).into(), BTreeMap::new());
    let fixture = assert_ok!(fixture, "Template-syntax labels must form a valid folder graph");
    let system_under_test = format.exporter();
    let mut output = Vec::new();

    let result = if grouped {
        system_under_test.write_grouped(&fixture, &mut output)
    } else {
        system_under_test.write(&fixture, &mut output)
    };
    assert_ok!(result, "Template syntax in labels must serialize as data, grouped={grouped}");
    let output = String::from_utf8(output);
    let output = assert_ok!(output, "Serialized template-syntax labels must be UTF-8");
    let display_label = if grouped { escaped_label.to_owned() } else { format!("shared/{escaped_label}") };
    let (parent_markup, child_markup) = match format {
        Format::Dot => (format!("label=\"{display_label}\""), format!("label=\"{display_label}/leaf\"")),
        Format::Mermaid => (format!("[\"{display_label}\"]"), format!("[\"{display_label}/leaf\"]")),
        Format::Graphml => (format!(">{display_label}</y:NodeLabel>"), format!(">{display_label}/leaf</y:NodeLabel>")),
    };
    let parent_label_count = if grouped { 2 } else { 1 };

    assert_eq!(
        output.matches(&parent_markup).count(), parent_label_count,
        "Parent and group display labels must preserve template syntax as correctly escaped data"
    );
    assert_eq!(
        output.matches(&child_markup).count(), 1,
        "Child display labels must preserve template syntax as correctly escaped data"
    );
    if matches!(format, Format::Graphml) {
        let parent_data = format!("<data key=\"label\">shared/{escaped_label}</data>");
        let child_data = format!("<data key=\"label\">shared/{escaped_label}/leaf</data>");

        assert_eq!(output.matches(&parent_data).count(), 1, "GraphML parent data must retain the escaped original label");
        assert_eq!(output.matches(&child_data).count(), 1, "GraphML child data must retain the escaped original label");
    }
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
#[case(Format::Graphml)]
fn test_export_determinism(#[case] format: Format) {
    let fixture = graph_fixture();
    let fixture = assert_ok!(fixture, "Determinism fixture must form a valid folder graph");
    let system_under_test = format.exporter();

    let first_output = render(system_under_test.as_ref(), &fixture);
    let first_output = assert_ok!(first_output, "First graph export must render as UTF-8");
    let second_output = render(system_under_test.as_ref(), &fixture);
    let second_output = assert_ok!(second_output, "Repeated graph export must render as UTF-8");

    assert_eq!(first_output, second_output, "Repeated export changed output for the same graph");
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
#[case(Format::Graphml)]
fn test_empty_graph_serialization(#[case] format: Format, #[values(false, true)] grouped: bool) {
    let fixture = FolderGraph::default();
    let system_under_test = format.exporter();
    let mut output = Vec::new();

    let result = if grouped {
        system_under_test.write_grouped(&fixture, &mut output)
    } else {
        system_under_test.write(&fixture, &mut output)
    };
    assert_ok!(result, "An empty graph must serialize successfully, grouped={grouped}");
    let output = String::from_utf8(output);
    let output = assert_ok!(output, "Empty graph serialization must be UTF-8");

    match format {
        Format::Dot => {
            let graph = graphviz_rust::parse(&output);
            assert_ok!(graph, "Empty DOT output must be a parseable graph");

            assert_eq!(output, "strict digraph folders {\n  rankdir=LR\n}\n", "Empty DOT serialization changed");
        }
        Format::Mermaid => assert_eq!(output.as_str(), "flowchart LR\n", "Empty Mermaid graph must contain only its header"),
        Format::Graphml => {
            let document = Document::parse(&output);
            assert_ok!(document, "Empty GraphML output must be a parseable XML document");
            let expected = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<graphml xmlns="{NAMESPACE_GRAPHML}"
         xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
         xmlns:y="{NAMESPACE_YWORKS}"
         xsi:schemaLocation="{NAMESPACE_GRAPHML} http://www.yworks.com/xml/schema/graphml/1.1/ygraphml.xsd">
  <key id="label" for="node" attr.name="label" attr.type="string"/>
  <key id="weight" for="edge" attr.name="dependencies" attr.type="long"/>
  <key id="kind" for="edge" attr.name="dependency_kind" attr.type="string"/>
  <key id="ng" for="node" yfiles.type="nodegraphics"/>
  <key id="eg" for="edge" yfiles.type="edgegraphics"/>
  <graph id="folders" edgedefault="directed">
  </graph>
</graphml>
"#
            );

            assert_eq!(output, expected, "Empty GraphML serialization changed");
        }
    }
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
#[case(Format::Graphml)]
fn test_deep_grouped_export_and_cleanup(#[case] format: Format) {
    const DEPTH: usize = 2048;
    let fixture = FolderGraph::new(
        [format!("shared/{}leaf", "a/".repeat(DEPTH)), "shared/z".into()].map(FolderNode::new).into(),
        BTreeMap::new(),
    );
    let fixture = assert_ok!(fixture, "Deep grouped-export paths must form a valid graph");
    let system_under_test = format.exporter();

    let result = system_under_test.write_grouped(&fixture, &mut sink());
    let failed_write = system_under_test.write_grouped(&fixture, &mut BrokenWriter);

    assert_ok!(result, "Deep grouped export and cleanup must succeed");
    assert!(failed_write.is_err(), "Deep export must preserve writer failures while cleaning up its flat storage");
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
fn test_export_write_error_propagation(#[case] format: Format, #[values(false, true)] grouped: bool) {
    let fixture = graph_fixture();
    let fixture = assert_ok!(fixture, "Writer-error fixture must form a valid folder graph");
    let system_under_test = format.exporter();

    let result = if grouped {
        system_under_test.write_grouped(&fixture, &mut BrokenWriter)
    } else {
        system_under_test.write(&fixture, &mut BrokenWriter)
    };
    let writer_error = result.as_ref().err().and_then(|error| error.downcast_ref::<std::io::Error>());

    assert!(result.is_err(), "Exporter did not propagate its writer's failure");
    assert_eq!(
        writer_error.map(std::io::Error::kind), Some(ErrorKind::BrokenPipe),
        "Exporter must preserve the original I/O error type and kind"
    );
}

#[derive(Default)]
struct ShortWriter {
    output: Vec<u8>,
}

impl Write for ShortWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let Some(&byte) = buffer.first() else { return Ok(0); };
        self.output.push(byte);
        Ok(1)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        unreachable!("ShortWriter::flush must not be called by an exporter");
    }
}

#[rstest]
#[case(Format::Dot)]
#[case(Format::Mermaid)]
#[case(Format::Graphml)]
fn test_export_short_writes(#[case] format: Format, #[values(false, true)] grouped: bool) {
    let fixture = graph_fixture();
    let fixture = assert_ok!(fixture, "Short-write fixture must form a valid folder graph");
    let system_under_test = format.exporter();
    let mut complete_output = Vec::new();
    let mut short_writer = ShortWriter::default();

    if grouped {
        let complete_write_result = system_under_test.write_grouped(&fixture, &mut complete_output);
        assert_ok!(complete_write_result, "Grouped export to a complete writer must succeed");
        let short_write_result = system_under_test.write_grouped(&fixture, &mut short_writer);
        assert_ok!(short_write_result, "Grouped export must handle single-byte writes");
    } else {
        let complete_write_result = system_under_test.write(&fixture, &mut complete_output);
        assert_ok!(complete_write_result, "Flat export to a complete writer must succeed");
        let short_write_result = system_under_test.write(&fixture, &mut short_writer);
        assert_ok!(short_write_result, "Flat export must handle single-byte writes");
    }
    let complete_output = String::from_utf8(complete_output);
    let complete_output = assert_ok!(complete_output, "Complete writer output must be UTF-8");
    let short_output = String::from_utf8(short_writer.output);
    let short_output = assert_ok!(short_output, "Single-byte writes must preserve valid UTF-8");

    assert_eq!(short_output, complete_output, "Short writes must not truncate or corrupt serialized UTF-8 output");
}
