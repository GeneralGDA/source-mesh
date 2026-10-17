use std::ffi::OsStr;
use std::fs;
use std::path::Path;

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
#[case::malformed_index(true, Path::new("."))]
#[case::outside_root(false, Path::new(".."))]
fn test_invalid_input_output_preservation(#[case] malformed_index: bool, #[case] root: &Path) -> Result<()> {
    const EXISTING_OUTPUT: &str = "previous diagram";
    let fixture = tempdir()?;
    let index_path = fixture.path().join("input.scip");
    let diagram_path = fixture.path().join("diagram.dot");
    let index = if malformed_index {
        b"this is not protobuf".to_vec()
    } else {
        acyclic_index_fixture()?
    };
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

    assert!(result.is_err(), "Command accepted an invalid index or subtree");
    if let Err(error) = result {
        assert!(!error.to_string().is_empty(), "Command omitted invalid-input diagnostics");
    }
    assert!(output.is_empty(), "Command wrote to stdout for invalid input");
    assert_eq!(preserved, EXISTING_OUTPUT, "Command overwrote the existing diagram");
    Ok(())
}

fn acyclic_index_fixture() -> Result<Vec<u8>> {
    Ok(Index {
        documents: vec![Document {
            relative_path: "src/item.rs".into(),
            ..Document::default()
        }],
        ..Index::default()
    }.write_to_bytes()?)
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
fn test_cycle_limit_output_preservation(
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

    assert!(result.is_err(), "Command accepted more cycles than the limit");
    if let Err(error) = result {
        assert!(error.to_string().contains("Cycle count exceeds"), "Command failed before checking the cycle limit: {error:#}");
    }
    assert!(output.is_empty(), "Command wrote to stdout before checking the cycle limit");
    for path in [&diagram_path, &folder_report_path, &file_report_path] {
        let preserved = fs::read_to_string(path)?;
        assert_eq!(preserved, EXISTING_OUTPUT, "Cycle limit error overwrote {}", path.display());
    }
    Ok(())
}

#[rstest]
#[case::report_cycles(false)]
#[case::cycle_gate(true)]
fn test_cyclic_graph_outputs(#[case] fail_on_cycles: bool, #[values(false, true)] diagram_to_file: bool) -> Result<()> {
    let fixture = tempdir()?;
    let index_path = fixture.path().join("input.scip");
    let diagram_path = fixture.path().join("diagram.graphml");
    let folder_report_path = fixture.path().join("folders.md");
    let file_report_path = fixture.path().join("files.md");
    fs::write(&index_path, cyclic_index_fixture()?)?;
    let mut system_under_test = Cli::try_parse_from([
        OsStr::new(env!("CARGO_PKG_NAME")),
        OsStr::new("--from-scip"), index_path.as_os_str(),
        OsStr::new("--format"), OsStr::new("graphml"),
        OsStr::new("--highlight-cycles"),
        OsStr::new("--cycles-output"), folder_report_path.as_os_str(),
        OsStr::new("--file-cycles-output"), file_report_path.as_os_str(),
    ])?;
    system_under_test.cycles.fail_on_cycles = fail_on_cycles;
    system_under_test.output = diagram_to_file.then(|| diagram_path.clone());
    let mut output = Vec::new();

    let result = run(system_under_test, &mut output);
    let standard_output_is_empty = output.is_empty();
    let diagram = if diagram_to_file { fs::read_to_string(&diagram_path)? } else { String::from_utf8(output)? };
    let folder_report = fs::read_to_string(&folder_report_path)?;
    let file_report = fs::read_to_string(&file_report_path)?;

    assert_eq!(result.is_err(), fail_on_cycles, "Cycles must fail only when explicitly requested: {result:?}");
    if let Err(error) = result {
        assert!(error.to_string().contains("Dependency cycles found:"), "Cycle gate failed for an unrelated reason: {error:#}");
    }
    assert_eq!(standard_output_is_empty, diagram_to_file, "Diagram output did not follow the selected destination");
    assert!(diagram.contains("cycle_group"), "Cyclic graph did not produce a highlighted diagram");
    assert!(folder_report.contains("## C1 ("), "Cyclic graph did not produce a folder cycle report");
    assert!(file_report.contains("## C1 ("), "Cyclic graph did not produce a file cycle report");
    Ok(())
}

#[test]
fn test_acyclic_graph_cycle_gate() -> Result<()> {
    let fixture = tempdir()?;
    let index_path = fixture.path().join("input.scip");
    fs::write(&index_path, acyclic_index_fixture()?)?;
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
