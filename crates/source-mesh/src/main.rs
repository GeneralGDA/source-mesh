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
    #[arg(long, default_value_t = DEFAULT_CYCLE_LIMIT, help = "Maximum cycles per report; fail without output if exceeded")]
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
    #[arg(long, default_value = ".", help = "Restrict to a relative index/project subtree, e.g. src or crates/server/src")]
    root: PathBuf,
    #[arg(long, help = "Keep edges between distinct files collapsed into the same folder")]
    include_self: bool,
    #[arg(long, help = "Nest subfolders, hide shared ancestor groups, and keep their own files as separate nodes")]
    group_folders: bool,
    #[arg(long, help = "Hide dependencies used only by tests")]
    exclude_tests: bool,
    #[command(flatten)]
    cycles: CycleOptions,
    #[arg(long, conflicts_with_all = ["save_scip", "config"], help = "Reuse saved analysis without reanalyzing sources")]
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
mod cli_tests {
    use std::iter;
    use std::process::ExitCode;

    use anyhow::Error;
    use claims::assert_ok;
    use clap::Parser as _;
    use clap::error::ErrorKind;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::BOOLEAN_OPTIONS_DEFAULT_HELP;
    use super::Cli;
    use super::error_message;
    use super::report_error;
    use super::should_display_help;

    #[rstest]
    #[case(&["--depth=-1"])]
    #[case(&["--depth", "not-a-number"])]
    #[case(&["--format", "unknown"])]
    #[case(&["--unknown-option"])]
    #[case(&["--save-scip", "saved.scip"])]
    #[case(&["--config", "config.json"])]
    #[case(&["--cycle-limit", "0"])]
    fn test_invalid_arguments(#[case] arguments: &[&str]) {
        let replay_arguments = [env!("CARGO_PKG_NAME"), "--from-scip", "input.scip"];
        let system_under_test = Cli::try_parse_from(replay_arguments.into_iter().chain(arguments.iter().copied()));

        assert!(system_under_test.is_err(), "CLI accepted invalid or conflicting arguments");
        if let Err(error) = system_under_test {
            assert_ne!(error.kind(), ErrorKind::DisplayHelp, "CLI displayed help instead of rejecting arguments");
            assert!(!error.to_string().is_empty(), "CLI omitted invalid-argument diagnostics");
        }
    }

    #[rstest]
    #[case::no_arguments_without_project(false, false, true)]
    #[case::arguments_without_project(true, false, false)]
    #[case::no_arguments_with_project(false, true, false)]
    #[case::arguments_with_project(true, true, false)]
    fn test_help_display_conditions(#[case] has_arguments: bool, #[case] has_project_file: bool, #[case] expected: bool) {
        let system_under_test = should_display_help(has_arguments, has_project_file);

        assert_eq!(system_under_test, expected, "CLI help display policy changed");
    }

    #[test]
    fn test_folder_grouping_help() {
        let system_under_test = Cli::try_parse_from([env!("CARGO_PKG_NAME"), "--help"]);

        assert!(system_under_test.is_err(), "CLI did not return help");
        if let Err(error) = system_under_test {
            let help = error.to_string();

            assert_eq!(error.kind(), ErrorKind::DisplayHelp, "CLI rejected the help argument");
            assert!(help.contains("--group-folders"), "CLI help omitted folder grouping");
            assert!(help.contains(BOOLEAN_OPTIONS_DEFAULT_HELP), "CLI help omitted boolean option defaults");
        }
    }

    #[test]
    fn test_cycle_failure_argument() {
        let system_under_test = Cli::try_parse_from([env!("CARGO_PKG_NAME"), "--fail-on-cycles"]);
        let system_under_test = assert_ok!(system_under_test, "CLI must accept the cycle-failure flag");

        assert!(system_under_test.cycles.fail_on_cycles, "CLI did not enable cycle failure");
    }

