use std::fs;
use std::io;
use std::io::Write as _;
use std::path::Path;

use anyhow::Context as _;
use anyhow::Result;
use protobuf::Message as _;
use protobuf::MessageField;
use scip::types::Index;
use scip::types::Metadata;
use source_mesh_core::analyzer_options::Options as AnalyzerOptions;
use source_mesh_core::backend::IndexSource as _;
use source_mesh_core::backend::RustAnalyzer;

use regenerate_dependency_graph_scip as main;

const FIXTURE_PROJECT_ROOT: &str = "file:///fixture/dependency-graph";

fn regenerate_dependency_graph_scip() -> Result<()> {
    let fixture_directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("fixtures");
    let fixture_project = fixture_directory.join("dependency-graph");
    let analyzer_options = AnalyzerOptions::new(false);
    let generated_bytes = RustAnalyzer::new(&fixture_project, None, &analyzer_options).index()?;
    let mut index = Index::parse_from_bytes(&generated_bytes).context("Cannot decode generated fixture")?;
    make_metadata_portable(&mut index.metadata);
    let portable_bytes = index.write_to_bytes().context("Cannot serialize portable fixture")?;
    let fixture_index = fixture_directory.join("dependency-graph.scip");
    let fixture_write = fs::write(&fixture_index, portable_bytes);
    fixture_write.with_context(|| format!("Cannot write fixture {}", fixture_index.display()))?;
    /* Diagnostic output is optional: write failures must not replace successful fixture generation. */
    let _: io::Result<()> = writeln!(io::stderr().lock(), "Regenerated {} with portable metadata.", fixture_index.display());
    Ok(())
}

fn make_metadata_portable(metadata: &mut MessageField<Metadata>) {
    let Some(metadata) = metadata.as_mut() else {
        unreachable!("Test classification must install fixture metadata");
    };
    let Some(tool) = metadata.tool_info.as_mut() else {
        unreachable!("Test classification must install fixture tool information");
    };
    metadata.project_root = FIXTURE_PROJECT_ROOT.into();
    tool.arguments.clear();
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use scip::types::Metadata;
    use scip::types::ToolInfo;

    use super::FIXTURE_PROJECT_ROOT;
    use super::make_metadata_portable;

    #[test]
    fn test_portable_metadata() {
        let fixture = Metadata {
            project_root: "file:///machine/project".into(),
            tool_info: Some(ToolInfo {
                name: "classification-tool".into(),
                version: "1.0.0".into(),
                arguments: vec!["machine-specific-argument".into()],
                ..ToolInfo::default()
            }).into(),
            ..Metadata::default()
        };
        let mut expected = fixture.clone();
        expected.project_root = FIXTURE_PROJECT_ROOT.into();
        if let Some(tool) = expected.tool_info.as_mut() {
            tool.arguments.clear();
        }
        let mut system_under_test = Some(fixture).into();

        make_metadata_portable(&mut system_under_test);

        assert_eq!(system_under_test.as_ref(), Some(&expected));
    }
}
