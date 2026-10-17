use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use pretty_assertions::assert_eq;
use protobuf::Message as _;
use rstest::rstest;
use scip::types::Document;
use scip::types::Index;
use tempfile::tempdir;

use crate::application::CycleOptions;
use crate::application::Input;
use crate::application::Request;
use crate::application::run;
use crate::export::Format;
use crate::export::Options;
use crate::scip_backend::parse_scip;

#[test]
fn test_saved_analysis_replay() -> Result<()> {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src").join("fixtures").join("dependency-graph.scip");
    let graph = parse_scip(&fs::read(&fixture)?)?;
    let root = PathBuf::from("src");
    let folders = graph.within_root(&root)?.folders(&root, Some(1), false)?;
    let mut expected = Vec::new();
    Format::Mermaid.exporter().write_with_options(&folders, &Options::builder().group_folders(true).build(), &mut expected)?;
    let request = Request::builder().input(Input::ScipFile(fixture)).root(root).format(Format::Mermaid);
    let system_under_test = request.depth(1).group_folders(true).build();
    let mut diagram = Vec::new();

    run(&system_under_test, &mut diagram)?;

    assert_eq!(diagram, expected);
    assert_ne!(diagram, [] as [u8; 0]);
    Ok(())
}

#[test]
fn test_replay_cannot_save_analysis() -> Result<()> {
    const EXISTING_CONTENT: &str = "preserved";
    let fixture = tempdir()?;
    let output = fixture.path().join("index.scip");
    fs::write(&output, EXISTING_CONTENT)?;
    let input = Input::ScipFile(fixture.path().join("missing.scip"));
    let system_under_test = Request::builder().input(input).save_scip(output.clone()).build();
    let mut diagram = Vec::new();

    let result = run(&system_under_test, &mut diagram);
    let preserved = fs::read_to_string(output)?;

    assert!(result.is_err_and(|error| error.to_string().contains("Cannot save analysis")));
    assert_eq!(preserved, EXISTING_CONTENT);
    assert_eq!(diagram, [] as [u8; 0]);
    Ok(())
}

#[rstest]
#[case::same_path(false)]
#[case::hard_link(true)]
fn test_replay_cannot_overwrite_input(#[case] alias: bool) -> Result<()> {
    let fixture = tempdir()?;
    let input = fixture.path().join("index.scip");
    let bytes = include_bytes!("fixtures/dependency-graph.scip");
    fs::write(&input, bytes)?;
    let output = if alias {
        let output = fixture.path().join("alias.scip");
        fs::hard_link(&input, &output)?;
        output
    } else {
        input.clone()
    };
    let system_under_test = Request::builder().input(Input::ScipFile(input.clone())).output(output).build();
    let mut diagram = Vec::new();

    let result = run(&system_under_test, &mut diagram);
    let preserved = fs::read(input)?;

    assert!(result.is_err_and(|error| error.to_string().contains("must not overwrite input")));
    assert_eq!(preserved, bytes);
    assert_eq!(diagram, [] as [u8; 0]);
    Ok(())
}

#[rstest]
#[case::empty(b"")]
#[case::malformed(b"\xff")]
fn test_invalid_index_preserves_outputs(#[case] bytes: &[u8]) -> Result<()> {
    const EXISTING_CONTENT: &str = "preserved";
    let fixture = tempdir()?;
    let input = fixture.path().join("index.scip");
    let output = fixture.path().join("diagram.dot");
    let folder_report = fixture.path().join("folders.json");
    let file_report = fixture.path().join("files.json");
    fs::write(&input, bytes)?;
    fs::write(&output, EXISTING_CONTENT)?;
    let cycles = CycleOptions::builder().cycles_output(folder_report.clone());
    let cycles = cycles.file_cycles_output(file_report.clone()).build();
    let system_under_test = Request::builder().input(Input::ScipFile(input)).output(output.clone()).cycles(cycles).build();
    let mut diagram = Vec::new();

    let result = run(&system_under_test, &mut diagram);
    let preserved = fs::read_to_string(output)?;

    assert!(result.is_err(), "Malformed external analysis must return an error");
    assert_eq!(preserved, EXISTING_CONTENT);
    assert!(!folder_report.try_exists()?, "Invalid input created a folder report");
    assert!(!file_report.try_exists()?, "Invalid input created a file report");
    assert_eq!(diagram, [] as [u8; 0]);
    Ok(())
}

#[rstest]
#[case::empty("")]
#[case::parent("../outside.rs")]
#[case::absolute("/absolute.rs")]
fn test_invalid_source_path_preserves_outputs(#[case] path: &str) -> Result<()> {
    let fixture = tempdir()?;
    let input = fixture.path().join("index.scip");
    let output = fixture.path().join("diagram.dot");
    let index = Index {
        documents: vec![Document { relative_path: path.into(), ..Document::default() }],
        ..Index::default()
    };
    fs::write(&input, index.write_to_bytes()?)?;
    let system_under_test = Request::builder().input(Input::ScipFile(input)).output(output.clone()).build();
    let mut diagram = Vec::new();

    let result = run(&system_under_test, &mut diagram);

    assert!(result.is_err(), "Malformed external source paths must return an error");
    assert!(!output.try_exists()?, "Invalid input created a diagram");
    assert_eq!(diagram, [] as [u8; 0]);
    Ok(())
}

#[rstest]
#[case::same_path(false)]
#[case::hard_link(true)]
fn test_conflicting_outputs_preserve_files(#[case] alias: bool) -> Result<()> {
    const EXISTING_CONTENT: &str = "preserved";
    let fixture = tempdir()?;
    let input = fixture.path().join("index.scip");
    let output = fixture.path().join("diagram.dot");
    let file_report = fixture.path().join("files.json");
    fs::write(&input, include_bytes!("fixtures/dependency-graph.scip"))?;
    fs::write(&output, EXISTING_CONTENT)?;
    fs::write(&file_report, EXISTING_CONTENT)?;
    let folder_report = if alias {
        let report = fixture.path().join("folders.json");
        fs::hard_link(&output, &report)?;
        report
    } else {
        output.clone()
    };
    let cycles = CycleOptions::builder().cycles_output(folder_report).file_cycles_output(file_report.clone()).build();
    let system_under_test = Request::builder().input(Input::ScipFile(input)).output(output.clone()).cycles(cycles).build();
    let mut diagram = Vec::new();

    let result = run(&system_under_test, &mut diagram);
    let preserved_diagram = fs::read_to_string(output)?;
    let preserved_report = fs::read_to_string(file_report)?;

    assert!(result.is_err_and(|error| error.to_string().contains("distinct paths")));
    assert_eq!(preserved_diagram, EXISTING_CONTENT);
    assert_eq!(preserved_report, EXISTING_CONTENT);
    assert_eq!(diagram, [] as [u8; 0]);
    Ok(())
}
