use anyhow::Result;
use pretty_assertions::assert_eq;

use super::Mermaid;
use crate::export::rendering::exporter::Exporter as _;
use crate::export::rendering::grouping_fixture::hierarchy_fixture;
use crate::export::rendering::grouping_fixture::render_grouped;
use crate::export::rendering::serialization_fixture::graph_fixture;
use crate::export::rendering::serialization_fixture::render;

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
