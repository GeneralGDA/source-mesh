use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

use crate::application::Input;
use crate::application::Request;
use crate::application::run;
use crate::export::Format;
use crate::export::Options;
use crate::scip_backend::parse_scip;

#[test]
fn test_saved_analysis_replay() -> Result<()> {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src").join("fixtures").join("dependency-graph.scip");
    let graph = parse_scip(&fs::read(&fixture)?)?;
    let root = PathBuf::from("src");
    let folders = graph.within_root(&root)?.folders(&root, Some(1), false)?;
    let mut expected = Vec::new();
    Format::Mermaid.exporter().write_with_options(
        &folders, &Options::new(true, None), &mut expected,
    )?;
    let system_under_test = Request::builder()
        .input(Input::ScipFile(fixture))
        .root(root)
        .format(Format::Mermaid)
        .depth(1)
        .group_folders(true)
        .build();
    let mut diagram = Vec::new();

    run(&system_under_test, &mut diagram)?;

    assert_eq!(diagram, expected);
    assert!(!diagram.is_empty());
    Ok(())
}

#[test]
fn test_replay_cannot_save_analysis() -> Result<()> {
    const EXISTING_CONTENT: &str = "preserved";
    let fixture = tempdir()?;
    let output = fixture.path().join("index.scip");
    fs::write(&output, EXISTING_CONTENT)?;
    let system_under_test = Request::builder()
        .input(Input::ScipFile(fixture.path().join("missing.scip")))
        .save_scip(output.clone())
        .build();
    let mut diagram = Vec::new();

    let result = run(&system_under_test, &mut diagram);
    let preserved = fs::read_to_string(output)?;

    assert!(result.is_err_and(|error| error.to_string().contains("Cannot save analysis")));
    assert_eq!(preserved, EXISTING_CONTENT);
    assert!(diagram.is_empty());
    Ok(())
}

#[test]
fn test_replay_cannot_overwrite_input() -> Result<()> {
    let fixture = tempdir()?;
    let input = fixture.path().join("index.scip");
    let bytes = include_bytes!("fixtures/dependency-graph.scip");
    fs::write(&input, bytes)?;
    let system_under_test = Request::builder()
        .input(Input::ScipFile(input.clone()))
        .output(input.clone())
        .build();
    let mut diagram = Vec::new();

    let result = run(&system_under_test, &mut diagram);
    let preserved = fs::read(input)?;

    assert!(result.is_err_and(|error| error.to_string().contains("must not overwrite input")));
    assert_eq!(preserved, bytes);
    assert!(diagram.is_empty());
    Ok(())
}
