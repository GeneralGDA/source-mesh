use std::fs;
use std::io::Write;
use std::num::NonZeroUsize;
use std::path::PathBuf;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use bon::Builder;
use derive_new::new;

use crate::analyzer_options::Options as AnalyzerOptions;
use crate::backend::IndexSource;
use crate::backend::RustAnalyzer;
use crate::backend::ScipFile;
use crate::cycle_report::file_report;
use crate::cycle_report::folder_report;
use crate::cycles::analyze_files;
use crate::cycles::analyze_folders;
use crate::export::Format;
use crate::export::Options as ExportOptions;
use crate::output_paths::validate_output_paths;
use crate::scip_backend::parse_scip;

/// Maximum number of simple cycles in each report by default.
pub const DEFAULT_CYCLE_LIMIT: NonZeroUsize = match NonZeroUsize::new(10_000) {
    Some(limit) => limit,
    None => unreachable!(),
};

/// A Rust project and its optional rust-analyzer JSON configuration.
#[derive(new)]
pub struct Project {
    path: PathBuf,
    configuration: Option<PathBuf>,
}

/// Analyze a project or replay an existing SCIP index.
pub enum Input {
    Project(Project),
    ScipFile(PathBuf),
}

/// Cycle highlighting, reports, and failure policy.
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

/// An analysis and export operation. Boolean options are disabled by default.
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

Validation, analysis, and serialization finish before any output is written.
Cycle gating fails only after outputs have been written. File-system or writer
failures during output can leave earlier outputs written.
*/
pub fn run(request: &Request, diagram_output: &mut dyn Write) -> Result<()> {
    ensure!(
        !matches!(request.input, Input::ScipFile(_)) || request.save_scip.is_none(),
        "Cannot save analysis when replaying a SCIP file"
    );
    let input_path = match request.input {
        Input::Project(ref project) => project.configuration.as_deref(),
        Input::ScipFile(ref path) => Some(path.as_path()),
    };
    validate_output_paths(
        &[
            request.output.as_deref(),
            request.save_scip.as_deref(),
            request.cycles.cycles_output.as_deref(),
            request.cycles.file_cycles_output.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>(),
        &input_path.into_iter().collect::<Vec<_>>(),
    )?;
    let index_source: Box<dyn IndexSource + '_> = match request.input {
        Input::ScipFile(ref path) => Box::new(ScipFile::new(path.clone())),
        Input::Project(ref project) => Box::new(RustAnalyzer::new(
            &project.path,
            project.configuration.as_deref(),
            &request.analyzer,
        )),
    };
    let index_bytes = index_source.index()?;
    let file_graph = parse_scip(&index_bytes)?;
    let file_graph = if request.exclude_tests {
        file_graph.without_test_dependencies()
    } else {
        file_graph
    };
    let file_graph = file_graph.within_root(&request.root)?;
    let folder_graph = file_graph.folders(&request.root, request.depth, request.include_self)?;
    ensure!(
        !folder_graph.nodes().is_empty(),
        "No indexed Rust files under root {}",
        request.root.display()
    );
    let folder_cycle_analysis = if request.cycles.highlight_cycles
        || request.cycles.cycles_output.is_some()
        || request.cycles.fail_on_cycles
    {
        Some(analyze_folders(&folder_graph)?)
    } else {
        None
    };
    let folder_report_content = request
        .cycles
        .cycles_output
        .as_ref()
        .map(|_| {
            folder_report(
                folder_cycle_analysis
                    .as_ref()
                    .context("Folder cycle analysis is missing")?,
                request.cycles.cycle_limit,
            )
        })
        .transpose()?;
    let file_cycle_analysis =
        if request.cycles.file_cycles_output.is_some() || request.cycles.fail_on_cycles {
            Some(analyze_files(&file_graph)?)
        } else {
            None
        };
    let file_report_content = request
        .cycles
        .file_cycles_output
        .as_ref()
        .map(|_| {
            file_report(
                file_cycle_analysis
                    .as_ref()
                    .context("File cycle analysis is missing")?,
                request.cycles.cycle_limit,
            )
        })
        .transpose()?;
    let mut diagram_bytes = Vec::new();
    let exporter = request.format.exporter();
    exporter.write_with_options(
        &folder_graph,
        &ExportOptions::new(
            request.group_folders,
            folder_cycle_analysis
                .as_ref()
                .filter(|_| request.cycles.highlight_cycles),
        ),
        &mut diagram_bytes,
    )?;
    if let Some(path) = request.save_scip.as_deref() {
        fs::write(path, index_bytes)
            .with_context(|| format!("Cannot save analysis to {}", path.display()))?;
    }
    if let (Some(path), Some(report)) = (request.cycles.cycles_output.as_deref(), folder_report_content) {
        fs::write(path, report)
            .with_context(|| format!("Cannot write folder cycle report {}", path.display()))?;
    }
    if let (Some(path), Some(report)) = (request.cycles.file_cycles_output.as_deref(), file_report_content) {
        fs::write(path, report)
            .with_context(|| format!("Cannot write file cycle report {}", path.display()))?;
    }
    match request.output.as_deref() {
        Some(path) => fs::write(path, diagram_bytes)
            .with_context(|| format!("Cannot write {}", path.display()))?,
        None => diagram_output.write_all(&diagram_bytes)?,
    }
    if request.cycles.fail_on_cycles {
        let folder_cycle_group_count = folder_cycle_analysis
            .as_ref()
            .context("Folder cycle analysis is missing")?
            .group_count();
        let file_cycle_group_count = file_cycle_analysis
            .as_ref()
            .context("File cycle analysis is missing")?
            .group_count();
        ensure!(
            folder_cycle_group_count == 0 && file_cycle_group_count == 0,
            "Dependency cycles found: {folder_cycle_group_count} folder cycle group(s), {file_cycle_group_count} file cycle group(s)"
        );
    }
    Ok(())
}
