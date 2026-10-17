use std::io;
use std::io::Write;
use std::num::NonZeroUsize;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Error;
use anyhow::Result;
use clap::Args;
use clap::CommandFactory as _;
use clap::Parser;
use clap::ValueEnum;

use source_mesh_core::CycleOptions as LibraryCycleOptions;
use source_mesh_core::DEFAULT_CYCLE_LIMIT;
use source_mesh_core::Input;
use source_mesh_core::Project;
use source_mesh_core::Request;
use source_mesh_core::analyzer_options::Options as AnalyzerOptions;
use source_mesh_core::backend::contains_project_file;
use source_mesh_core::export::Format as ExportFormat;

const BOOLEAN_OPTIONS_DEFAULT_HELP: &str = "All boolean options are disabled by default.";

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Dot,
    Mermaid,
    Graphml,
}

impl From<Format> for ExportFormat {
    fn from(format: Format) -> Self {
        match format {
            Format::Dot => Self::Dot,
            Format::Mermaid => Self::Mermaid,
            Format::Graphml => Self::Graphml,
        }
    }
}

#[derive(Args)]
struct CycleOptions {
    #[arg(long, help = "Highlight cyclic folder components with distinct bright colors")]
    highlight_cycles: bool,
    #[arg(long, help = "Exit unsuccessfully after writing outputs if file or folder dependency cycles are found")]
    fail_on_cycles: bool,
    #[arg(long, help = "Write every simple folder cycle to a Markdown report")]
    cycles_output: Option<PathBuf>,
    #[arg(long, help = "Write file cycles before folder folding to a separate Markdown report")]
    file_cycles_output: Option<PathBuf>,
    #[arg(
        long,
        default_value_t = DEFAULT_CYCLE_LIMIT,
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
    #[arg(default_value = ".", help = "Project/workspace directory or Cargo.toml (unused with --from-scip)")]
    path: PathBuf,
    #[arg(short, long, value_enum, default_value = "dot")]
    format: Format,
    #[arg(short, long, help = "Output file (otherwise stdout)")]
    output: Option<PathBuf>,
    #[arg(short, long, help = "Maximum folder levels below --root: 0 = root only; default = all")]
    depth: Option<usize>,
    #[arg(
        long, default_value = ".",
        help = "Restrict to this relative subtree of the index/project, e.g. src or crates/server/src"
    )]
    root: PathBuf,
    #[arg(long, help = "Keep edges between distinct files collapsed into the same folder")]
    include_self: bool,
    #[arg(long, help = "Nest subfolders, hide shared ancestor groups, and keep their own files as separate nodes")]
    group_folders: bool,
    #[arg(long, help = "Hide dependencies used only by tests")]
    exclude_tests: bool,
    #[command(flatten)]
    cycles: CycleOptions,
    #[arg(long, conflicts_with_all = ["save_scip", "config"], help = "Reuse saved analysis without analyzing the source again")]
    from_scip: Option<PathBuf>,
    #[arg(long, help = "Save the index for fast re-export at other depths/formats")]
    save_scip: Option<PathBuf>,
    #[arg(long, help = "rust-analyzer JSON config (features, target, cfg, etc.)")]
    config: Option<PathBuf>,
    #[command(flatten)]
    analyzer: AnalyzerArguments,
}

fn run(cli: Cli, standard_output: &mut dyn Write) -> Result<()> {
    let input = match cli.from_scip {
        Some(path) => Input::ScipFile(path),
        None => Input::Project(Project::new(cli.path, cli.config)),
    };
    let cycles = LibraryCycleOptions::builder();
    let cycles = cycles.highlight_cycles(cli.cycles.highlight_cycles).fail_on_cycles(cli.cycles.fail_on_cycles);
    let cycles = cycles.maybe_cycles_output(cli.cycles.cycles_output).maybe_file_cycles_output(cli.cycles.file_cycles_output);
    let cycles = cycles.cycle_limit(cli.cycles.cycle_limit).build();
    let request = Request::builder().input(input).root(cli.root).format(cli.format.into());
    let request = request.maybe_output(cli.output).maybe_depth(cli.depth);
    let request = request.include_self(cli.include_self).group_folders(cli.group_folders).exclude_tests(cli.exclude_tests);
    let request = request.cycles(cycles).maybe_save_scip(cli.save_scip);
    let request = request.analyzer(AnalyzerOptions::new(cli.analyzer.verbose)).build();
    source_mesh_core::run(&request, standard_output)
}

#[must_use]
const fn should_display_help(has_arguments: bool, has_project_file: bool) -> bool {
    !has_arguments && !has_project_file
}

#[must_use]
fn error_message(error: &Error, verbose: bool) -> String {
    if verbose {
        format!("{error:#}")
    } else {
        error.to_string()
    }
}

fn report_error(message: &str, standard_error: &mut dyn Write) -> ExitCode {
    let _: io::Result<()> = writeln!(standard_error, "error: {message}");
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    if should_display_help(std::env::args_os().nth(1).is_some(), contains_project_file(Path::new("."))) {
        if let Err(error) = Cli::command().print_help() {
            return report_error(&error.to_string(), &mut io::stderr().lock());
        }
        return ExitCode::SUCCESS;
    }
    let cli = Cli::parse();
    let verbose = cli.analyzer.verbose;
    if let Err(error) = run(cli, &mut io::stdout().lock()) {
        if error.downcast_ref::<io::Error>().is_some_and(|source| source.kind() == io::ErrorKind::BrokenPipe) {
            return ExitCode::SUCCESS;
        }
        return report_error(&error_message(&error, verbose), &mut io::stderr().lock());
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
