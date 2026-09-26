use std::fs;
use std::path::Path;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use protobuf::Message as _;
use source_mesh::analyzer_options::Options as AnalyzerOptions;
use source_mesh::backend::IndexSource as _;
use source_mesh::backend::RustAnalyzer;
use source_mesh::scip_backend::parse_scip;
use scip::types::Index;

use regenerate_dependency_graph_scip as main;

fn regenerate_dependency_graph_scip() -> Result<()> {
    let fixture_directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("fixtures");
    let fixture_project = fixture_directory.join("dependency-graph");
    let analyzer_options = AnalyzerOptions::new(false);
    let generated_bytes = RustAnalyzer::new(&fixture_project, None, &analyzer_options).index()?;
    let generated_graph = parse_scip(&generated_bytes)?;
    let mut index = Index::parse_from_bytes(&generated_bytes).context("Cannot decode generated fixture")?;
    {
        let metadata = index.metadata.as_mut().context("Generated fixture has no metadata")?;
        metadata.project_root = "file:///fixture/dependency-graph".into();
        metadata.tool_info.as_mut().context("Generated fixture has no tool information")?.arguments.clear();
    }
    let portable_bytes = index.write_to_bytes().context("Cannot serialize portable fixture")?;
    ensure!(
        parse_scip(&portable_bytes)? == generated_graph,
        "Removing machine-specific fixture metadata changed the dependency graph"
    );
    let fixture_index = fixture_directory.join("dependency-graph.scip");
    fs::write(&fixture_index, portable_bytes)
        .with_context(|| format!("Cannot write fixture {}", fixture_index.display()))?;
    eprintln!("Regenerated {} with portable metadata.", fixture_index.display());
    Ok(())
}
