use std::io::Write;
use std::num::NonZeroUsize;
use std::path::PathBuf;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use bon::Builder;
use derive_new::new;
use protobuf::Message as _;
use tempfile::NamedTempFile;

use crate::analyzer::options::Options as AnalyzerOptions;
use crate::backend::RustAnalyzer;
use crate::cycle_report::file_report;
use crate::cycle_report::folder_report;
use crate::cycles::analyze_files;
use crate::cycles::analyze_folders;
use crate::export::Format;
use crate::export::Options as ExportOptions;
use crate::output_paths::FileOutput;
use crate::output_paths::InputFile;
use crate::output_paths::OutputFiles;
use crate::scip::backend::file_graph;
use crate::scip::index::ParsedIndex;

/// Applies independently to the file-cycle and folder-cycle reports.
pub const DEFAULT_CYCLE_LIMIT: NonZeroUsize = match NonZeroUsize::new(10_000) {
    Some(limit) => limit,
    None => unreachable!(),
};

#[derive(new)]
pub struct Project {
    path: PathBuf,
    rust_analyzer_configuration: Option<PathBuf>,
}

pub enum Input {
    Project(Project),
    ScipFile(PathBuf),
}

#[derive(Builder)]
pub struct CycleOptions {
    #[builder(default)]
    highlight_cycles: bool,
    #[builder(default)]
    fail_on_cycles: bool,
    cycles_output: Option<PathBuf>,
    file_cycles_output: Option<PathBuf>,
    #[builder(default = DEFAULT_CYCLE_LIMIT)]
    cycle_limit: NonZeroUsize,
}

#[derive(Builder)]
pub struct Request {
    input: Input,
    #[builder(default = PathBuf::from("."))]
    root: PathBuf,
    #[builder(default = Format::Dot)]
    format: Format,
    output: Option<PathBuf>,
    depth: Option<usize>,
    #[builder(default)]
    include_self: bool,
    #[builder(default)]
    group_folders: bool,
    #[builder(default)]
    exclude_tests: bool,
    #[builder(default = CycleOptions::builder().build())]
    cycles: CycleOptions,
    save_scip: Option<PathBuf>,
    #[builder(default = AnalyzerOptions::new(false))]
    analyzer: AnalyzerOptions,
}

