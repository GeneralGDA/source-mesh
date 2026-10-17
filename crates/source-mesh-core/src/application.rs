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

use crate::analyzer_options::Options as AnalyzerOptions;
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
use crate::scip_backend::file_graph;
use crate::scip_index::ParsedIndex;

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
    let file_graph = if request.exclude_tests {
        file_graph.without_test_dependencies()
    } else {
        file_graph
    };
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
#[expect(
    clippy::panic_in_result_fn,
    reason = "Tests use assertions for expectations while returning Result for fallible setup."
)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use protobuf::Message as _;
    use rstest::rstest;
    use scip::types::Document;
    use scip::types::Index;
    use tempfile::tempdir;

    use super::CycleOptions;
    use super::Input;
    use super::Request;
    use super::run;
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
}
