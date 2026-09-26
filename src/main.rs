use std::fs;
use std::io;
use std::io::Write;
use std::num::NonZeroUsize;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use clap::Args;
use clap::CommandFactory as _;
use clap::Parser;

use source_mesh::analyzer_options::Options as AnalyzerOptions;
use source_mesh::backend::IndexSource;
use source_mesh::backend::RustAnalyzer;
use source_mesh::backend::ScipFile;
use source_mesh::backend::contains_project_file;
use source_mesh::cycle_report::file_report;
use source_mesh::cycle_report::folder_report;
use source_mesh::cycles::analyze_files;
use source_mesh::cycles::analyze_folders;
use source_mesh::export::Format;
use source_mesh::export::Options as ExportOptions;
use source_mesh::scip_backend::parse_scip;

mod output_paths;

use output_paths::validate_output_paths;

const BOOLEAN_OPTIONS_DEFAULT_HELP: &str = "All boolean options are disabled by default.";

#[derive(Args)]
struct CycleOptions {
    #[arg(
        long,
        help = "Highlight cyclic folder components with distinct bright colors"
    )]
    highlight_cycles: bool,
    #[arg(
        long,
        help = "Exit unsuccessfully after writing outputs if file or folder dependency cycles are found"
    )]
    fail_on_cycles: bool,
    #[arg(long, help = "Write every simple folder cycle to a Markdown report")]
    cycles_output: Option<PathBuf>,
    #[arg(
        long,
        help = "Write file cycles before folder folding to a separate Markdown report"
    )]
    file_cycles_output: Option<PathBuf>,
    #[arg(
        long,
        default_value = "10000",
        help = "Maximum cycles per report; exceeding it fails without writing partial results"
    )]
    cycle_limit: NonZeroUsize,
}

#[derive(Args)]
struct AnalyzerArguments {
    #[arg(short, long, help = "Show diagnostic details and analysis timings")]
    verbose: bool,
}

#[derive(Parser)]
#[command(
    version,
    about = "Draw Rust folder dependencies and detect dependency cycles",
    after_help = BOOLEAN_OPTIONS_DEFAULT_HELP
)]
struct Cli {
    #[arg(
        default_value = ".",
        help = "Project/workspace directory or Cargo.toml (unused with --from-scip)"
    )]
    path: PathBuf,
    #[arg(short, long, value_enum, default_value = "dot")]
    format: Format,
    #[arg(short, long, help = "Output file (otherwise stdout)")]
    output: Option<PathBuf>,
    #[arg(
        short,
        long,
        help = "Maximum folder levels below --root: 0 = root only; default = all"
    )]
    depth: Option<usize>,
    #[arg(
        long,
        default_value = ".",
        help = "Restrict to this relative subtree of the index/project, e.g. src or crates/server/src"
    )]
    root: PathBuf,
    #[arg(
        long,
        help = "Keep edges between distinct files collapsed into the same folder"
    )]
    include_self: bool,
    #[arg(
        long,
        help = "Nest subfolders, hide shared ancestor groups, and keep their own files as separate nodes"
    )]
    group_folders: bool,
    #[arg(
        long,
        help = "Hide dependencies used only by tests"
    )]
    exclude_tests: bool,
    #[command(flatten)]
    cycles: CycleOptions,
    #[arg(long, conflicts_with_all = ["save_scip", "config"], help = "Reuse saved analysis without analyzing the source again")]
    from_scip: Option<PathBuf>,
    #[arg(
        long,
        help = "Save the index for fast re-export at other depths/formats"
    )]
    save_scip: Option<PathBuf>,
    #[arg(long, help = "rust-analyzer JSON config (features, target, cfg, etc.)")]
    config: Option<PathBuf>,
    #[command(flatten)]
    analyzer: AnalyzerArguments,
}

