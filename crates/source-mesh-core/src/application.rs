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
