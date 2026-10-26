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
use crate::outgoing_dependencies::Analysis as OutgoingDependencyAnalysis;
use crate::outgoing_dependencies::Mode as OutgoingDependencyMode;
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
pub struct OutgoingDependencyOptions {
    maximum: usize,
    #[builder(default = OutgoingDependencyMode::Folders)]
    mode: OutgoingDependencyMode,
    output: Option<PathBuf>,
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
    outgoing_dependencies: Option<OutgoingDependencyOptions>,
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
Cycle and outgoing dependency checks fail only after outputs have been written.
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
    let (outgoing_analysis, outgoing_report_output) = if let Some(options) = request.outgoing_dependencies.as_ref() {
        let analysis = match options.mode {
            OutgoingDependencyMode::Files => OutgoingDependencyAnalysis::files(&file_graph, options.maximum)?,
            OutgoingDependencyMode::Folders => OutgoingDependencyAnalysis::folders(&folder_graph, options.maximum),
        };
        let report = options.output.as_ref().map(|path| (path, analysis.markdown()));
        (Some(analysis), report)
    } else {
        (None, None)
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
    if let Some((path, ref report)) = outgoing_report_output {
        outputs.push(FileOutput::new(path, report.as_bytes()));
    }
    if let Some(path) = request.output.as_deref() {
        outputs.push(FileOutput::new(path, &diagram_bytes));
    }
    OutputFiles::open(&outputs, &protected_inputs)?.write()?;
    if request.output.is_none() {
        diagram_output.write_all(&diagram_bytes)?;
    }
    let mut failures = Vec::new();
    if cycles.fail_on_cycles && (folder_cycle_group_count != 0 || file_cycle_group_count != 0) {
        failures.push(format!(
            "Dependency cycles found: {folder_cycle_group_count} folder cycle group(s), \
             {file_cycle_group_count} file cycle group(s)"
        ));
    }
    if let Some(analysis) = outgoing_analysis && analysis.has_violations() {
        failures.push(analysis.diagnostic());
    }
    ensure!(failures.is_empty(), "{}", failures.join("\n\n"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use anyhow::Error;
    use claims::assert_ok;
    use claims::assert_some;
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
    use super::OutgoingDependencyOptions;
    use super::Request;
    use super::run;
    use crate::cycle_report::NO_CYCLES_FOUND;
    use crate::export::Format;
    use crate::export::Options;
    use crate::outgoing_dependencies::Mode;
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

    #[must_use]
    fn outgoing_dependency_index_fixture() -> Index {
        let targets = [
            ("src/target/first.rs", "first", 0),
            ("src/target/second.rs", "second", 0),
            ("src/target/deep/third.rs", "third", 0),
            ("src/testonly/test.rs", "test_only", SymbolRole::Test as i32),
            ("outside/external.rs", "external", 0),
        ];
        let mut documents = Vec::new();
        let mut references = Vec::new();
        for (relative_path, name, symbol_roles) in targets {
            let symbol = format!("rust-analyzer cargo fixture 0.0.0 {name}().");
            references.push(Occurrence { symbol: symbol.clone(), symbol_roles, ..Occurrence::default() });
            documents.push(Document {
                relative_path: relative_path.into(),
                occurrences: vec![Occurrence { symbol, symbol_roles: SymbolRole::Definition as i32, ..Occurrence::default() }],
                ..Document::default()
            });
        }
        documents.push(Document {
            relative_path: "outside/entry.rs".into(),
            occurrences: references.clone(),
            ..Document::default()
        });
        documents.push(Document { relative_path: "src/app/entry.rs".into(), occurrences: references, ..Document::default() });
        Index { documents, ..Index::default() }
    }

    #[rstest]
    #[case::files_all(Mode::Files, ".", false, None, &[("outside/entry.rs", 5), ("src/app/entry.rs", 5)])]
    #[case::folders_all(Mode::Folders, ".", false, None, &[("outside", 3), ("src/app", 4)])]
    #[case::files_root(Mode::Files, "src", false, None, &[("src/app/entry.rs", 4)])]
    #[case::folders_root(Mode::Folders, "src", false, None, &[("app", 3)])]
    #[case::files_production(Mode::Files, "src", true, None, &[("src/app/entry.rs", 3)])]
    #[case::folders_production(Mode::Folders, "src", true, None, &[("app", 2)])]
    #[case::files_folded(Mode::Files, "src", true, Some(1), &[("src/app/entry.rs", 3)])]
    #[case::folders_folded(Mode::Folders, "src", true, Some(1), &[])]
    #[case::folders_folded_with_tests(Mode::Folders, "src", false, Some(1), &[("app", 2)])]
    fn test_outgoing_dependency_graph_selection(
        #[case] mode: Mode,
        #[case] root: &str,
        #[case] exclude_tests: bool,
        #[case] depth: Option<usize>,
        #[case] expected: &[(&str, usize)],
    ) {
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Outgoing dependency fixture directory must be created");
        let input = fixture.path().join("index.scip");
        let report_path = fixture.path().join("outgoing.md");
        let bytes = outgoing_dependency_index_fixture().write_to_bytes();
        let bytes = assert_ok!(bytes, "Outgoing dependency fixture must serialize");
        let written = fs::write(&input, bytes);
        assert_ok!(written, "Outgoing dependency index must be written");
        let options = OutgoingDependencyOptions::builder().maximum(1).mode(mode).output(report_path.clone()).build();
        let request = Request::builder().input(Input::ScipFile(input)).root(PathBuf::from(root)).exclude_tests(exclude_tests);
        let system_under_test = request.maybe_depth(depth).outgoing_dependencies(options).build();
        let expected_lines: Vec<_> = expected.iter().map(|&(label, count)| {
            let label = match mode {
                Mode::Files => PathBuf::from_iter(label.split('/')).to_string_lossy().replace('\\', "&#92;"),
                Mode::Folders => label.to_owned(),
            };
            format!("- <code>{label}</code>: {count} outgoing dependencies (maximum: 1)")
        }).collect();
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);
        let report = fs::read_to_string(report_path);
        let report = assert_ok!(report, "Outgoing report must be written before a threshold failure");
        let actual_lines: Vec<_> = report.lines().filter(|line| line.starts_with("- ")).collect();

        assert_eq!(actual_lines, expected_lines, "Report must reflect the selected graph after filtering and folding");
        assert_eq!(result.is_err(), !expected.is_empty(), "Threshold result disagrees with expected sources: {result:?}");
        if let Err(error) = result {
            assert!(error.to_string().contains("Outgoing dependency threshold exceeded"), "Unexpected failure: {error}");
        }
        if expected.is_empty() {
            assert!(report.contains("No outgoing dependency violations"), "Passing reports must state that no sources violate");
        }
        let mode_label = match mode { Mode::Files => "files", Mode::Folders => "folders" };
        assert!(report.contains(&format!("Mode: {mode_label}")), "Report must identify the selected mode");
        assert!(report.contains("Maximum: 1"), "Report must include the requested maximum");
        assert!(!diagram.is_empty(), "Graph checks must run after diagram output is written");
    }

    #[rstest]
    #[case::disabled(false, None, true, 0, false)]
    #[case::default_folders(true, None, false, 0, false)]
    #[case::default_folder_self(true, None, true, 0, true)]
    #[case::files_without_self(true, Some(Mode::Files), false, 0, true)]
    #[case::files_with_self(true, Some(Mode::Files), true, 0, true)]
    #[case::folder_equality(true, Some(Mode::Folders), true, 1, false)]
    #[case::file_equality(true, Some(Mode::Files), false, 1, false)]
    fn test_outgoing_dependency_gate_and_outputs(
        #[case] enabled: bool,
        #[case] mode: Option<Mode>,
        #[case] include_self: bool,
        #[case] maximum: usize,
        #[case] expected_failure: bool,
        #[values(false, true)] report_requested: bool,
    ) {
        const CONTENTS_EXISTING: &str = "preserved";
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Outgoing gate fixture directory must be created");
        let input = fixture.path().join("index.scip");
        let output = fixture.path().join("diagram.dot");
        let report_path = fixture.path().join("outgoing.md");
        let bytes = two_file_index_fixture(false).write_to_bytes();
        let bytes = assert_ok!(bytes, "Same-folder dependency fixture must serialize");
        let index_write = fs::write(&input, bytes);
        assert_ok!(index_write, "Same-folder dependency index must be written");
        let diagram_write = fs::write(&output, CONTENTS_EXISTING);
        assert_ok!(diagram_write, "Existing diagram must be initialized before the outgoing gate");
        let write_report = enabled && report_requested;
        if write_report {
            let report_write = fs::write(&report_path, CONTENTS_EXISTING);
            assert_ok!(report_write, "Existing outgoing report must be initialized before the gate");
        }
        let options = OutgoingDependencyOptions::builder().maximum(maximum);
        let options = options.maybe_output(report_requested.then(|| report_path.clone()));
        let options = match mode { Some(mode) => options.mode(mode).build(), None => options.build() };
        let request = Request::builder().input(Input::ScipFile(input)).root(PathBuf::from("src")).include_self(include_self);
        let system_under_test = request.output(output.clone()).maybe_outgoing_dependencies(enabled.then_some(options)).build();
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);
        let exported = fs::read_to_string(output);
        let exported = assert_ok!(exported, "Diagram must be readable after the outgoing gate");
        let report_exists = report_path.try_exists();
        let report_exists = assert_ok!(report_exists, "Outgoing report path must be inspectable");

        assert_eq!(result.is_err(), expected_failure, "Unexpected outgoing threshold result: {result:?}");
        if let Err(error) = result {
            assert!(error.to_string().contains("Outgoing dependency threshold exceeded"), "Unexpected gate failure: {error}");
        }
        assert!(exported.contains("digraph"), "The requested diagram must replace existing output before gating");
        assert_eq!(diagram, [] as [u8; 0], "An output-file request must not also write the diagram to the supplied writer");
        assert_eq!(report_exists, write_report, "Only an enabled check with an output path should write a report");
        if write_report {
            let report = fs::read_to_string(report_path);
            let report = assert_ok!(report, "Requested outgoing report must remain readable after gating");
            let actual_lines: Vec<_> = report.lines().filter(|line| line.starts_with("- ")).collect();
            let expected_lines = if expected_failure {
                let label = if matches!(mode, Some(Mode::Files)) {
                    PathBuf::from("src").join("first.rs").to_string_lossy().replace('\\', "&#92;")
                } else {
                    String::from(".")
                };
                vec![format!("- <code>{label}</code>: 1 outgoing dependencies (maximum: {maximum})")]
            } else {
                Vec::new()
            };
            assert_eq!(actual_lines, expected_lines, "File targets must not inherit folder self-edge filtering");
            if !expected_failure {
                assert!(report.contains("No outgoing dependency violations"), "Equality and zero targets must pass the check");
            }
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
        #[values(false, true)] outgoing_requested: bool,
    ) {
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Cycle report fixture directory must be created");
        let input = fixture.path().join("index.scip");
        let file_report = fixture.path().join("files.md");
        let outgoing_report = fixture.path().join("outgoing.md");
        let bytes = two_file_index_fixture(cyclic).write_to_bytes();
        let bytes = assert_ok!(bytes, "Two-file cycle fixture must serialize");
        let written = fs::write(&input, bytes);
        assert_ok!(written, "Cycle fixture index must be written: {}", input.display());
        let cycles = CycleOptions::builder().fail_on_cycles(fail_on_cycles);
        let cycles = cycles.maybe_file_cycles_output(report_requested.then(|| file_report.clone())).build();
        let outgoing = outgoing_requested.then(|| {
            OutgoingDependencyOptions::builder().maximum(0).mode(Mode::Files).output(outgoing_report.clone()).build()
        });
        /* Same-folder files and disabled self-edges keep the folder gate from masking skipped file analysis. */
        let request = Request::builder().input(Input::ScipFile(input)).root(PathBuf::from("src")).include_self(false);
        let system_under_test = request.cycles(cycles).maybe_outgoing_dependencies(outgoing).build();
        let expected_error = expected_file_cycle_groups.map(|groups| {
            format!("Dependency cycles found: 0 folder cycle group(s), {groups} file cycle group(s)")
        });
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);

        let actual_error = result.err().map(|error| error.to_string());
        if outgoing_requested {
            let error = assert_some!(actual_error, "The zero outgoing maximum must reject the fixture's file dependencies");
            assert!(error.contains("Outgoing dependency threshold exceeded"), "Combined gate omitted the outgoing reason");
            assert_eq!(error.contains("Dependency cycles found:"), expected_error.is_some(), "Cycle checks are independent");
            if let Some(expected_error) = expected_error {
                assert!(error.contains(&expected_error), "Combined gate omitted the cycle reason: {error}");
            }
            let report = fs::read_to_string(outgoing_report);
            let report = assert_ok!(report, "Outgoing report must be written even when both dependency checks fail");
            let violations = report.lines().filter(|line| line.starts_with("- ")).count();
            assert_eq!(violations, if cyclic { 2 } else { 1 }, "Outgoing report must include every violating source");
            assert!(report.contains("first.rs</code>: 1 outgoing dependencies (maximum: 0)"), "First source count is missing");
            let second_source = report.contains("second.rs</code>: 1 outgoing dependencies (maximum: 0)");
            assert_eq!(second_source, cyclic, "Only a cyclic fixture has an outgoing dependency from the second source");
        } else {
            assert_eq!(actual_error, expected_error);
        }
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
    fn test_replay_cannot_overwrite_input(#[case] alias: bool, #[values(false, true)] outgoing_report: bool) {
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Protected input fixture directory must be created");
        let input = fixture.path().join("index.scip");
        let bytes = two_file_index_fixture(false).write_to_bytes();
        let bytes = assert_ok!(bytes, "Protected input fixture must serialize");
        let written = fs::write(&input, &bytes);
        assert_ok!(written, "Protected SCIP input must be initialized");
        let output = if alias {
            let output = fixture.path().join("alias.scip");
            let linked = fs::hard_link(&input, &output);
            assert_ok!(linked, "Output alias must refer to the protected input file");
            output
        } else {
            input.clone()
        };
        let request = Request::builder().input(Input::ScipFile(input.clone()));
        let system_under_test = if outgoing_report {
            let options = OutgoingDependencyOptions::builder().maximum(usize::MAX).output(output).build();
            request.outgoing_dependencies(options).build()
        } else {
            request.output(output).build()
        };
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);
        let preserved = fs::read(input);
        let preserved = assert_ok!(preserved, "Protected SCIP input must remain readable after replay rejection");

        assert!(result.as_ref().is_err_and(Error::is::<OutputPathConflict>), "Replay must protect input aliases: {result:?}");
        assert_eq!(preserved, bytes);
        assert_eq!(diagram, [] as [u8; 0]);
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::malformed(b"\xff")]
    fn test_invalid_index_preserves_outputs(#[case] bytes: &[u8], #[values(false, true)] existing_report: bool) {
        const CONTENTS_EXISTING: &str = "preserved";
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Invalid-index fixture directory must be created");
        let input = fixture.path().join("index.scip");
        let output = fixture.path().join("diagram.dot");
        let folder_report = fixture.path().join("folders.json");
        let file_report = fixture.path().join("files.json");
        let outgoing_report = fixture.path().join("outgoing.md");
        let input_write = fs::write(&input, bytes);
        assert_ok!(input_write, "Invalid SCIP bytes must be written for the rejection check");
        let diagram_write = fs::write(&output, CONTENTS_EXISTING);
        assert_ok!(diagram_write, "Existing diagram must be initialized before invalid-index replay");
        if existing_report {
            let report_write = fs::write(&outgoing_report, CONTENTS_EXISTING);
            assert_ok!(report_write, "Existing outgoing report must be initialized before invalid-index replay");
        }
        let outgoing = OutgoingDependencyOptions::builder().maximum(0).output(outgoing_report.clone()).build();
        let cycles = CycleOptions::builder().cycles_output(folder_report.clone());
        let cycles = cycles.file_cycles_output(file_report.clone()).build();
        let request = Request::builder().input(Input::ScipFile(input)).output(output.clone()).cycles(cycles);
        let system_under_test = request.outgoing_dependencies(outgoing).build();
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
        let outgoing_report_exists = outgoing_report.try_exists();
        let outgoing_report_exists = assert_ok!(outgoing_report_exists, "Outgoing report path must be inspectable");
        assert_eq!(outgoing_report_exists, existing_report, "Invalid input must not create an outgoing report");
        if existing_report {
            let preserved_report = fs::read_to_string(outgoing_report);
            let preserved_report = assert_ok!(preserved_report, "Invalid input must leave the existing outgoing report readable");
            assert_eq!(preserved_report, CONTENTS_EXISTING, "Invalid input must not replace an outgoing report");
        }
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
    fn test_conflicting_outputs_preserve_files(#[case] alias: bool, #[values(false, true)] outgoing_report: bool) {
        const CONTENTS_EXISTING: &str = "preserved";
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Conflicting-output fixture directory must be created");
        let input = fixture.path().join("index.scip");
        let output = fixture.path().join("diagram.dot");
        let file_report = fixture.path().join("files.json");
        let bytes = two_file_index_fixture(false).write_to_bytes();
        let bytes = assert_ok!(bytes, "Conflicting-output fixture must serialize");
        let input_write = fs::write(&input, bytes);
        assert_ok!(input_write, "Valid SCIP input must be initialized before output conflict validation");
        let diagram_write = fs::write(&output, CONTENTS_EXISTING);
        assert_ok!(diagram_write, "Existing diagram must be initialized before output conflict validation");
        let report_write = fs::write(&file_report, CONTENTS_EXISTING);
        assert_ok!(report_write, "File-cycle report must be written before testing output conflicts");
        let conflicting_report = if alias {
            let report = fixture.path().join("conflict.md");
            let linked = fs::hard_link(&output, &report);
            assert_ok!(linked, "Conflicting report alias must refer to the existing diagram");
            report
        } else {
            output.clone()
        };
        let cycles = CycleOptions::builder().file_cycles_output(file_report.clone());
        let request = Request::builder().input(Input::ScipFile(input)).output(output.clone());
        let system_under_test = if outgoing_report {
            let outgoing = OutgoingDependencyOptions::builder().maximum(usize::MAX).output(conflicting_report).build();
            request.cycles(cycles.build()).outgoing_dependencies(outgoing).build()
        } else {
            request.cycles(cycles.cycles_output(conflicting_report).build()).build()
        };
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);
        let preserved_diagram = fs::read_to_string(output);
        let preserved_diagram = assert_ok!(preserved_diagram, "Diagram must remain readable after output conflicts");
        let preserved_report = fs::read_to_string(file_report);
        let preserved_report = assert_ok!(preserved_report, "File report must remain readable after output conflicts");

        assert!(result.as_ref().is_err_and(Error::is::<OutputPathConflict>), "Replay must reject output aliases: {result:?}");
        assert_eq!(preserved_diagram, CONTENTS_EXISTING);
        assert_eq!(preserved_report, CONTENTS_EXISTING);
        assert_eq!(diagram, [] as [u8; 0]);
    }
}