fn run(cli: Cli, standard_output: &mut dyn Write) -> Result<()> {
    validate_output_paths(
        &[
            cli.output.as_deref(),
            cli.save_scip.as_deref(),
            cli.cycles.cycles_output.as_deref(),
            cli.cycles.file_cycles_output.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>(),
        &[cli.from_scip.as_deref(), cli.config.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
    )?;
    let analyzer_options = AnalyzerOptions::new(cli.analyzer.verbose);
    let index_source: Box<dyn IndexSource + '_> = match cli.from_scip {
        Some(path) => Box::new(ScipFile::new(path)),
        None => Box::new(RustAnalyzer::new(
            &cli.path,
            cli.config.as_deref(),
            &analyzer_options,
        )),
    };
    let index_bytes = index_source.index()?;
    let file_graph = parse_scip(&index_bytes)?;
    let file_graph = if cli.exclude_tests {
        file_graph.without_test_dependencies()
    } else {
        file_graph
    };
    let file_graph = file_graph.within_root(&cli.root)?;
    let folder_graph = file_graph.folders(&cli.root, cli.depth, cli.include_self)?;
    ensure!(
        !folder_graph.nodes().is_empty(),
        "No indexed Rust files under --root {}",
        cli.root.display()
    );
    let folder_cycle_analysis = if cli.cycles.highlight_cycles
        || cli.cycles.cycles_output.is_some()
        || cli.cycles.fail_on_cycles
    {
        Some(analyze_folders(&folder_graph)?)
    } else {
        None
    };
    let folder_report_content = cli
        .cycles
        .cycles_output
        .as_ref()
        .map(|_| {
            folder_report(
                folder_cycle_analysis
                    .as_ref()
                    .context("Folder cycle analysis is missing")?,
                cli.cycles.cycle_limit,
            )
        })
        .transpose()?;
    let file_cycle_analysis =
        if cli.cycles.file_cycles_output.is_some() || cli.cycles.fail_on_cycles {
            Some(analyze_files(&file_graph)?)
        } else {
            None
        };
    let file_report_content = cli
        .cycles
        .file_cycles_output
        .as_ref()
        .map(|_| {
            file_report(
                file_cycle_analysis
                    .as_ref()
                    .context("File cycle analysis is missing")?,
                cli.cycles.cycle_limit,
            )
        })
        .transpose()?;
    let mut diagram_bytes = Vec::new();
    let exporter = cli.format.exporter();
    exporter.write_with_options(
        &folder_graph,
        &ExportOptions::new(
            cli.group_folders,
            folder_cycle_analysis
                .as_ref()
                .filter(|_| cli.cycles.highlight_cycles),
        ),
        &mut diagram_bytes,
    )?;
    if let Some(path) = cli.save_scip {
        fs::write(&path, index_bytes)
            .with_context(|| format!("Cannot save analysis to {}", path.display()))?;
    }
    if let (Some(path), Some(report)) = (cli.cycles.cycles_output, folder_report_content) {
        fs::write(&path, report)
            .with_context(|| format!("Cannot write folder cycle report {}", path.display()))?;
    }
    if let (Some(path), Some(report)) = (cli.cycles.file_cycles_output, file_report_content) {
        fs::write(&path, report)
            .with_context(|| format!("Cannot write file cycle report {}", path.display()))?;
    }
    match cli.output {
        Some(path) => fs::write(&path, diagram_bytes)
            .with_context(|| format!("Cannot write {}", path.display()))?,
        None => standard_output.write_all(&diagram_bytes)?,
    }
    if cli.cycles.fail_on_cycles {
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

const fn should_display_help(has_arguments: bool, has_project_file: bool) -> bool {
    !has_arguments && !has_project_file
}

fn main() -> ExitCode {
    if should_display_help(
        std::env::args_os().nth(1).is_some(),
        contains_project_file(Path::new(".")),
    ) {
        if let Err(error) = Cli::command().print_help() {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }
    if let Err(error) = run(Cli::parse(), &mut io::stdout().lock()) {
        if error
            .downcast_ref::<io::Error>()
            .is_some_and(|source| source.kind() == io::ErrorKind::BrokenPipe)
        {
            return ExitCode::SUCCESS;
        }
        eprintln!("error: {error:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Tests use assertions for expectations while returning Result for fallible setup."
)]
mod cli_tests;
#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Tests use assertions for expectations while returning Result for fallible setup."
)]
mod command_tests;
