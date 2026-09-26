use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use anyhow::Context as _;
use anyhow::Result;
use clap::Parser as _;
use pretty_assertions::assert_eq;
use protobuf::Enum as _;
use protobuf::Message as _;
use rstest::rstest;
use scip::types::Document;
use scip::types::Index;
use scip::types::Occurrence;
use scip::types::SymbolRole;
use tempfile::tempdir;

use crate::Cli;
use crate::run;

#[rstest]
#[case::malformed_index(b"this is not protobuf", Path::new("."))]
#[case::outside_root(include_bytes!("fixtures/dependency-graph.scip"), Path::new(".."))]
fn test_invalid_input_preserves_output(#[case] index: &[u8], #[case] root: &Path) -> Result<()> {
    const EXISTING_OUTPUT: &str = "previous diagram";
    let fixture = tempdir()?;
    let index_path = fixture.path().join("input.scip");
    let diagram_path = fixture.path().join("diagram.dot");
    fs::write(&index_path, index)?;
    fs::write(&diagram_path, EXISTING_OUTPUT)?;
    let system_under_test = Cli::try_parse_from([
        OsStr::new(env!("CARGO_PKG_NAME")),
        OsStr::new("--from-scip"), index_path.as_os_str(),
        OsStr::new("--root"), root.as_os_str(),
        OsStr::new("--output"), diagram_path.as_os_str(),
    ])?;
    let mut output = Vec::new();

    let result = run(system_under_test, &mut output);
    let preserved = fs::read_to_string(&diagram_path)?;

    let error = result.err().context("Command accepted an invalid index or subtree")?;
    assert!(!error.to_string().is_empty(), "Command omitted invalid-input diagnostics");
    assert!(output.is_empty(), "Command wrote to stdout for invalid input");
    assert_eq!(preserved, EXISTING_OUTPUT, "Command overwrote the existing diagram");
    Ok(())
}

fn cyclic_index_fixture() -> Result<Vec<u8>> {
    let documents = [
        ("first/item.rs", &["second/item.rs"][..]),
        ("second/item.rs", &["first/item.rs", "third/item.rs"]),
        ("third/item.rs", &["second/item.rs"]),
    ].into_iter().map(|(path, references)| {
        let mut occurrences = vec![Occurrence {
            symbol: path.into(),
            symbol_roles: SymbolRole::Definition.value(),
            ..Occurrence::default()
        }];
        occurrences.extend(references.iter().map(|&target| Occurrence {
            symbol: target.into(),
            ..Occurrence::default()
        }));
        Document { relative_path: path.into(), occurrences, ..Document::default() }
    }).collect();
    Ok(Index { documents, ..Index::default() }.write_to_bytes()?)
}

#[rstest]
#[case::folder_report(None)]
#[case::file_report(Some(0))]
fn test_cycle_limit_preserves_outputs(
    #[case] depth: Option<usize>,
    #[values(false, true)] diagram_to_file: bool,
) -> Result<()> {
    const EXISTING_OUTPUT: &str = "previous result";
    let fixture = tempdir()?;
    let index_path = fixture.path().join("input.scip");
    let diagram_path = fixture.path().join("diagram.dot");
    let folder_report_path = fixture.path().join("folders.md");
    let file_report_path = fixture.path().join("files.md");
    fs::write(&index_path, cyclic_index_fixture()?)?;
    for path in [&diagram_path, &folder_report_path, &file_report_path] {
        fs::write(path, EXISTING_OUTPUT)?;
    }
    let mut system_under_test = Cli::try_parse_from([
        OsStr::new(env!("CARGO_PKG_NAME")),
        OsStr::new("--from-scip"), index_path.as_os_str(),
        OsStr::new("--cycle-limit"), OsStr::new("1"),
        OsStr::new("--cycles-output"), folder_report_path.as_os_str(),
        OsStr::new("--file-cycles-output"), file_report_path.as_os_str(),
    ])?;
    system_under_test.depth = depth;
    system_under_test.output = diagram_to_file.then(|| diagram_path.clone());
    let mut output = Vec::new();

    let result = run(system_under_test, &mut output);

    let error = result.err().context("Command accepted more cycles than the limit")?;
    assert!(error.to_string().contains("Cycle count exceeds"), "Command failed before checking the cycle limit: {error:#}");
    assert!(output.is_empty(), "Command wrote to stdout before checking the cycle limit");
    for path in [&diagram_path, &folder_report_path, &file_report_path] {
        let preserved = fs::read_to_string(path)?;
        assert_eq!(preserved, EXISTING_OUTPUT, "Cycle limit error overwrote {}", path.display());
    }
    Ok(())
}

#[test]
fn test_fail_on_cycles_writes_diagnostics_before_failing() -> Result<()> {
    let fixture = tempdir()?;
    let index_path = fixture.path().join("input.scip");
    let diagram_path = fixture.path().join("diagram.graphml");
    let folder_report_path = fixture.path().join("folders.md");
    let file_report_path = fixture.path().join("files.md");
    fs::write(&index_path, cyclic_index_fixture()?)?;
    let system_under_test = Cli::try_parse_from([
        OsStr::new(env!("CARGO_PKG_NAME")),
        OsStr::new("--from-scip"), index_path.as_os_str(),
        OsStr::new("--format"), OsStr::new("graphml"),
        OsStr::new("--highlight-cycles"),
        OsStr::new("--fail-on-cycles"),
        OsStr::new("--output"), diagram_path.as_os_str(),
        OsStr::new("--cycles-output"), folder_report_path.as_os_str(),
        OsStr::new("--file-cycles-output"), file_report_path.as_os_str(),
    ])?;
    let mut output = Vec::new();

    let result = run(system_under_test, &mut output);
    let diagram = fs::read_to_string(&diagram_path)?;
    let folder_report = fs::read_to_string(&folder_report_path)?;
    let file_report = fs::read_to_string(&file_report_path)?;

    assert!(result.is_err(), "Cycle gate accepted a cyclic graph");
    assert!(
        result.as_ref().err().is_some_and(|error| error.to_string().contains("Dependency cycles found:")),
        "Cycle gate did not explain why it failed"
    );
    assert!(output.is_empty(), "File export also wrote to stdout");
    assert!(diagram.contains("cycle_group"), "Cycle gate did not write a highlighted diagram");
    assert!(folder_report.contains("## C1 ("), "Cycle gate did not write a folder cycle report");
    assert!(file_report.contains("## C1 ("), "Cycle gate did not write a file cycle report");
    Ok(())
}

#[test]
fn test_fail_on_cycles_accepts_acyclic_graph() -> Result<()> {
    let fixture = tempdir()?;
    let index_path = fixture.path().join("input.scip");
    fs::write(&index_path, include_bytes!("fixtures/dependency-graph.scip"))?;
    let system_under_test = Cli::try_parse_from([
        OsStr::new(env!("CARGO_PKG_NAME")),
        OsStr::new("--from-scip"), index_path.as_os_str(),
        OsStr::new("--root"), OsStr::new("src"),
        OsStr::new("--fail-on-cycles"),
    ])?;
    let mut output = Vec::new();

    let result = run(system_under_test, &mut output);

    assert!(result.is_ok(), "Cycle gate rejected an acyclic graph: {result:?}");
    assert!(!output.is_empty(), "Cycle gate did not export the acyclic graph");
    Ok(())
}