/**
Analyze and export dependencies, writing the diagram to the supplied writer
unless the request selects an output file.

Analysis and serialization failures leave existing output files unchanged.
Output files must be distinct and must not overwrite input files.
Opening failures can leave new empty files; writing failures can leave partial outputs.
Cycle gating fails only after outputs have been written.
*/
pub fn run(request: &Request, diagram_output: &mut dyn Write) -> Result<()> {
    ensure!(
        !matches!(request.input, Input::ScipFile(_)) || request.save_scip.is_none(),
        "Cannot save analysis when replaying a SCIP file"
    );
    let mut protected_inputs = Vec::new();
    let index = match request.input {
        Input::ScipFile(ref path) => {
            let mut input = InputFile::open(path)?;
            let bytes = input.read()?;
            protected_inputs.push(input);
            ParsedIndex::parse(&bytes).context("Cannot load saved dependency analysis")?
        }
        Input::Project(ref project) => {
            let configuration = project.rust_analyzer_configuration.as_deref().map(|path| -> Result<_> {
                let mut input = InputFile::open(path)?;
                let mut snapshot = NamedTempFile::new().context("Cannot create analysis configuration snapshot")?;
                snapshot.write_all(&input.read()?).context("Cannot write analysis configuration snapshot")?;
                protected_inputs.push(input);
                Ok(snapshot)
            });
            let configuration = configuration.transpose()?;
            let configuration_path = configuration.as_ref().map(NamedTempFile::path);
            RustAnalyzer::new(&project.path, configuration_path, &request.analyzer).index_parsed()?
        }
    };
    let file_graph = file_graph(&index)?;
    let file_graph = if request.exclude_tests { file_graph.without_test_dependencies() } else { file_graph };
    let file_graph = file_graph.within_root(&request.root)?;
    let folder_graph = file_graph.folders(&request.root, request.depth, request.include_self)?;
    ensure!(!folder_graph.nodes().is_empty(), "No indexed Rust files under root {}", request.root.display());
    let cycles = &request.cycles;
    let analyze_folder_cycles = cycles.highlight_cycles || cycles.cycles_output.is_some() || cycles.fail_on_cycles;
    let (highlight_analysis, folder_report_output, folder_cycle_group_count) = if analyze_folder_cycles {
        let analysis = analyze_folders(&folder_graph)?;
        let report = cycles.cycles_output.as_ref().map(|path| {
            folder_report(&analysis, cycles.cycle_limit).map(|report| (path, report))
        });
        let report = report.transpose()?;
        let group_count = analysis.group_count();
        (cycles.highlight_cycles.then_some(analysis), report, group_count)
    } else {
        (None, None, 0)
    };
    let analyze_file_cycles = cycles.file_cycles_output.is_some() || cycles.fail_on_cycles;
    let (file_report_output, file_cycle_group_count) = if analyze_file_cycles {
        let analysis = analyze_files(&file_graph)?;
        let report = cycles.file_cycles_output.as_ref().map(|path| {
            file_report(&analysis, cycles.cycle_limit).map(|report| (path, report))
        });
        let report = report.transpose()?;
        (report, analysis.group_count())
    } else {
        (None, 0)
    };
    let mut diagram_bytes = Vec::new();
    let exporter = request.format.exporter();
    let options = ExportOptions::builder().group_folders(request.group_folders).maybe_cycles(highlight_analysis.as_ref()).build();
    exporter.write_with_options(&folder_graph, &options, &mut diagram_bytes)?;
    let saved_index = request.save_scip.as_ref().map(|path| {
        let serialized = index.into_index().write_to_bytes().context("Cannot serialize dependency analysis");
        serialized.map(|bytes| (path, bytes))
    });
    let saved_index = saved_index.transpose()?;
    let mut outputs = Vec::new();
    if let Some((path, ref bytes)) = saved_index {
        outputs.push(FileOutput::new(path, bytes));
    }
    if let Some((path, ref report)) = folder_report_output {
        outputs.push(FileOutput::new(path, report.as_bytes()));
    }
    if let Some((path, ref report)) = file_report_output {
        outputs.push(FileOutput::new(path, report.as_bytes()));
    }
    if let Some(path) = request.output.as_deref() {
        outputs.push(FileOutput::new(path, &diagram_bytes));
    }
    OutputFiles::open(&outputs, &protected_inputs)?.write()?;
    if request.output.is_none() {
        diagram_output.write_all(&diagram_bytes)?;
    }
    if cycles.fail_on_cycles {
        ensure!(
            folder_cycle_group_count == 0 && file_cycle_group_count == 0,
            "Dependency cycles found: {folder_cycle_group_count} folder cycle group(s), \
             {file_cycle_group_count} file cycle group(s)"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use anyhow::Error;
    use claims::assert_ok;
    use pretty_assertions::assert_eq;
    use protobuf::Message as _;
    use rstest::rstest;
    use scip::types::Document;
    use scip::types::Index;
    use scip::types::Occurrence;
    use scip::types::SymbolRole;
    use tempfile::tempdir;

    use super::CycleOptions;
    use super::Input;
    use super::Request;
    use super::run;
    use crate::cycle_report::NO_CYCLES_FOUND;
    use crate::export::Format;
    use crate::export::Options;
    use crate::output_paths::OutputPathConflict;
    use crate::scip::backend::parse_scip;

    #[test]
    fn test_saved_analysis_replay() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src").join("fixtures").join("dependency-graph.scip");
        let index = fs::read(&fixture);
        let index = assert_ok!(index, "Saved SCIP fixture must be readable: {}", fixture.display());
        let graph = parse_scip(&index);
        let graph = assert_ok!(graph, "Saved SCIP fixture must contain a valid dependency graph");
        let root = PathBuf::from("src");
        let within_root = graph.within_root(&root);
        let within_root = assert_ok!(within_root, "Fixture graph must support the selected source root");
        let folders = within_root.folders(&root, Some(1), false);
        let folders = assert_ok!(folders, "Fixture graph must support depth-limited folder aggregation");
        let mut expected = Vec::new();
        let export = Format::Mermaid.exporter()
            .write_with_options(&folders, &Options::builder().group_folders(true).build(), &mut expected);
        assert_ok!(export, "Expected grouped Mermaid diagram must render successfully");
        let request = Request::builder().input(Input::ScipFile(fixture)).root(root).format(Format::Mermaid);
        let system_under_test = request.depth(1).group_folders(true).build();
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);

        assert_ok!(result, "Replaying saved analysis must produce a dependency diagram");
        assert_eq!(diagram, expected);
        assert_ne!(diagram, [] as [u8; 0]);
    }

    #[must_use]
    fn two_file_index_fixture(cyclic: bool) -> Index {
        let first_symbol = "rust-analyzer cargo fixture 0.0.0 first().";
        let second_symbol = "rust-analyzer cargo fixture 0.0.0 second().";
        let mut second_occurrences = vec![Occurrence {
            range: vec![0, 3, 9],
            symbol: second_symbol.into(),
            symbol_roles: SymbolRole::Definition as i32,
            ..Occurrence::default()
        }];
        if cyclic {
            second_occurrences.push(Occurrence { range: vec![1, 0, 5], symbol: first_symbol.into(), ..Occurrence::default() });
        }
        Index {
            documents: vec![
                Document {
                    relative_path: "src/first.rs".into(),
                    occurrences: vec![
                        Occurrence {
                            range: vec![0, 3, 8],
                            symbol: first_symbol.into(),
                            symbol_roles: SymbolRole::Definition as i32,
                            ..Occurrence::default()
                        },
                        Occurrence { range: vec![1, 0, 6], symbol: second_symbol.into(), ..Occurrence::default() },
                    ],
                    ..Document::default()
                },
                Document {
                    relative_path: "src/second.rs".into(),
                    occurrences: second_occurrences,
                    ..Document::default()
                },
            ],
            ..Index::default()
        }
    }

    #[rstest]
    #[case::cyclic_neither(true, false, false, None)]
    #[case::cyclic_report_only(true, true, false, None)]
    #[case::cyclic_gate_only(true, false, true, Some(1))]
    #[case::cyclic_report_and_gate(true, true, true, Some(1))]
    #[case::acyclic_neither(false, false, false, None)]
    #[case::acyclic_report_only(false, true, false, None)]
    #[case::acyclic_gate_only(false, false, true, None)]
    #[case::acyclic_report_and_gate(false, true, true, None)]
    fn test_file_cycle_report_and_gate_flags(
        #[case] cyclic: bool,
        #[case] report_requested: bool,
        #[case] fail_on_cycles: bool,
        #[case] expected_file_cycle_groups: Option<usize>,
    ) {
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Cycle report fixture directory must be created");
        let input = fixture.path().join("index.scip");
        let file_report = fixture.path().join("files.md");
        let bytes = two_file_index_fixture(cyclic).write_to_bytes();
        let bytes = assert_ok!(bytes, "Two-file cycle fixture must serialize");
        let written = fs::write(&input, bytes);
        assert_ok!(written, "Cycle fixture index must be written: {}", input.display());
        let cycles = CycleOptions::builder().fail_on_cycles(fail_on_cycles);
        let cycles = cycles.maybe_file_cycles_output(report_requested.then(|| file_report.clone())).build();
        /* Same-folder files and disabled self-edges keep the folder gate from masking skipped file analysis. */
        let request = Request::builder().input(Input::ScipFile(input)).root(PathBuf::from("src")).include_self(false);
        let system_under_test = request.cycles(cycles).build();
        let expected_error = expected_file_cycle_groups.map(|groups| {
            format!("Dependency cycles found: 0 folder cycle group(s), {groups} file cycle group(s)")
        });
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);

        assert_eq!(result.err().map(|error| error.to_string()), expected_error);
        assert!(!diagram.is_empty(), "Cycle gating must happen after diagram output is written");
        let report_exists = file_report.try_exists();
        let report_exists = assert_ok!(report_exists, "File-cycle report path must be inspectable");
        assert_eq!(report_exists, report_requested, "Only a requested file-cycle report should be written");
        if report_requested {
            let report = fs::read_to_string(file_report);
            let report = assert_ok!(report, "Requested file-cycle report must be readable");
            assert_eq!(report.lines().next(), Some("# File dependency cycles"));
            let cycle_groups = report.lines().filter(|line| line.starts_with("## ")).count();
            assert_eq!(cycle_groups, usize::from(cyclic));
            if cyclic {
                assert!(report.contains("first.rs</code>"), "File-cycle report omitted the first file");
                assert!(report.contains("second.rs</code>"), "File-cycle report omitted the second file");
                assert!(!report.contains(NO_CYCLES_FOUND), "File-cycle report incorrectly claims there are no cycles");
            } else {
                assert_eq!(report.lines().last(), Some(NO_CYCLES_FOUND));
            }
        }
    }

    #[test]
    fn test_replay_cannot_save_analysis() {
        const CONTENTS_EXISTING: &str = "preserved";
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Replay output fixture directory must be created");
        let output = fixture.path().join("index.scip");
        let written = fs::write(&output, CONTENTS_EXISTING);
        assert_ok!(written, "Existing analysis output must be initialized");
        let input = Input::ScipFile(fixture.path().join("missing.scip"));
        let system_under_test = Request::builder().input(input).save_scip(output.clone()).build();
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);
        let preserved = fs::read_to_string(output);
        let preserved = assert_ok!(preserved, "Rejected replay must leave the existing analysis output readable");

        assert!(
            result.as_ref().is_err_and(|error| error.to_string().contains("Cannot save analysis")),
            "Replay must reject saving a new analysis: {result:?}"
        );
        assert_eq!(preserved, CONTENTS_EXISTING);
        assert_eq!(diagram, [] as [u8; 0]);
    }

    #[rstest]
    #[case::same_path(false)]
    #[case::hard_link(true)]
    fn test_replay_cannot_overwrite_input(#[case] alias: bool) {
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Protected input fixture directory must be created");
        let input = fixture.path().join("index.scip");
        let bytes = include_bytes!("fixtures/dependency-graph.scip");
        let written = fs::write(&input, bytes);
        assert_ok!(written, "Protected SCIP input must be initialized");
        let output = if alias {
            let output = fixture.path().join("alias.scip");
            let linked = fs::hard_link(&input, &output);
            assert_ok!(linked, "Output alias must refer to the protected input file");
            output
        } else {
            input.clone()
        };
        let system_under_test = Request::builder().input(Input::ScipFile(input.clone())).output(output).build();
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);
        let preserved = fs::read(input);
        let preserved = assert_ok!(preserved, "Protected SCIP input must remain readable after replay rejection");

        assert!(
            result.as_ref().is_err_and(Error::is::<OutputPathConflict>),
            "Replay must reject overwriting its input through either path spelling: {result:?}"
        );
        assert_eq!(preserved, bytes);
        assert_eq!(diagram, [] as [u8; 0]);
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::malformed(b"\xff")]
    fn test_invalid_index_preserves_outputs(#[case] bytes: &[u8]) {
        const CONTENTS_EXISTING: &str = "preserved";
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Invalid-index fixture directory must be created");
        let input = fixture.path().join("index.scip");
        let output = fixture.path().join("diagram.dot");
        let folder_report = fixture.path().join("folders.json");
        let file_report = fixture.path().join("files.json");
        let input_write = fs::write(&input, bytes);
        assert_ok!(input_write, "Invalid SCIP bytes must be written for the rejection check");
        let diagram_write = fs::write(&output, CONTENTS_EXISTING);
        assert_ok!(diagram_write, "Existing diagram must be initialized before invalid-index replay");
        let cycles = CycleOptions::builder().cycles_output(folder_report.clone());
        let cycles = cycles.file_cycles_output(file_report.clone()).build();
        let system_under_test = Request::builder().input(Input::ScipFile(input)).output(output.clone()).cycles(cycles).build();
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);
        let preserved = fs::read_to_string(output);
        let preserved = assert_ok!(preserved, "Existing diagram must remain readable after invalid-index rejection");

        assert!(result.is_err(), "Malformed external analysis must return an error");
        assert_eq!(preserved, CONTENTS_EXISTING);
        let folder_report_exists = folder_report.try_exists();
        let folder_report_exists = assert_ok!(folder_report_exists, "Folder report path must be inspectable");
        assert!(!folder_report_exists, "Invalid input created a folder report");
        let file_report_exists = file_report.try_exists();
        let file_report_exists = assert_ok!(file_report_exists, "File report path must be inspectable");
        assert!(!file_report_exists, "Invalid input created a file report");
        assert_eq!(diagram, [] as [u8; 0]);
    }

    #[rstest]
    #[case::empty("")]
    #[case::parent("../outside.rs")]
    #[case::absolute("/absolute.rs")]
    fn test_invalid_source_path_preserves_outputs(#[case] path: &str) {
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Invalid-source-path fixture directory must be created");
        let input = fixture.path().join("index.scip");
        let output = fixture.path().join("diagram.dot");
        let index = Index { documents: vec![Document { relative_path: path.into(), ..Document::default() }], ..Index::default() };
        let bytes = index.write_to_bytes();
        let bytes = assert_ok!(bytes, "Index with an invalid source path must serialize before validation");
        let written = fs::write(&input, bytes);
        assert_ok!(written, "Invalid-source-path index must be written");
        let system_under_test = Request::builder().input(Input::ScipFile(input)).output(output.clone()).build();
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);

        assert!(result.is_err(), "Malformed external source paths must return an error");
        let output_exists = output.try_exists();
        let output_exists = assert_ok!(output_exists, "Diagram output path must be inspectable");
        assert!(!output_exists, "Invalid input created a diagram");
        assert_eq!(diagram, [] as [u8; 0]);
    }

    #[rstest]
    #[case::same_path(false)]
    #[case::hard_link(true)]
    fn test_conflicting_outputs_preserve_files(#[case] alias: bool) {
        const CONTENTS_EXISTING: &str = "preserved";
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Conflicting-output fixture directory must be created");
        let input = fixture.path().join("index.scip");
        let output = fixture.path().join("diagram.dot");
        let file_report = fixture.path().join("files.json");
        let input_write = fs::write(&input, include_bytes!("fixtures/dependency-graph.scip"));
        assert_ok!(input_write, "Valid SCIP input must be initialized before output conflict validation");
        let diagram_write = fs::write(&output, CONTENTS_EXISTING);
        assert_ok!(diagram_write, "Existing diagram must be initialized before output conflict validation");
        let report_write = fs::write(&file_report, CONTENTS_EXISTING);
        assert_ok!(report_write, "Existing file-cycle report must be initialized before output conflict validation");
        let folder_report = if alias {
            let report = fixture.path().join("folders.json");
            let linked = fs::hard_link(&output, &report);
            assert_ok!(linked, "Folder report alias must refer to the existing diagram");
            report
        } else {
            output.clone()
        };
        let cycles = CycleOptions::builder().cycles_output(folder_report).file_cycles_output(file_report.clone()).build();
        let system_under_test = Request::builder().input(Input::ScipFile(input)).output(output.clone()).cycles(cycles).build();
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);
        let preserved_diagram = fs::read_to_string(output);
        let preserved_diagram = assert_ok!(preserved_diagram, "Diagram must remain readable after output conflict rejection");
        let preserved_report = fs::read_to_string(file_report);
        let preserved_report = assert_ok!(preserved_report, "File report must remain readable after output conflict rejection");

        assert!(
            result.as_ref().is_err_and(Error::is::<OutputPathConflict>),
            "Replay must reject output files referring to the same file: {result:?}"
        );
        assert_eq!(preserved_diagram, CONTENTS_EXISTING);
        assert_eq!(preserved_report, CONTENTS_EXISTING);
        assert_eq!(diagram, [] as [u8; 0]);
    }
}
