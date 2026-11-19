use std::io::Write;
use std::num::NonZeroUsize;
use std::path::PathBuf;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use bon::Builder;
use derive_new::new;
use nonzero_ext::nonzero;
use protobuf::Message as _;
use tempfile::NamedTempFile;

use crate::Exclusions;
use crate::analyzer::options::Options as AnalyzerOptions;
use crate::backend::RustAnalyzer;
use crate::cycle_report::file_report;
use crate::cycle_report::folder_report;
use crate::cycles::analyze_files;
use crate::cycles::analyze_folders;
use crate::diagnostics::emit;
use crate::export::Format;
use crate::export::Options as ExportOptions;
use crate::outgoing_dependencies::Analysis as OutgoingDependencyAnalysis;
use crate::outgoing_dependencies::Mode as OutgoingDependencyMode;
use crate::model::FileGraph;
use crate::output_paths::FileOutput;
use crate::output_paths::InputFile;
use crate::output_paths::OutputFiles;
use crate::scip::backend::file_graph;
use crate::scip::index::ParsedIndex;

const NO_FILES_AFTER_FILTERING: &str = "No indexed Rust files remain after applying root and file filters";

pub const DEFAULT_CYCLE_LIMIT: NonZeroUsize = nonzero!(10_000_usize);

#[derive(new)]
pub struct Project {
    path: PathBuf,
    rust_analyzer_configuration: Option<PathBuf>,
}

pub enum Input {
    Project(Project),
    ScipFile(PathBuf),
}

enum PreparedInput {
    Project(Project),
    ScipFile { index: ParsedIndex, protected_input: InputFile },
}

impl TryFrom<Input> for PreparedInput {
    type Error = anyhow::Error;

    fn try_from(input: Input) -> Result<Self> {
        match input {
            Input::Project(project) => Ok(Self::Project(project)),
            Input::ScipFile(path) => {
                let mut protected_input = InputFile::open(&path)?;
                let bytes = protected_input.read()?;
                let index = ParsedIndex::parse(&bytes).context("Cannot load saved dependency analysis")?;
                Ok(Self::ScipFile { index, protected_input })
            }
        }
    }
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

pub struct Request {
    input: PreparedInput,
    root: PathBuf,
    format: Format,
    output: Option<PathBuf>,
    depth: Option<usize>,
    include_self: bool,
    group_folders: bool,
    exclude_tests: bool,
    exclusions: Exclusions,
    cycles: CycleOptions,
    outgoing_dependencies: Option<OutgoingDependencyOptions>,
    save_scip: Option<PathBuf>,
    protected_inputs: Vec<InputFile>,
    analyzer: AnalyzerOptions,
}

#[bon::bon]
impl Request {
    #[builder]
    pub fn new(
        input: Input,
        #[builder(default = PathBuf::from("."))] root: PathBuf,
        #[builder(default = Format::Dot)] format: Format,
        output: Option<PathBuf>,
        depth: Option<usize>,
        #[builder(default)] include_self: bool,
        #[builder(default)] group_folders: bool,
        #[builder(default)] exclude_tests: bool,
        #[builder(default)] exclusions: Exclusions,
        #[builder(default = CycleOptions::builder().build())] cycles: CycleOptions,
        outgoing_dependencies: Option<OutgoingDependencyOptions>,
        save_scip: Option<PathBuf>,
        #[builder(default)] protected_inputs: Vec<InputFile>,
        #[builder(default = AnalyzerOptions::new(false))] analyzer: AnalyzerOptions,
    ) -> Result<Self> {
        ensure!(!matches!(input, Input::ScipFile(_)) || save_scip.is_none(), "Cannot save analysis when replaying a SCIP file");
        let input = PreparedInput::try_from(input)?;
        Ok(Self {
            input, root, format, output, depth, include_self, group_folders, exclude_tests, exclusions,
            cycles, outgoing_dependencies, save_scip, protected_inputs, analyzer,
        })
    }
}

pub fn run(request: &Request, diagram_output: &mut dyn Write) -> Result<()> {
    let mut retained_inputs = Vec::new();
    let (file_graph, saved_index, replay_input) = match request.input {
        PreparedInput::ScipFile { ref index, ref protected_input } => (file_graph(index)?, None, Some(protected_input)),
        PreparedInput::Project(ref project) => {
            let configuration = if let Some(path) = project.rust_analyzer_configuration.as_deref() {
                let mut input = InputFile::open(path)?;
                let snapshot = snapshot_analyzer_configuration(&mut input)?;
                retained_inputs.push(input);
                Some(snapshot)
            } else {
                None
            };
            let configuration_path = configuration.as_ref().map(NamedTempFile::path);
            let index = RustAnalyzer::new(&project.path, configuration_path, &request.analyzer).index_parsed()?;
            let graph = file_graph(&index)?;
            let saved_index = if let Some(path) = request.save_scip.as_deref() {
                let bytes = index.into_index().write_to_bytes().context("Cannot serialize dependency analysis")?;
                Some((path, bytes))
            } else {
                None
            };
            (graph, saved_index, None)
        }
    };
    let protected_inputs: Vec<_> = retained_inputs.iter().chain(&request.protected_inputs).chain(replay_input).collect();
    let saved_index_output = saved_index.as_ref().map(|&(path, ref bytes)| FileOutput::new(path, bytes));
    let outputs = write_analysis_outputs().request(request).file_graph(file_graph).protected_inputs(&protected_inputs);
    outputs.maybe_saved_index(saved_index_output).diagram_output(diagram_output).call()
}

fn snapshot_analyzer_configuration(input: &mut InputFile) -> Result<NamedTempFile> {
    let mut snapshot = NamedTempFile::new().context("Cannot create analysis configuration snapshot")?;
    let bytes = input.read()?;
    snapshot.write_all(&bytes).context("Cannot write analysis configuration snapshot")?;
    Ok(snapshot)
}

enum IndexCheckpoint<'a> {
    NotRequested,
    Saved { output_files: OutputFiles<'a> },
}

fn open_outputs_and_save_index<'a>(
    request: &'a Request, index_save_file: Option<FileOutput<'a>>, protected_inputs: &[&InputFile],
) -> Result<IndexCheckpoint<'a>> {
    let Some(index_save_file) = index_save_file else { return Ok(IndexCheckpoint::NotRequested); };
    let outgoing_report = request.outgoing_dependencies.as_ref().and_then(|options| options.output.as_deref());
    let empty_output = |path| FileOutput::new(path, &[]);
    let outputs = velcro::vec![
        index_save_file,
        ..request.cycles.cycles_output.as_deref().map(empty_output),
        ..request.cycles.file_cycles_output.as_deref().map(empty_output),
        ..outgoing_report.map(empty_output),
        ..request.output.as_deref().map(empty_output),
    ];
    let mut prepared = OutputFiles::open(&outputs, protected_inputs)?;
    prepared.write_output(&index_save_file)?;
    emit(format_args!("Dependency analysis saved to {}", index_save_file.path().display()));
    Ok(IndexCheckpoint::Saved { output_files: prepared })
}