    #[rstest]
    #[case(&["--analyzer-threads", "0"])]
    #[case(&["--analysis-mode", "sequential"])]
    fn test_removed_analyzer_arguments(#[case] arguments: &[&str]) {
        let system_under_test = Cli::try_parse_from(iter::once(env!("CARGO_PKG_NAME")).chain(arguments.iter().copied()));

        assert!(system_under_test.is_err(), "CLI accepted a removed analyzer option");
        if let Err(error) = system_under_test {
            assert_eq!(error.kind(), ErrorKind::UnknownArgument, "Removed analyzer options must be unknown arguments");
        }
    }

    #[rstest]
    fn test_error_detail_visibility(#[values(false, true)] verbose: bool) {
        const MESSAGE_FAILURE: &str = "Cannot resolve dependency definitions";
        const MESSAGE_DIAGNOSTIC: &str = "Internal analyzer diagnostic";
        let fixture = Error::msg(MESSAGE_DIAGNOSTIC).context(MESSAGE_FAILURE);

        let system_under_test = error_message(&fixture, verbose);

        assert!(system_under_test.starts_with(MESSAGE_FAILURE), "The failure reason is missing");
        assert_eq!(system_under_test.contains(MESSAGE_DIAGNOSTIC), verbose, "Diagnostic details must require verbose output");
    }

    #[test]
    fn test_error_report() {
        let mut fixture = Vec::new();

        let system_under_test = report_error("Cannot analyze project", &mut fixture);

        assert_eq!(system_under_test, ExitCode::FAILURE, "Reporting an error must return the failure exit code");
        assert_eq!(fixture, b"error: Cannot analyze project\n", "Error reporting must write the complete diagnostic");
    }

    #[rstest]
    #[case::unavailable(0, b"")]
    #[case::partial(3, b"err")]
    fn test_error_report_write_failure(#[case] capacity: usize, #[case] expected: &[u8]) {
        let mut fixture = vec![0; capacity];
        let mut standard_error = fixture.as_mut_slice();

        let system_under_test = report_error("Cannot analyze project", &mut standard_error);

        assert_eq!(system_under_test, ExitCode::FAILURE, "Unavailable stderr must preserve the failure exit code");
        assert_eq!(fixture, expected, "Error reporting must retain the bytes written before stderr became unavailable");
    }

    #[rstest]
    #[case::default(&[], false)]
    #[case::verbose(&["--verbose"], true)]
    #[case::short(&["-v"], true)]
    fn test_verbose_argument(#[case] arguments: &[&str], #[case] enabled: bool) {
        let system_under_test = Cli::try_parse_from(iter::once(env!("CARGO_PKG_NAME")).chain(arguments.iter().copied()));
        let system_under_test = assert_ok!(system_under_test, "CLI must accept supported verbosity arguments: {arguments:?}");

        assert_eq!(system_under_test.analyzer.verbose, enabled, "CLI verbosity must follow the supplied arguments");
    }
}