#[bon::builder]
fn write_analysis_outputs(
    request: &Request,
    file_graph: FileGraph,
    saved_index: Option<FileOutput<'_>>,
    protected_inputs: &[&InputFile],
    diagram_output: &mut dyn Write,
) -> Result<()> {
    let mut checkpoint = open_outputs_and_save_index(request, saved_index, protected_inputs)?;
    let file_graph = if request.exclude_tests { file_graph.without_test_dependencies() } else { file_graph };
    let file_graph = file_graph.within_root(&request.root)?.excluding(&request.exclusions);
    let folder_graph = file_graph.folders(&request.root, request.depth, request.include_self)?;
    ensure!(!folder_graph.nodes().is_empty(), "{NO_FILES_AFTER_FILTERING}: root {}", request.root.display());
    let cycles = &request.cycles;
    let analyze_folder_cycles = cycles.highlight_cycles || cycles.cycles_output.is_some() || cycles.fail_on_cycles;
    let (highlight_analysis, folder_report_output, folder_cycle_group_count) = if analyze_folder_cycles {
        let analysis = analyze_folders(&folder_graph)?;
        let report = if let Some(path) = cycles.cycles_output.as_ref() {
            Some((path, folder_report(&analysis, cycles.cycle_limit)?))
        } else {
            None
        };
        let group_count = analysis.group_count();
        (cycles.highlight_cycles.then_some(analysis), report, group_count)
    } else {
        (None, None, 0)
    };
    let analyze_file_cycles = cycles.file_cycles_output.is_some() || cycles.fail_on_cycles;
    let (file_report_output, file_cycle_group_count) = if analyze_file_cycles {
        let analysis = analyze_files(&file_graph)?;
        let report = if let Some(path) = cycles.file_cycles_output.as_ref() {
            Some((path, file_report(&analysis, cycles.cycle_limit)?))
        } else {
            None
        };
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

    let mut outputs = Vec::new();
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
    match checkpoint {
        IndexCheckpoint::Saved { ref mut output_files } => {
            for output in &outputs {
                output_files.write_output(output)?;
            }
        }
        IndexCheckpoint::NotRequested => OutputFiles::open(&outputs, protected_inputs)?.write()?,
    }
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
    use std::io;
    use std::num::NonZeroUsize;
    use std::path::Path;
    use std::path::PathBuf;

    use anyhow::Error;
    use anyhow::Result;
    use assertables::assert_contains;
    use claims::assert_ok;
    use claims::assert_some;
    use derive_new::new;
    use pretty_assertions::assert_eq;
    use protobuf::Message as _;
    use rstest::rstest;
    use scip::types::Document;
    use scip::types::Index;
    use scip::types::Occurrence;
    use scip::types::SymbolRole;
    use tempfile::TempDir;
    use tempfile::tempdir;

    use super::CycleOptions;
    use super::Input;
    use super::NO_FILES_AFTER_FILTERING;
    use super::OutgoingDependencyOptions;
    use super::Project;
    use super::Request;
    use super::run;
    use super::write_analysis_outputs;
    use crate::Exclusions;
    use crate::InputFile;
    use crate::export::Format;
    use crate::export::Options;
    use crate::model::FileGraph;
    use crate::outgoing_dependencies::Mode;
    use crate::output_paths::FileOutput;
    use crate::output_paths::OutputPathConflict;
    use crate::scip::backend::parse_scip;

    struct ReplayTestFixture {
        directory: TempDir,
        index: PathBuf,
        saved_index: PathBuf,
        index_bytes: Vec<u8>,
        diagram: PathBuf,
        file_report: PathBuf,
        folder_report: PathBuf,
        outgoing_report: PathBuf,
    }

    #[derive(new)]
    struct ReplayRequestTestFixture {
        fixture: ReplayTestFixture,
        request: Request,
    }

    #[derive(new)]
    struct AnalyzedProjectTestFixture {
        fixture: ReplayTestFixture,
        request: Request,
        graph: FileGraph,
    }

    fn replay_bytes_test_fixture(index_bytes: Vec<u8>) -> ReplayTestFixture {
        let directory = tempdir();
        let directory = assert_ok!(directory, "Failed to create the replay fixture directory");
        let fixture = ReplayTestFixture {
            index: directory.path().join("index.scip"),
            saved_index: directory.path().join("saved.scip"),
            diagram: directory.path().join("diagram.dot"),
            file_report: directory.path().join("files.md"),
            folder_report: directory.path().join("folders.md"),
            outgoing_report: directory.path().join("outgoing.md"),
            directory, index_bytes,
        };
        let written = fs::write(&fixture.index, &fixture.index_bytes);
        assert_ok!(written, "Failed to write the replay fixture index at {}", fixture.index.display());
        fixture
    }

    fn replay_test_fixture(index: &Index) -> ReplayRequestTestFixture {
        let bytes = index.write_to_bytes();
        let bytes = assert_ok!(bytes, "Failed to serialize the replay fixture index");
        let fixture = replay_bytes_test_fixture(bytes);
        let request = Request::builder().input(Input::ScipFile(fixture.index.clone())).root(PathBuf::from("src")).build();
        let request = assert_ok!(request, "Failed to construct the replay request from its fixture index");
        ReplayRequestTestFixture::new(fixture, request)
    }

    #[must_use]
    fn cycle_reports_test_fixture(file_report: &Path, folder_report: &Path) -> CycleOptions {
        let options = CycleOptions::builder().file_cycles_output(file_report.to_path_buf());
        options.cycles_output(folder_report.to_path_buf()).build()
    }

    #[must_use]
    fn outgoing_options_test_fixture(output: Option<&Path>, maximum: usize, mode: Option<Mode>) -> OutgoingDependencyOptions {
        let options = OutgoingDependencyOptions::builder().maximum(maximum).maybe_mode(mode);
        options.maybe_output(output.map(Path::to_path_buf)).build()
    }

    fn write_outputs_test_fixture(paths: &[&Path], contents: &str) {
        for path in paths {
            let written = fs::write(path, contents);
            assert_ok!(written, "Failed to initialize the existing output at {}", path.display());
        }
    }

    fn read_outputs_test_fixture(paths: &[&Path]) -> Vec<String> {
        paths.iter().map(|path| {
            let contents = fs::read_to_string(path);
            assert_ok!(contents, "Failed to read the output at {}", path.display())
        }).collect()
    }

    fn output_alias_test_fixture(path: &Path, alias: bool) -> PathBuf {
        if alias {
            let output = path.with_extension("alias");
            let linked = fs::hard_link(path, &output);
            assert_ok!(linked, "Failed to create a hard link to {}", path.display());
            output
        } else {
            path.to_path_buf()
        }
    }

    #[must_use]
    fn request_output_test_fixture(mut request: Request, output: PathBuf, outgoing_report: bool) -> Request {
        if outgoing_report {
            request.outgoing_dependencies = Some(outgoing_options_test_fixture(Some(&output), usize::MAX, None));
        } else {
            request.output = Some(output);
        }
        request
    }

    fn analyzed_project_test_fixture() -> AnalyzedProjectTestFixture {
        let mut index = two_file_index_fixture(true);
        let third_symbol = "rust-analyzer cargo fixture 0.0.0 third().";
        let first = index.documents.first_mut();
        let first = assert_some!(first, "The analysis fixture contains no source files");
        let first_symbol = first.occurrences.first();
        let first_symbol = assert_some!(first_symbol, "The first fixture file contains no symbol occurrences").symbol.clone();
        first.occurrences.push(Occurrence { range: vec![2, 0, 5], symbol: third_symbol.into(), ..Occurrence::default() });
        index.documents.push(Document {
            relative_path: "src/third.rs".into(),
            occurrences: vec![
                Occurrence {
                    range: vec![0, 3, 8], symbol: third_symbol.into(),
                    symbol_roles: SymbolRole::Definition as i32, ..Occurrence::default()
                },
                Occurrence { range: vec![1, 0, 5], symbol: first_symbol, ..Occurrence::default() },
            ],
            ..Document::default()
        });
        let bytes = index.write_to_bytes();
        let bytes = assert_ok!(bytes, "Failed to serialize the completed-analysis fixture");
        let fixture = replay_bytes_test_fixture(bytes);
        let graph = parse_scip(&fixture.index_bytes);
        let graph = assert_ok!(graph, "Failed to parse the completed-analysis fixture into a file graph");
        let project = Project::new(fixture.directory.path().to_path_buf(), None);
        let cycles = cycle_reports_test_fixture(&fixture.file_report, &fixture.folder_report);
        let request = Request::builder().input(Input::Project(project)).root(PathBuf::from("src"));
        let request = request.save_scip(fixture.saved_index.clone()).output(fixture.diagram.clone()).cycles(cycles).build();
        let request = assert_ok!(request, "Failed to construct the completed-analysis request");
        AnalyzedProjectTestFixture::new(fixture, request, graph)
    }

    fn write_analyzed_outputs_test_fixture(
        fixture: &ReplayTestFixture, request: &Request, graph: FileGraph, diagram: &mut dyn io::Write,
    ) -> Result<()> {
        let protected_inputs: Vec<_> = request.protected_inputs.iter().collect();
        let outputs = write_analysis_outputs().request(request).file_graph(graph).protected_inputs(&protected_inputs);
        let saved_index = FileOutput::new(&fixture.saved_index, &fixture.index_bytes);
        outputs.saved_index(saved_index).diagram_output(diagram).call()
    }

    fn assert_saved_index_test_fixture(path: &Path, expected: &[u8]) {
        let saved = fs::read(path);
        let saved = assert_ok!(saved, "Failed to read the saved analysis at {}", path.display());

        assert_eq!(saved, expected, "Unexpected checkpoint contents at {}", path.display());
    }

    #[rstest]
    #[case::filtering(PathBuf::from("missing"), PathBuf::from("folders.md"), true)]
    #[case::output_open(PathBuf::from("src"), Path::new("missing").join("folders.md"), false)]
    fn test_analysis_checkpoint_failure_order(
        #[case] root: PathBuf, #[case] folder_report: PathBuf, #[case] checkpoint_written: bool,
    ) {
        const CONTENTS_EXISTING: &str = "previous result";
        let AnalyzedProjectTestFixture { fixture, mut request, graph } = analyzed_project_test_fixture();
        request.root = root;
        request.cycles.cycles_output = Some(fixture.directory.path().join(folder_report));
        let reports: [&Path; 3] = [&fixture.diagram, &fixture.folder_report, &fixture.file_report];
        write_outputs_test_fixture(
            &[&fixture.saved_index, &fixture.diagram, &fixture.folder_report, &fixture.file_report], CONTENTS_EXISTING,
        );
        let mut diagram = Vec::new();

        let system_under_test = write_analyzed_outputs_test_fixture(&fixture, &request, graph, &mut diagram);
        let preserved = read_outputs_test_fixture(&reports);

        let error = assert_some!(system_under_test.err(), "Output generation accepted an invalid path or empty root");
        let expected_index = if checkpoint_written { &fixture.index_bytes } else { CONTENTS_EXISTING.as_bytes() };
        if !checkpoint_written {
            assert_eq!(error.downcast_ref::<io::Error>().map(io::Error::kind), Some(io::ErrorKind::NotFound), "{error:#}");
        }
        assert_saved_index_test_fixture(&fixture.saved_index, expected_index);
        assert_eq!(preserved, [CONTENTS_EXISTING; 3], "Failed output generation changed existing reports");
        assert!(diagram.is_empty());
    }

    #[test]
    fn test_analysis_checkpoint_diagram_write_failure() {
        let AnalyzedProjectTestFixture { fixture, mut request, graph } = analyzed_project_test_fixture();
        request.output = None;
        let mut diagram: &mut [u8] = &mut [];

        let system_under_test = write_analyzed_outputs_test_fixture(&fixture, &request, graph, &mut diagram);
        let reports = read_outputs_test_fixture(&[&fixture.folder_report, &fixture.file_report]);

        let error = assert_some!(system_under_test.err(), "Writing a diagram to a zero-capacity sink unexpectedly succeeded");
        assert_eq!(error.downcast_ref::<io::Error>().map(io::Error::kind), Some(io::ErrorKind::WriteZero), "{error:#}");
        assert_saved_index_test_fixture(&fixture.saved_index, &fixture.index_bytes);
        assert!(reports.iter().all(|report| !report.is_empty()));
    }

    #[rstest]
    fn test_analysis_checkpoint_path_conflicts(
        #[values("diagram", "folder", "file", "outgoing", "input")] target: &str,
        #[values(false, true)] hard_link: bool,
    ) {
        const CONTENTS_EXISTING: &str = "previous result";
        let AnalyzedProjectTestFixture { fixture, mut request, graph } = analyzed_project_test_fixture();
        let outputs: [&Path; 4] = [&fixture.saved_index, &fixture.diagram, &fixture.folder_report, &fixture.file_report];
        write_outputs_test_fixture(&outputs, CONTENTS_EXISTING);
        let conflicting_path = output_alias_test_fixture(&fixture.saved_index, hard_link);
        match target {
            "input" => {
                let opened = InputFile::open(&conflicting_path);
                let input = assert_ok!(opened, "Failed to open the saved-index conflict fixture as a protected input");
                request.protected_inputs.push(input);
            }
            "diagram" => request.output = Some(conflicting_path),
            "folder" => request.cycles.cycles_output = Some(conflicting_path),
            "file" => request.cycles.file_cycles_output = Some(conflicting_path),
            "outgoing" => {
                request.outgoing_dependencies = Some(outgoing_options_test_fixture(Some(&conflicting_path), 0, None));
            }
            _ => unreachable!("Unsupported output kind in the checkpoint fixture: {target}"),
        }
        request.root = PathBuf::from("missing");
        let mut diagram = Vec::new();

        let system_under_test = write_analyzed_outputs_test_fixture(&fixture, &request, graph, &mut diagram);
        let preserved = read_outputs_test_fixture(&outputs);

        let conflict = system_under_test.as_ref().is_err_and(Error::is::<OutputPathConflict>);
        assert!(conflict, "Checkpoint path conflict was not reported: {system_under_test:?}");
        assert_eq!(preserved, [CONTENTS_EXISTING; 4]);
        assert!(diagram.is_empty());
    }

    #[test]
    fn test_analysis_failure_before_checkpoint() {
        const CONTENTS_EXISTING: &[u8] = b"previous index";
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Failed to create the project-without-manifest fixture");
        let saved_path = fixture.path().join("saved.scip");
        let project = Project::new(fixture.path().to_path_buf(), None);
        let request = Request::builder().input(Input::Project(project)).save_scip(saved_path.clone()).build();
        let request = assert_ok!(request, "Request construction rejected a project before analysis");
        let initialized = fs::write(&saved_path, CONTENTS_EXISTING);
        assert_ok!(initialized, "Failed to initialize the previous saved index");
        let mut diagram = Vec::new();

        let system_under_test = run(&request, &mut diagram);

        assert!(system_under_test.is_err(), "Analysis succeeded for a project without a manifest");
        assert_saved_index_test_fixture(&saved_path, CONTENTS_EXISTING);
        assert!(diagram.is_empty());
    }

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
        let options = Options::builder().group_folders(true).build();
        let export = Format::Mermaid.exporter().write_with_options(&folders, &options, &mut expected);
        assert_ok!(export, "Expected grouped Mermaid diagram must render successfully");
        let request = Request::builder().input(Input::ScipFile(fixture)).root(root).format(Format::Mermaid);
        let system_under_test = request.depth(1).group_folders(true).build();
        let system_under_test = assert_ok!(system_under_test, "Saved analysis must load during request construction");
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
        let fixture = replay_test_fixture(&outgoing_dependency_index_fixture());
        let ReplayRequestTestFixture { fixture: _fixture, request: mut system_under_test } = fixture;
        system_under_test.root = PathBuf::from(root);
        system_under_test.exclude_tests = exclude_tests;
        system_under_test.depth = depth;
        system_under_test.outgoing_dependencies = Some(outgoing_options_test_fixture(None, 1, Some(mode)));
        let expected_failure = !expected.is_empty();
        let expected_lines: Vec<_> = expected.iter().map(|&(label, count)| {
            let label = match mode {
                Mode::Files => PathBuf::from_iter(label.split('/')).to_string_lossy().into_owned(),
                Mode::Folders => label.to_owned(),
            };
            format!("- {label:?}: {count} outgoing dependencies (maximum: 1)")
        }).collect();
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);
        let failed = result.is_err();
        let diagnostic = result.err().map_or_else(String::new, |error| error.to_string());
        let actual_lines: Vec<_> = diagnostic.lines().filter(|line| line.starts_with("- ")).collect();

        assert_eq!(failed, expected_failure, "Unexpected graph-selection result: {diagnostic}");
        assert_eq!(actual_lines, expected_lines, "Violations must reflect the selected graph after filtering and folding");
    }

    #[rstest]
    #[case::disabled(false, None, true, 0, false)]
    #[case::default_folders(true, None, false, 0, false)]
    #[case::default_folder_self(true, None, true, 0, true)]
    #[case::files_without_self(true, Some(Mode::Files), false, 0, true)]
    #[case::files_with_self(true, Some(Mode::Files), true, 0, true)]
    #[case::folder_equality(true, Some(Mode::Folders), true, 1, false)]
    #[case::file_equality(true, Some(Mode::Files), false, 1, false)]
    fn test_outgoing_dependency_gate(
        #[case] enabled: bool,
        #[case] mode: Option<Mode>,
        #[case] include_self: bool,
        #[case] maximum: usize,
        #[case] expected_failure: bool,
    ) {
        let fixture = replay_test_fixture(&two_file_index_fixture(false));
        let ReplayRequestTestFixture { fixture: _fixture, request: mut system_under_test } = fixture;
        system_under_test.include_self = include_self;
        system_under_test.outgoing_dependencies = enabled.then(|| outgoing_options_test_fixture(None, maximum, mode));
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);
        let failed = result.is_err();

        assert_eq!(failed, expected_failure, "Unexpected outgoing threshold result: {result:?}");
        if let Err(error) = result {
            let diagnostic = error.to_string();
            assert_contains!(diagnostic, "Outgoing dependency threshold exceeded", "Unexpected gate failure: {diagnostic}");
        }
    }

    #[rstest]
    #[case::at_limit(1, false)]
    #[case::above_limit(0, true)]
    fn test_outgoing_dependency_outputs(#[case] maximum: usize, #[case] expected_failure: bool) {
        const CONTENTS_EXISTING: &str = "preserved";
        let fixture = replay_test_fixture(&two_file_index_fixture(false));
        let ReplayRequestTestFixture { fixture, request: mut system_under_test } = fixture;
        let outputs: [&Path; 2] = [&fixture.diagram, &fixture.outgoing_report];
        write_outputs_test_fixture(&outputs, CONTENTS_EXISTING);
        system_under_test.output = Some(fixture.diagram.clone());
        let outgoing = outgoing_options_test_fixture(Some(&fixture.outgoing_report), maximum, Some(Mode::Files));
        system_under_test.outgoing_dependencies = Some(outgoing);
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);
        let exported = read_outputs_test_fixture(&outputs);
        let replaced: Vec<_> = exported.iter().map(|contents| !contents.is_empty() && contents != CONTENTS_EXISTING).collect();
        let failed = result.is_err();

        assert_eq!(failed, expected_failure, "Unexpected outgoing threshold result: {result:?}");
        assert_eq!(replaced, [true; 2], "Both outputs must be replaced before gating: {exported:?}");
        assert_eq!(diagram, [] as [u8; 0], "An output-file request must not also write to the supplied writer");
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
        let fixture = replay_test_fixture(&two_file_index_fixture(cyclic));
        let ReplayRequestTestFixture { fixture, request: mut system_under_test } = fixture;
        system_under_test.cycles.fail_on_cycles = fail_on_cycles;
        system_under_test.cycles.file_cycles_output = report_requested.then(|| fixture.file_report.clone());
        /* Same-folder files and disabled self-edges keep the folder gate from masking skipped file analysis. */
        system_under_test.include_self = false;
        let expected_error = expected_file_cycle_groups.map(|groups| {
            format!("Dependency cycles found: 0 folder cycle group(s), {groups} file cycle group(s)")
        });
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);

        let actual_error = result.err().map(|error| error.to_string());
        assert_eq!(actual_error, expected_error);
        assert!(!diagram.is_empty(), "Cycle gating must happen after diagram output is written");
        let report_exists = fixture.file_report.try_exists();
        let report_exists = assert_ok!(report_exists, "File-cycle report path must be inspectable");
        assert_eq!(report_exists, report_requested, "Only a requested file-cycle report should be written");
        if report_requested {
            let report = fs::read_to_string(&fixture.file_report);
            let report = assert_ok!(report, "Requested file-cycle report must be readable");
            let cycle_groups = report.lines().filter(|line| line.starts_with("## ")).count();
            let expected_groups = usize::from(cyclic);
            assert_eq!(cycle_groups, expected_groups, "Report requests must trigger file-cycle analysis independently of gating");
        }
    }

    #[rstest]
    #[case::report_only(false, 2, [false, false])]
    #[case::cycles_only(true, 2, [true, false])]
    #[case::outgoing_only(false, 0, [false, true])]
    #[case::both(true, 0, [true, true])]
    fn test_combined_dependency_gates(#[case] fail_on_cycles: bool, #[case] maximum: usize, #[case] expected_reasons: [bool; 2]) {
        let AnalyzedProjectTestFixture { fixture, request: mut system_under_test, graph } = analyzed_project_test_fixture();
        system_under_test.output = None;
        system_under_test.include_self = true;
        system_under_test.cycles.cycle_limit = NonZeroUsize::MIN;
        system_under_test.cycles.fail_on_cycles = fail_on_cycles;
        let outgoing = outgoing_options_test_fixture(Some(&fixture.outgoing_report), maximum, Some(Mode::Files));
        system_under_test.outgoing_dependencies = Some(outgoing);
        let mut diagram = Vec::new();

        let result = write_analyzed_outputs_test_fixture(&fixture, &system_under_test, graph, &mut diagram);
        assert_eq!(result.is_err(), expected_reasons.contains(&true), "Unexpected gate result: {result:?}");
        let diagnostic = result.err().map_or_else(String::new, |error| error.to_string());
        let reasons = ["Dependency cycles found:", "Outgoing dependency threshold exceeded"];
        let actual_reasons = reasons.map(|reason| diagnostic.contains(reason));
        let reports = read_outputs_test_fixture(&[&fixture.folder_report, &fixture.file_report, &fixture.outgoing_report]);
        let cycle_listings: Vec<_> = reports.iter().take(2).map(|report| {
            let chain_count = report.lines().filter(|line| line.starts_with("- ")).count();
            (chain_count, report.contains("cycle listing truncated"))
        }).collect();

        assert_eq!(actual_reasons, expected_reasons, "Dependency gate failure reasons differ: {diagnostic}");
        assert_saved_index_test_fixture(&fixture.saved_index, &fixture.index_bytes);
        assert_eq!(cycle_listings, [(1, false), (1, true)], "Incorrect folder/file chain counts or truncation markers");
        assert!(reports.iter().all(|report| !report.is_empty()), "An exported dependency report is empty");
        assert!(!diagram.is_empty(), "The exported diagram is empty");
    }

    #[rstest]
    #[case::matching("src/second.rs", false)]
    #[case::project_relative("second.rs", true)]
    fn test_exclusions_and_checks(
        #[case] pattern: &str,
        #[case] expected_failure: bool,
        #[values(Mode::Files, Mode::Folders)] mode: Mode,
    ) {
        let fixture = replay_test_fixture(&two_file_index_fixture(true));
        let ReplayRequestTestFixture { fixture, request: mut system_under_test } = fixture;
        let exclusions = Exclusions::new(&[pattern.to_owned()]);
        system_under_test.exclusions = assert_ok!(exclusions, "Replay exclusion patterns must compile");
        system_under_test.include_self = true;
        system_under_test.cycles = cycle_reports_test_fixture(&fixture.file_report, &fixture.folder_report);
        system_under_test.cycles.highlight_cycles = true;
        system_under_test.cycles.fail_on_cycles = true;
        let outgoing = outgoing_options_test_fixture(Some(&fixture.outgoing_report), 0, Some(mode));
        system_under_test.outgoing_dependencies = Some(outgoing);
        let expected_groups = usize::from(expected_failure);
        let expected_sources = if expected_failure { match mode { Mode::Files => 2, Mode::Folders => 1 } } else { 0 };
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);
        let reports = read_outputs_test_fixture(&[&fixture.file_report, &fixture.folder_report]);
        let groups: Vec<_> = reports.iter().map(|report| report.lines().filter(|line| line.starts_with("## ")).count()).collect();
        let outgoing_contents = fs::read_to_string(&fixture.outgoing_report);
        let outgoing_contents = assert_ok!(outgoing_contents, "Outgoing counts must be written for the filtered replay graph");
        let violating_sources = outgoing_contents.lines().filter(|line| line.starts_with("- ")).count();
        let failed = result.is_err();

        assert_eq!(failed, expected_failure, "Exclusions did not affect dependency checks as expected: {result:?}");
        assert_eq!(groups, [expected_groups; 2], "File and folder cycles must use the filtered graph");
        assert_eq!(violating_sources, expected_sources, "Outgoing counts must use the filtered graph");
    }

    #[test]
    fn test_exclusions_saved_replay() {
        let fixture = replay_test_fixture(&two_file_index_fixture(true));
        let ReplayRequestTestFixture { fixture, request: mut system_under_test } = fixture;
        let exclusions = Exclusions::new(&["src/second.rs".to_owned()]);
        system_under_test.exclusions = assert_ok!(exclusions, "Replay exclusion patterns must compile");
        system_under_test.include_self = true;
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);
        assert_ok!(result, "Filtered replay must export its remaining graph");
        system_under_test.exclusions = Exclusions::default();
        let mut replayed_diagram = Vec::new();
        let replay = run(&system_under_test, &mut replayed_diagram);
        assert_ok!(replay, "The same saved index must support replay with different filters");
        let preserved = fs::read(&fixture.index);
        let preserved = assert_ok!(preserved, "The saved index must remain readable after changing replay filters");

        assert_ne!(diagram, replayed_diagram, "Clearing exclusions must restore the original graph");
        assert_eq!(preserved, fixture.index_bytes, "Exclusions must not rewrite the saved index");
    }

    #[test]
    fn test_exclusions_all_files() {
        const CONTENTS_EXISTING: &str = "preserved";
        let fixture = replay_test_fixture(&two_file_index_fixture(false));
        let ReplayRequestTestFixture { fixture, request: mut system_under_test } = fixture;
        let outputs: [&Path; 3] = [&fixture.diagram, &fixture.file_report, &fixture.outgoing_report];
        write_outputs_test_fixture(&outputs, CONTENTS_EXISTING);
        let exclusions = Exclusions::new(&["src/**".to_owned()]);
        system_under_test.exclusions = assert_ok!(exclusions, "A whole-root exclusion must compile");
        system_under_test.output = Some(fixture.diagram.clone());
        system_under_test.cycles = cycle_reports_test_fixture(&fixture.file_report, &fixture.folder_report);
        system_under_test.outgoing_dependencies = Some(outgoing_options_test_fixture(Some(&fixture.outgoing_report), 0, None));
        let mut diagram = Vec::new();

        let error = run(&system_under_test, &mut diagram).err();
        let error = assert_some!(error, "Excluding every indexed file must fail before opening outputs");
        let diagnostic = error.to_string();
        let preserved = read_outputs_test_fixture(&outputs);
        let report_exists = fixture.folder_report.try_exists();
        let report_exists = assert_ok!(report_exists, "The unwritten folder-report path must be inspectable");

        assert!(diagnostic.starts_with(NO_FILES_AFTER_FILTERING), "The error must explain filtering: {diagnostic}");
        assert_eq!(preserved, [CONTENTS_EXISTING; 3], "An empty filtered graph replaced an existing output");
        assert!(!report_exists, "An empty filtered graph created a report");
        assert!(diagram.is_empty(), "An empty filtered graph wrote diagram bytes");
    }

    #[rstest]
    #[case::readable_input(false)]
    #[case::missing_input(true)]
    fn test_replay_cannot_save_analysis(#[case] missing_input: bool) {
        let ReplayRequestTestFixture { fixture, request: _request } = replay_test_fixture(&two_file_index_fixture(false));
        let output = fixture.saved_index;
        let initialized = fs::write(&output, b"preserved");
        assert_ok!(initialized, "Existing analysis output must be initialized separately from the replay input");
        let input = if missing_input { fixture.directory.path().join("missing.scip") } else { fixture.index.clone() };
        let system_under_test = Request::builder().input(Input::ScipFile(input)).save_scip(output.clone());

        let request = system_under_test.build();
        let preserved = fs::read(&output);
        let input_bytes = fs::read(&fixture.index);

        let error = assert_some!(request.err(), "Request construction must reject saving replayed analysis");
        assert!(!error.is::<std::io::Error>(), "Conflicting options must be rejected before input I/O: {error:#}");
        let preserved = assert_ok!(preserved, "Rejected replay must leave the existing analysis output readable");
        let input_bytes = assert_ok!(input_bytes, "Rejected replay must leave the source index readable");
        assert_eq!(preserved, b"preserved", "Rejected replay must preserve the existing output");
        assert_eq!(input_bytes, fixture.index_bytes, "Rejected replay must preserve its source index");
    }

    #[rstest]
    #[case::missing(false)]
    #[case::directory(true)]
    fn test_replay_input_io_at_construction(#[case] directory_input: bool) {
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "The request construction fixture must be created");
        let path = if directory_input { fixture.path().to_path_buf() } else { fixture.path().join("missing.scip") };
        let system_under_test = Request::builder().input(Input::ScipFile(path.clone()));

        let error = system_under_test.build().err();
        let error = assert_some!(error, "An unreadable saved index must fail before execution");
        let diagnostic = format!("{error:#}");

        assert!(error.is::<std::io::Error>(), "Input failure must preserve its I/O cause: {diagnostic}");
        assert_contains!(diagnostic, &path.to_string_lossy().into_owned(), "Input failure must identify its path: {diagnostic}");
    }

    #[rstest]
    #[case::overwrite_contents(false)]
    #[case::replace_path(true)]
    fn test_replay_snapshot_at_construction(#[case] replace_path: bool) {
        let fixture = replay_test_fixture(&two_file_index_fixture(false));
        let ReplayRequestTestFixture { fixture, request: system_under_test } = fixture;
        if replace_path {
            let renamed = fs::rename(&fixture.index, fixture.directory.path().join("retained.scip"));
            assert_ok!(renamed, "The loaded index must be renamed before replacing its path");
        }
        let changed = fs::write(&fixture.index, b"\xff");
        assert_ok!(changed, "The index path must contain malformed data before the first run");
        let mut diagram = Vec::new();
        let mut repeated_diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);
        let repeated = run(&system_under_test, &mut repeated_diagram);
        let reloaded = Request::builder().input(Input::ScipFile(fixture.index)).build();

        assert_ok!(result, "Execution must use the snapshot captured by build, not the changed file");
        assert_ok!(repeated, "The same request must support repeated execution without rereading the file");
        assert!(!diagram.is_empty(), "The parsed snapshot must still produce a diagram");
        assert_eq!(diagram, repeated_diagram, "Repeated execution must use the same saved analysis");
        assert!(reloaded.is_err(), "A new request must load the changed file instead of reusing another request's snapshot");
    }

    #[test]
    fn test_renamed_replay_input_protection() {
        let fixture = replay_test_fixture(&two_file_index_fixture(false));
        let ReplayRequestTestFixture { fixture, request: mut system_under_test } = fixture;
        let retained = fixture.directory.path().join("retained.scip");
        let renamed = fs::rename(&fixture.index, &retained);
        assert_ok!(renamed, "The loaded index must be renamed while its identity is retained");
        let replaced = fs::write(&fixture.index, b"replacement");
        assert_ok!(replaced, "The original input path must contain a different file before execution");
        system_under_test.output = Some(retained.clone());
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);
        let preserved = fs::read(&retained);
        let replacement = fs::read(&fixture.index);

        assert!(result.as_ref().is_err_and(Error::is::<OutputPathConflict>), "The retained input must be protected: {result:?}");
        let preserved = assert_ok!(preserved, "The renamed index must remain readable after rejecting output");
        let replacement = assert_ok!(replacement, "The replacement file must remain readable after rejecting output");
        assert_eq!(preserved, fixture.index_bytes, "Output protection must follow the original file's identity");
        assert_eq!(replacement, b"replacement", "Output protection must not modify the replacement file");
        assert!(diagram.is_empty(), "An input conflict must not write a diagram");
    }

    #[rstest]
    #[case::same_path(false)]
    #[case::hard_link(true)]
    fn test_replay_cannot_overwrite_input(#[case] alias: bool, #[values(false, true)] outgoing_report: bool) {
        let ReplayRequestTestFixture { fixture, request } = replay_test_fixture(&two_file_index_fixture(false));
        let output = output_alias_test_fixture(&fixture.index, alias);
        let system_under_test = request_output_test_fixture(request, output, outgoing_report);
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);
        let preserved = fs::read(&fixture.index);
        let preserved = assert_ok!(preserved, "Protected SCIP input must remain readable after replay rejection");
        let conflict = result.as_ref().is_err_and(Error::is::<OutputPathConflict>);

        assert!(conflict, "Replay must protect input aliases: {result:?}");
        assert_eq!(preserved, fixture.index_bytes);
        assert_eq!(diagram, [] as [u8; 0]);
    }

    #[rstest]
    #[case::configuration_path(true, false, false)]
    #[case::configuration_alias(true, true, false)]
    #[case::configuration_renamed(true, false, true)]
    #[case::index_path(false, false, false)]
    #[case::index_alias(false, true, false)]
    fn test_request_protected_inputs(
        #[case] configuration_target: bool,
        #[case] alias: bool,
        #[case] renamed: bool,
        #[values(false, true)] outgoing_report: bool,
    ) {
        const CONTENTS_CONFIGURATION: &[u8] = b"# protected configuration\n";
        const CONTENTS_REPLACEMENT: &[u8] = b"# replacement configuration\n";
        let ReplayRequestTestFixture { fixture, mut request } = replay_test_fixture(&two_file_index_fixture(false));
        let configuration = fixture.directory.path().join("source-mesh.toml");
        let retained_configuration = fixture.directory.path().join("retained.toml");
        let configuration_write = fs::write(&configuration, CONTENTS_CONFIGURATION);
        assert_ok!(configuration_write, "Protected configuration must be written before loading");
        let opened_configuration = InputFile::open(&configuration);
        let mut opened_configuration = assert_ok!(opened_configuration, "Configuration must open before request construction");
        let loaded = opened_configuration.read();
        let loaded = assert_ok!(loaded, "Configuration contents must be read from its retained handle");
        assert_eq!(CONTENTS_CONFIGURATION, loaded, "The retained handle must identify the loaded configuration");
        let configuration_path = if renamed {
            let moved = fs::rename(&configuration, &retained_configuration);
            assert_ok!(moved, "Loaded configuration must be renamed while its identity is retained");
            let replacement_write = fs::write(&configuration, CONTENTS_REPLACEMENT);
            assert_ok!(replacement_write, "The original configuration path must be replaced before output validation");
            &retained_configuration
        } else {
            &configuration
        };
        let protected_path = if configuration_target { configuration_path } else { &fixture.index };
        let output = output_alias_test_fixture(protected_path, alias);
        request.protected_inputs = vec![opened_configuration];
        let system_under_test = request_output_test_fixture(request, output, outgoing_report);
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);
        let preserved_index = fs::read(&fixture.index);
        let preserved_index = assert_ok!(preserved_index, "Replay index must remain readable after a protected-input conflict");
        let preserved_configuration = fs::read(configuration_path);
        let preserved_configuration = assert_ok!(preserved_configuration, "The loaded configuration must remain readable");

        let conflict = result.as_ref().is_err_and(Error::is::<OutputPathConflict>);
        assert!(conflict, "Output validation must protect both request-owned and locally opened inputs: {result:?}");
        assert_eq!(fixture.index_bytes, preserved_index, "Output validation changed the replay index");
        assert_eq!(CONTENTS_CONFIGURATION, preserved_configuration, "Output validation changed the loaded configuration");
        if renamed {
            let replacement = fs::read(configuration);
            let replacement = assert_ok!(replacement, "The replacement configuration must remain readable");
            assert_eq!(CONTENTS_REPLACEMENT, replacement, "Output validation changed the replacement configuration");
        }
        assert!(diagram.is_empty(), "An input conflict must be detected before writing diagram bytes");
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::malformed(b"\xff")]
    #[case::metadata_without_documents(b"\x0a\x00")]
    fn test_invalid_index_preserves_outputs(#[case] bytes: &[u8], #[values(false, true)] existing_report: bool) {
        const CONTENTS_EXISTING: &str = "preserved";
        let fixture = replay_bytes_test_fixture(bytes.to_vec());
        let mut existing_outputs = vec![fixture.diagram.as_path()];
        if existing_report { existing_outputs.push(&fixture.outgoing_report); }
        write_outputs_test_fixture(&existing_outputs, CONTENTS_EXISTING);
        let cycles = cycle_reports_test_fixture(&fixture.file_report, &fixture.folder_report);
        let outgoing = outgoing_options_test_fixture(Some(&fixture.outgoing_report), 0, None);
        let request = Request::builder().input(Input::ScipFile(fixture.index.clone()));
        let system_under_test = request.output(fixture.diagram.clone()).cycles(cycles).outgoing_dependencies(outgoing);

        let result = system_under_test.build();
        let preserved = read_outputs_test_fixture(&existing_outputs);
        let expected = vec![CONTENTS_EXISTING; existing_outputs.len()];
        let reports = [&fixture.folder_report, &fixture.file_report, &fixture.outgoing_report];
        let reports_exist = reports.map(|path| {
            let exists = path.try_exists();
            assert_ok!(exists, "Report path must be inspectable: {}", path.display())
        });

        assert!(result.is_err(), "Malformed external analysis must fail during request construction");
        assert_eq!(preserved, expected, "Invalid input must not replace an existing output");
        assert_eq!(reports_exist, [false, false, existing_report], "Invalid input must not create any report");
    }

    #[rstest]
    #[case::empty("")]
    #[case::parent("../outside.rs")]
    #[case::absolute("/absolute.rs")]
    fn test_invalid_source_path_preserves_outputs(#[case] path: &str) {
        let index = Index { documents: vec![Document { relative_path: path.into(), ..Document::default() }], ..Index::default() };
        let bytes = index.write_to_bytes();
        let bytes = assert_ok!(bytes, "The invalid-source-path index must serialize");
        let fixture = replay_bytes_test_fixture(bytes);
        let system_under_test = Request::builder().input(Input::ScipFile(fixture.index.clone())).output(fixture.diagram.clone());

        let result = system_under_test.build();

        assert!(result.is_err(), "Malformed external source paths must fail during request construction");
        let output_exists = fixture.diagram.try_exists();
        let output_exists = assert_ok!(output_exists, "Diagram output path must be inspectable");
        assert!(!output_exists, "Invalid input created a diagram");
    }

    #[rstest]
    #[case::same_path(false)]
    #[case::hard_link(true)]
    fn test_conflicting_outputs_preserve_files(#[case] alias: bool, #[values(false, true)] outgoing_report: bool) {
        const CONTENTS_EXISTING: &str = "preserved";
        let fixture = replay_test_fixture(&two_file_index_fixture(false));
        let ReplayRequestTestFixture { fixture, request: mut system_under_test } = fixture;
        let outputs: [&Path; 2] = [&fixture.diagram, &fixture.file_report];
        write_outputs_test_fixture(&outputs, CONTENTS_EXISTING);
        let conflicting_report = output_alias_test_fixture(&fixture.diagram, alias);
        system_under_test.output = Some(fixture.diagram.clone());
        system_under_test.cycles.file_cycles_output = Some(fixture.file_report.clone());
        if outgoing_report {
            let outgoing = outgoing_options_test_fixture(Some(&conflicting_report), usize::MAX, None);
            system_under_test.outgoing_dependencies = Some(outgoing);
        } else {
            system_under_test.cycles.cycles_output = Some(conflicting_report);
        }
        let mut diagram = Vec::new();

        let result = run(&system_under_test, &mut diagram);
        let preserved = read_outputs_test_fixture(&outputs);
        let conflict = result.as_ref().is_err_and(Error::is::<OutputPathConflict>);

        assert!(conflict, "Replay must reject output aliases: {result:?}");
        assert_eq!(preserved, [CONTENTS_EXISTING; 2]);
        assert_eq!(diagram, [] as [u8; 0]);
    }
}