#[cfg(test)]
mod command_tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;

    use anyhow::Result;
    use claims::assert_ok;
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

    use super::Cli;
    use super::run;

    #[rstest]
    #[case::malformed_index(true, Path::new("."))]
    #[case::outside_root(false, Path::new(".."))]
    fn test_invalid_input_output_preservation(#[case] malformed_index: bool, #[case] root: &Path) {
        const CONTENTS_OUTPUT_EXISTING: &str = "previous diagram";
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Invalid-input fixture must have a temporary directory");
        let index_path = fixture.path().join("input.scip");
        let diagram_path = fixture.path().join("diagram.dot");
        let index = if malformed_index {
            b"this is not protobuf".to_vec()
        } else {
            let index = acyclic_index_fixture();
            assert_ok!(index, "Outside-root scenario must start with a valid serialized acyclic index")
        };
        let index_write_result = fs::write(&index_path, index);
        assert_ok!(index_write_result, "Invalid-input scenario must write its SCIP fixture");
        let diagram_write_result = fs::write(&diagram_path, CONTENTS_OUTPUT_EXISTING);
        assert_ok!(diagram_write_result, "Existing diagram must be written before testing invalid input");
        let system_under_test = Cli::try_parse_from([
            OsStr::new(env!("CARGO_PKG_NAME")),
            OsStr::new("--from-scip"), index_path.as_os_str(),
            OsStr::new("--root"), root.as_os_str(),
            OsStr::new("--output"), diagram_path.as_os_str(),
        ]);
        let system_under_test = assert_ok!(system_under_test, "Invalid-input fixture arguments must parse");
        let mut output = Vec::new();

        let command_result = run(system_under_test, &mut output);
        let preserved = fs::read_to_string(&diagram_path);
        let preserved = assert_ok!(preserved, "Existing diagram must remain readable after invalid input");

        assert!(command_result.is_err(), "Command accepted an invalid index or subtree");
        if let Err(error) = command_result {
            assert!(!error.to_string().is_empty(), "Command omitted invalid-input diagnostics");
        }
        assert!(output.is_empty(), "Command wrote to stdout for invalid input");
        assert_eq!(preserved, CONTENTS_OUTPUT_EXISTING, "Command overwrote the existing diagram");
    }

    fn acyclic_index_fixture() -> Result<Vec<u8>> {
        Ok(Index {
            documents: vec![Document { relative_path: "src/item.rs".into(), ..Document::default() }],
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
            occurrences.extend(references.iter().map(|&target| Occurrence { symbol: target.into(), ..Occurrence::default() }));
            Document { relative_path: path.into(), occurrences, ..Document::default() }
        }).collect();
        Ok(Index { documents, ..Index::default() }.write_to_bytes()?)
    }

    #[rstest]
    #[case::folder_report(None)]
    #[case::file_report(Some(0))]
    fn test_cycle_limit_output_preservation(#[case] depth: Option<usize>, #[values(false, true)] diagram_to_file: bool) {
        const CONTENTS_OUTPUT_EXISTING: &str = "previous result";
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Cycle-limit fixture must have a temporary directory");
        let index_path = fixture.path().join("input.scip");
        let diagram_path = fixture.path().join("diagram.dot");
        let folder_report_path = fixture.path().join("folders.md");
        let file_report_path = fixture.path().join("files.md");
        let index = cyclic_index_fixture();
        let index = assert_ok!(index, "Cycle-limit scenario must have a valid serialized cyclic index");
        let index_write_result = fs::write(&index_path, index);
        assert_ok!(index_write_result, "Cycle-limit scenario must write its SCIP fixture");
        for path in [&diagram_path, &folder_report_path, &file_report_path] {
            let output_write_result = fs::write(path, CONTENTS_OUTPUT_EXISTING);
            assert_ok!(output_write_result, "Cycle-limit scenario must create existing output at {}", path.display());
        }
        let system_under_test = Cli::try_parse_from([
            OsStr::new(env!("CARGO_PKG_NAME")),
            OsStr::new("--from-scip"), index_path.as_os_str(),
            OsStr::new("--cycle-limit"), OsStr::new("1"),
            OsStr::new("--cycles-output"), folder_report_path.as_os_str(),
            OsStr::new("--file-cycles-output"), file_report_path.as_os_str(),
        ]);
        let mut system_under_test = assert_ok!(system_under_test, "CLI must accept cycle-limit and report-output arguments");
        system_under_test.depth = depth;
        system_under_test.output = diagram_to_file.then(|| diagram_path.clone());
        let mut output = Vec::new();

        let command_result = run(system_under_test, &mut output);

        assert!(command_result.is_err(), "Command accepted more cycles than the limit");
        if let Err(error) = command_result {
            assert!(error.to_string().contains("Cycle count exceeds"), "Failure must be due to the cycle limit: {error:#}");
        }
        assert!(output.is_empty(), "Command wrote to stdout before checking the cycle limit");
        for path in [&diagram_path, &folder_report_path, &file_report_path] {
            let preserved = fs::read_to_string(path);
            let preserved = assert_ok!(preserved, "Cycle-limit failure must leave {} readable", path.display());
            assert_eq!(preserved, CONTENTS_OUTPUT_EXISTING, "Cycle limit error overwrote {}", path.display());
        }
    }

    #[rstest]
    #[case::report_cycles(false)]
    #[case::cycle_gate(true)]
    fn test_cyclic_graph_outputs(#[case] fail_on_cycles: bool, #[values(false, true)] diagram_to_file: bool) {
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Cyclic-output fixture must have a temporary directory");
        let index_path = fixture.path().join("input.scip");
        let diagram_path = fixture.path().join("diagram.graphml");
        let folder_report_path = fixture.path().join("folders.md");
        let file_report_path = fixture.path().join("files.md");
        let index = cyclic_index_fixture();
        let index = assert_ok!(index, "Cyclic-output scenario must have a valid serialized index");
        let index_write_result = fs::write(&index_path, index);
        assert_ok!(index_write_result, "Cyclic-output scenario must write its SCIP fixture");
        let system_under_test = Cli::try_parse_from([
            OsStr::new(env!("CARGO_PKG_NAME")),
            OsStr::new("--from-scip"), index_path.as_os_str(),
            OsStr::new("--format"), OsStr::new("graphml"),
            OsStr::new("--highlight-cycles"),
            OsStr::new("--cycles-output"), folder_report_path.as_os_str(),
            OsStr::new("--file-cycles-output"), file_report_path.as_os_str(),
        ]);
        let mut system_under_test = assert_ok!(system_under_test, "CLI must accept GraphML highlighting and cycle reports");
        system_under_test.cycles.fail_on_cycles = fail_on_cycles;
        system_under_test.output = diagram_to_file.then(|| diagram_path.clone());
        let mut output = Vec::new();

        let command_result = run(system_under_test, &mut output);
        let standard_output_is_empty = output.is_empty();
        let diagram = if diagram_to_file {
            let diagram = fs::read_to_string(&diagram_path);
            assert_ok!(diagram, "Cyclic graph command must produce a readable diagram file")
        } else {
            let diagram = String::from_utf8(output);
            assert_ok!(diagram, "Cyclic graph command must emit a UTF-8 diagram on stdout")
        };
        let folder_report = fs::read_to_string(&folder_report_path);
        let folder_report = assert_ok!(folder_report, "Cyclic graph command must produce a readable folder report");
        let file_report = fs::read_to_string(&file_report_path);
        let file_report = assert_ok!(file_report, "Cyclic graph command must produce a readable file report");

        assert_eq!(command_result.is_err(), fail_on_cycles, "Fail on cycles only when requested: {command_result:?}");
        if let Err(error) = command_result {
            assert!(error.to_string().contains("Dependency cycles found:"), "Failure must be due to cycles: {error:#}");
        }
        assert_eq!(standard_output_is_empty, diagram_to_file, "Diagram output did not follow the selected destination");
        assert!(diagram.contains("cycle_group"), "Cyclic graph did not produce a highlighted diagram");
        assert!(folder_report.contains("## C1 ("), "Cyclic graph did not produce a folder cycle report");
        assert!(file_report.contains("## C1 ("), "Cyclic graph did not produce a file cycle report");
    }

    #[test]
    fn test_acyclic_graph_cycle_gate() {
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Acyclic cycle-gate fixture must have a temporary directory");
        let index_path = fixture.path().join("input.scip");
        let index = acyclic_index_fixture();
        let index = assert_ok!(index, "Cycle-gate scenario must have a valid serialized acyclic index");
        let index_write_result = fs::write(&index_path, index);
        assert_ok!(index_write_result, "Acyclic cycle-gate scenario must write its SCIP fixture");
        let system_under_test = Cli::try_parse_from([
            OsStr::new(env!("CARGO_PKG_NAME")),
            OsStr::new("--from-scip"), index_path.as_os_str(),
            OsStr::new("--root"), OsStr::new("src"),
            OsStr::new("--fail-on-cycles"),
        ]);
        let system_under_test = assert_ok!(system_under_test, "CLI must accept the acyclic replay and cycle-gate arguments");
        let mut output = Vec::new();

        let command_result = run(system_under_test, &mut output);

        assert_ok!(command_result, "Cycle gate must accept an acyclic graph");
        assert!(!output.is_empty(), "Cycle gate did not export the acyclic graph");
    }
}
