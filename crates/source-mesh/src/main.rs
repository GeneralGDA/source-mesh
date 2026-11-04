use std::io;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use anyhow::Error;
use clap::ArgMatches;
use clap::CommandFactory as _;
use clap::FromArgMatches as _;
use source_mesh_core::backend::contains_project_file;

use settings::Cli;
use settings::Command;
use settings::LoadedSettings;

mod settings;

#[must_use]
const fn should_display_help(has_arguments: bool, has_project_file: bool, has_settings: bool) -> bool {
    !has_arguments && !has_project_file && !has_settings
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

fn load_command(matches: ArgMatches, has_arguments: bool, has_project_file: bool) -> Result<Option<Command>, String> {
    let cli = Cli::from_arg_matches(&matches).map_err(|error| error.to_string())?;
    let cli_verbose = cli.verbose();
    let settings = LoadedSettings::load(cli, matches).map_err(|error| error_message(&error, cli_verbose))?;
    if should_display_help(has_arguments, has_project_file, settings.has_settings()) {
        return Ok(None);
    }
    let effective_verbose = settings.verbose();
    settings.into_command().map(Some).map_err(|error| error_message(&error, effective_verbose))
}

fn main() -> ExitCode {
    let matches = Cli::command().get_matches();
    let has_arguments = std::env::args_os().nth(1).is_some();
    let has_project_file = contains_project_file(Path::new("."));
    let command = match load_command(matches, has_arguments, has_project_file) {
        Ok(Some(command)) => command,
        Ok(None) => {
            if let Err(error) = Cli::command().print_help() {
                return report_error(&error.to_string(), &mut io::stderr().lock());
            }
            return ExitCode::SUCCESS;
        }
        Err(message) => return report_error(&message, &mut io::stderr().lock()),
    };
    if let Err(error) = command.run(&mut io::stdout().lock()) {
        if error.downcast_ref::<io::Error>().is_some_and(|source| source.kind() == io::ErrorKind::BrokenPipe) {
            return ExitCode::SUCCESS;
        }
        return report_error(&error_message(&error, command.verbose()), &mut io::stderr().lock());
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod cli_tests {
    use std::process::ExitCode;

    use anyhow::Error;
    use claims::assert_some;
    use clap::Parser as _;
    use clap::error::ErrorKind;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::error_message;
    use super::report_error;
    use super::settings::BOOLEAN_OPTIONS_DEFAULT_HELP;
    use super::settings::Cli;
    use super::should_display_help;

    #[rstest]
    #[case::no_arguments_without_project(false, false, false, true)]
    #[case::arguments_without_project(true, false, false, false)]
    #[case::no_arguments_with_project(false, true, false, false)]
    #[case::arguments_with_project(true, true, false, false)]
    #[case::settings_without_arguments_or_project(false, false, true, false)]
    #[case::settings_with_arguments(true, false, true, false)]
    #[case::settings_with_project(false, true, true, false)]
    #[case::settings_with_arguments_and_project(true, true, true, false)]
    fn test_help_display_conditions(
        #[case] has_arguments: bool,
        #[case] has_project_file: bool,
        #[case] has_settings: bool,
        #[case] expected: bool,
    ) {
        let system_under_test = should_display_help(has_arguments, has_project_file, has_settings);

        assert_eq!(system_under_test, expected, "CLI help display policy changed");
    }

    #[test]
    fn test_folder_grouping_help() {
        let system_under_test = Cli::try_parse_from([env!("CARGO_PKG_NAME"), "--help"]).err();
        let error = assert_some!(system_under_test, "CLI did not return help");
        let help = error.to_string();

        assert_eq!(error.kind(), ErrorKind::DisplayHelp, "CLI rejected the help argument");
        assert!(help.contains("--group-folders"), "CLI help omitted folder grouping");
        assert!(help.contains(BOOLEAN_OPTIONS_DEFAULT_HELP), "CLI help omitted boolean option defaults");
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
}

#[cfg(test)]
mod command_tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;
    use std::process::ExitCode;

    use anyhow::Result;
    use claims::assert_ok;
    use claims::assert_some;
    use clap::ArgMatches;
    use clap::CommandFactory as _;
    use clap::FromArgMatches as _;
    use pretty_assertions::assert_eq;
    use protobuf::Enum as _;
    use protobuf::Message as _;
    use rstest::rstest;
    use scip::types::Document;
    use scip::types::Index;
    use scip::types::Occurrence;
    use scip::types::SymbolRole;
    use source_mesh_core::backend::contains_project_file;
    use tempfile::TempDir;
    use tempfile::tempdir;

    use super::error_message;
    use super::load_command;
    use super::report_error;
    use super::settings::Cli;
    use super::settings::LoadedSettings;
    use super::settings::SETTINGS_FILE_NAME;

    fn run(matches: ArgMatches, output: &mut dyn std::io::Write) -> Result<()> {
        let cli = Cli::from_arg_matches(&matches)?;
        LoadedSettings::load(cli, matches)?.into_command()?.run(output)
    }

    fn automatic_settings_test_fixture(contents: Option<&str>, arguments: &[&str]) -> Result<(TempDir, ArgMatches)> {
        let fixture = tempdir()?;
        if let Some(contents) = contents {
            fs::write(fixture.path().join(SETTINGS_FILE_NAME), contents)?;
        }
        /* Select the invocation directory without changing the working directory shared by parallel tests. */
        let project_arguments = [OsStr::new(env!("CARGO_PKG_NAME")), fixture.path().as_os_str()];
        let arguments = project_arguments.into_iter().chain(arguments.iter().map(OsStr::new));
        let matches = Cli::command().try_get_matches_from(arguments)?;
        Ok((fixture, matches))
    }

    #[rstest]
    #[case::absent(None, true)]
    #[case::replay(Some("[analysis]\nfrom-scip = 'input.scip'"), false)]
    fn test_no_arguments_settings_discovery(#[case] contents: Option<&str>, #[case] display_help: bool) {
        let fixture = automatic_settings_test_fixture(contents, &[]);
        let (fixture, matches) = assert_ok!(fixture, "Automatic settings fixture must prepare its invocation directory");
        let index = acyclic_index_fixture();
        let index = assert_ok!(index, "No-argument replay must have valid saved analysis");
        let index_write = fs::write(fixture.path().join("input.scip"), index);
        assert_ok!(index_write, "Saved analysis must be written beside automatic settings");
        let has_project_file = contains_project_file(fixture.path());

        let system_under_test = load_command(matches, false, has_project_file);
        let system_under_test = assert_ok!(system_under_test, "Automatic settings discovery must precede no-argument help");

        assert!(!has_project_file, "The replay fixture must not contain a Cargo manifest");
        assert_eq!(system_under_test.is_none(), display_help, "Loaded replay settings must prevent automatic help");
        if let Some(command) = system_under_test {
            let mut output = Vec::new();
            let result = command.run(&mut output);

            assert_ok!(result, "Settings-only replay must run without a Cargo manifest");
            assert!(!output.is_empty(), "Settings-only replay must produce a diagram rather than skip execution");
        }
    }

    #[rstest]
    #[case::malformed("[analysis")]
    #[case::unknown_key("[analysis]\nverbsoe = true")]
    fn test_no_arguments_settings_errors(#[case] contents: &str) {
        let fixture = automatic_settings_test_fixture(Some(contents), &[]);
        let (fixture, matches) = assert_ok!(fixture, "Invalid automatic settings must be written before startup");
        let has_project_file = contains_project_file(fixture.path());
        let mut diagnostic = Vec::new();

        let message = load_command(matches, false, has_project_file).err();
        let message = assert_some!(message, "Invalid automatic settings must fail instead of selecting help");
        let system_under_test = report_error(&message, &mut diagnostic);
        let diagnostic = String::from_utf8(diagnostic);
        let diagnostic = assert_ok!(diagnostic, "Automatic-settings diagnostics must be UTF-8");

        assert!(!has_project_file, "Invalid settings must be tested without a Cargo manifest");
        assert_eq!(system_under_test, ExitCode::FAILURE, "Malformed automatic settings must fail the invocation");
        assert!(diagnostic.starts_with("error: Cannot parse settings"), "Startup must report the settings parse error");
        assert!(diagnostic.contains(SETTINGS_FILE_NAME), "The diagnostic must identify the automatic settings file");
    }

    #[rstest]
    #[case::settings_quiet(false, None, false)]
    #[case::settings_verbose(true, None, true)]
    #[case::cli_quiet(true, Some("--verbose=false"), false)]
    #[case::cli_verbose(false, Some("--verbose"), true)]
    fn test_settings_validation_diagnostic_verbosity(
        #[case] configured: bool,
        #[case] argument: Option<&str>,
        #[case] verbose: bool,
    ) {
        let contents = format!("[graph]\nexclude = ['[']\n[analysis]\nverbose = {configured}");
        let fixture = automatic_settings_test_fixture(Some(&contents), argument.as_slice());
        let (fixture, matches) = assert_ok!(fixture, "Validation diagnostics require valid TOML with an invalid exclusion glob");
        let has_project_file = contains_project_file(fixture.path());
        let mut diagnostic = Vec::new();

        let message = load_command(matches, argument.is_some(), has_project_file).err();
        let message = assert_some!(message, "An invalid exclusion glob must fail after settings merge");
        let system_under_test = report_error(&message, &mut diagnostic);
        let diagnostic = String::from_utf8(diagnostic);
        let diagnostic = assert_ok!(diagnostic, "Validation diagnostics must be UTF-8");

        assert_eq!(system_under_test, ExitCode::FAILURE, "Invalid merged exclusions must fail the invocation");
        assert!(diagnostic.starts_with("error: Invalid exclusion pattern"), "Validation must identify the invalid exclusion");
        assert_eq!(diagnostic.contains("unclosed character class"), verbose, "Diagnostics must use effective verbosity");
    }

    #[rstest]
    fn test_settings_load_diagnostic_verbosity(#[values(false, true)] verbose: bool) {
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Missing-settings diagnostics require a temporary directory");
        let settings_path = fixture.path().join("missing.toml");
        let verbosity_argument = format!("--verbose={verbose}");
        let matches = Cli::command().try_get_matches_from([
            OsStr::new(env!("CARGO_PKG_NAME")), OsStr::new("--settings"), settings_path.as_os_str(),
            OsStr::new(&verbosity_argument),
        ]);
        let matches = assert_ok!(matches, "CLI verbosity must parse before settings loading fails");
        let mut diagnostic = Vec::new();

        let message = load_command(matches, true, false).err();
        let message = assert_some!(message, "Explicitly selected missing settings must fail loading");
        let system_under_test = report_error(&message, &mut diagnostic);
        let diagnostic = String::from_utf8(diagnostic);
        let diagnostic = assert_ok!(diagnostic, "Settings-loading diagnostics must be UTF-8");

        assert_eq!(system_under_test, ExitCode::FAILURE, "Settings loading errors must fail the invocation");
        assert!(diagnostic.starts_with("error: Cannot load settings"), "Startup must report the settings loading error");
        assert_eq!(diagnostic.contains("Cannot open input file"), verbose, "Loading errors must use CLI verbosity");
    }

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
        let system_under_test = Cli::command().try_get_matches_from([
            OsStr::new(env!("CARGO_PKG_NAME")),
            OsStr::new("--from-scip"), index_path.as_os_str(),
            OsStr::new("--root"), root.as_os_str(),
            OsStr::new("--output"), diagram_path.as_os_str(),
        ]);
        let system_under_test = assert_ok!(system_under_test, "Invalid-input fixture arguments must parse");
        let mut output = Vec::new();

        let error = run(system_under_test, &mut output).err();
        let error = assert_some!(error, "Command accepted an invalid index or subtree");
        let message = error.to_string();
        let preserved = fs::read_to_string(&diagram_path);
        let preserved = assert_ok!(preserved, "Existing diagram must remain readable after invalid input");

        assert!(!message.is_empty(), "Command omitted invalid-input diagnostics");
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
        let depth_argument = depth.map(|depth| depth.to_string());
        let mut arguments = vec![
            OsStr::new(env!("CARGO_PKG_NAME")),
            OsStr::new("--from-scip"), index_path.as_os_str(),
            OsStr::new("--cycle-limit"), OsStr::new("1"),
            OsStr::new("--cycles-output"), folder_report_path.as_os_str(),
            OsStr::new("--file-cycles-output"), file_report_path.as_os_str(),
        ];
        if let Some(depth_argument) = depth_argument.as_deref() {
            arguments.extend([OsStr::new("--depth"), OsStr::new(depth_argument)]);
        }
        if diagram_to_file {
            arguments.extend([OsStr::new("--output"), diagram_path.as_os_str()]);
        }
        let system_under_test = Cli::command().try_get_matches_from(arguments);
        let system_under_test = assert_ok!(system_under_test, "CLI must accept cycle-limit and report-output arguments");
        let mut output = Vec::new();

        let error = run(system_under_test, &mut output).err();
        let error = assert_some!(error, "Command accepted more cycles than the limit");
        let message = error.to_string();
        let preserved = [&diagram_path, &folder_report_path, &file_report_path].map(|path| {
            let preserved = fs::read_to_string(path);
            assert_ok!(preserved, "Cycle-limit failure must leave {} readable", path.display())
        });

        assert!(message.contains("Cycle count exceeds"), "Failure must be due to the cycle limit: {error:#}");
        assert!(output.is_empty(), "Command wrote to stdout before checking the cycle limit");
        assert_eq!(preserved, [CONTENTS_OUTPUT_EXISTING; 3], "Cycle limit error overwrote an output");
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
        let cycle_gate_argument = format!("--fail-on-cycles={fail_on_cycles}");
        let mut arguments = vec![
            OsStr::new(env!("CARGO_PKG_NAME")),
            OsStr::new("--from-scip"), index_path.as_os_str(),
            OsStr::new("--format"), OsStr::new("graphml"),
            OsStr::new("--highlight-cycles"), OsStr::new(&cycle_gate_argument),
            OsStr::new("--cycles-output"), folder_report_path.as_os_str(),
            OsStr::new("--file-cycles-output"), file_report_path.as_os_str(),
        ];
        if diagram_to_file {
            arguments.extend([OsStr::new("--output"), diagram_path.as_os_str()]);
        }
        let system_under_test = Cli::command().try_get_matches_from(arguments);
        let system_under_test = assert_ok!(system_under_test, "CLI must accept GraphML highlighting and cycle reports");
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
            let message = error.to_string();
            assert!(message.contains("Dependency cycles found:"), "Failure must be due to cycles: {error:#}");
        }
        assert_eq!(standard_output_is_empty, diagram_to_file, "Diagram output did not follow the selected destination");
        assert!(diagram.contains("cycle_group"), "Cyclic graph did not produce a highlighted diagram");
        assert!(folder_report.contains("## C1 ("), "Cyclic graph did not produce a folder cycle report");
        assert!(file_report.contains("## C1 ("), "Cyclic graph did not produce a file cycle report");
    }

    #[rstest]
    fn test_outgoing_dependency_failure_outputs(
        #[values("files", "folders")] mode: &str,
        #[values(false, true)] diagram_to_file: bool,
    ) {
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Outgoing dependency fixture must have a temporary directory");
        let index_path = fixture.path().join("input.scip");
        let diagram_path = fixture.path().join("diagram.dot");
        let report_path = fixture.path().join("outgoing.md");
        let index = cyclic_index_fixture();
        let index = assert_ok!(index, "Outgoing dependency scenario must have a valid serialized index");
        let written = fs::write(&index_path, index);
        assert_ok!(written, "Outgoing dependency scenario must write its SCIP fixture");
        let mut arguments = vec![
            OsStr::new(env!("CARGO_PKG_NAME")),
            OsStr::new("--from-scip"), index_path.as_os_str(),
            OsStr::new("--max-outgoing-dependencies"), OsStr::new("1"),
            OsStr::new("--outgoing-dependencies-mode"), OsStr::new(mode),
            OsStr::new("--outgoing-dependencies-output"), report_path.as_os_str(),
        ];
        if diagram_to_file {
            arguments.extend([OsStr::new("--output"), diagram_path.as_os_str()]);
        }
        let system_under_test = Cli::command().try_get_matches_from(arguments);
        let system_under_test = assert_ok!(system_under_test, "CLI must accept outgoing dependency check and report options");
        let mut output = Vec::new();
        let mut diagnostic = Vec::new();

        let error = run(system_under_test, &mut output).err();
        let error = assert_some!(error, "Two outgoing targets must exceed a maximum of one");
        let status = report_error(&error_message(&error, false), &mut diagnostic);
        let diagnostic = String::from_utf8(diagnostic);
        let diagnostic = assert_ok!(diagnostic, "Dependency diagnostic must be UTF-8");
        let report = fs::read_to_string(report_path);
        let report = assert_ok!(report, "The violation report must be written before command failure");
        let source = if mode == "files" { Path::new("second").join("item.rs") } else { "second".into() };
        let source = source.to_string_lossy();
        let expected_violation = format!("- {source:?}: 2 outgoing dependencies (maximum: 1)");
        let expected_check = format!("mode: {mode}, maximum: 1");
        let expected_mode = format!("Mode: {mode}");
        let violations: Vec<_> = diagnostic.lines().filter(|line| line.starts_with("- ")).collect();
        let report_violation_count = report.lines().filter(|line| line.starts_with("- ")).count();

        assert_eq!(status, ExitCode::FAILURE, "A threshold violation must fail the command");
        assert_eq!(violations, [expected_violation], "Normal stderr must list exactly the violating source and count");
        assert!(diagnostic.starts_with("error: "), "CLI must emit the failure on the error stream");
        assert!(diagnostic.contains(&expected_check), "Diagnostic must identify the check");
        assert!(report.contains(&expected_mode), "Report must use the requested dependency mode");
        assert_eq!(report_violation_count, 1, "Report must list the violation");
        assert_eq!(output.is_empty(), diagram_to_file, "Diagnostics must not replace or contaminate the diagram");
        if diagram_to_file {
            let diagram = fs::read(diagram_path);
            let diagram = assert_ok!(diagram, "Diagram must be written before threshold failure");
            assert!(!diagram.is_empty(), "Threshold failure must preserve the requested diagram");
        }
    }

    #[rstest]
    #[case::same_path(false)]
    #[case::hard_link(true)]
    fn test_settings_input_output_preservation(#[case] alias: bool) {
        const CONTENTS_SETTINGS: &str = "[export]\nformat = 'dot'\n";
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Settings protection fixture directory must be created");
        let settings_path = fixture.path().join("settings.toml");
        let settings_write = fs::write(&settings_path, CONTENTS_SETTINGS);
        assert_ok!(settings_write, "Settings must be written before testing output protection");
        let index_path = fixture.path().join("input.scip");
        let index = acyclic_index_fixture();
        let index = assert_ok!(index, "Settings protection requires valid analysis");
        let index_write = fs::write(&index_path, index);
        assert_ok!(index_write, "Settings protection must write its saved analysis");
        let output_path = if alias {
            let path = fixture.path().join("alias.toml");
            let linked = fs::hard_link(&settings_path, &path);
            assert_ok!(linked, "Settings alias must identify the loaded file");
            path
        } else {
            settings_path.clone()
        };
        let system_under_test = Cli::command().try_get_matches_from([
            OsStr::new(env!("CARGO_PKG_NAME")),
            OsStr::new("--settings"), settings_path.as_os_str(),
            OsStr::new("--from-scip"), index_path.as_os_str(),
            OsStr::new("--output"), output_path.as_os_str(),
        ]);
        let system_under_test = assert_ok!(system_under_test, "Settings protection arguments must parse");
        let mut output = Vec::new();

        let error = run(system_under_test, &mut output).err();
        let error = assert_some!(error, "The command must not overwrite its TOML settings");
        let message = error.to_string();
        let preserved = fs::read_to_string(&settings_path);
        let preserved = assert_ok!(preserved, "Loaded settings must remain readable after output rejection");

        assert!(message.contains("must not overwrite input"), "Unexpected settings failure: {error:#}");
        assert_eq!(preserved, CONTENTS_SETTINGS, "The settings file changed");
        assert!(output.is_empty(), "An input conflict must be detected before writing the diagram");
    }

    #[rstest]
    #[case::unknown_key("[graph]\nexlcude = []", "exlcude")]
    #[case::invalid_glob("[graph]\nexclude = ['[']", "exclusion pattern")]
    #[case::missing_limit("[outgoing-dependencies]\nmode = 'files'", "require")]
    fn test_invalid_settings_output_preservation(#[case] settings: &str, #[case] diagnostic: &str) {
        const CONTENTS_EXISTING: &str = "previous diagram";
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "Invalid-settings fixture directory must be created");
        let settings_path = fixture.path().join("settings.toml");
        let settings_write = fs::write(&settings_path, settings);
        assert_ok!(settings_write, "Invalid settings fixture must be written");
        let output_path = fixture.path().join("diagram.dot");
        let output_write = fs::write(&output_path, CONTENTS_EXISTING);
        assert_ok!(output_write, "Existing output must be written before loading invalid settings");
        let system_under_test = Cli::command().try_get_matches_from([
            OsStr::new(env!("CARGO_PKG_NAME")),
            OsStr::new("--settings"), settings_path.as_os_str(),
            OsStr::new("--from-scip"), OsStr::new("missing.scip"),
            OsStr::new("--output"), output_path.as_os_str(),
        ]);
        let system_under_test = assert_ok!(system_under_test, "Invalid-settings arguments must parse before validation");
        let mut output = Vec::new();

        let error = run(system_under_test, &mut output).err();
        let error = assert_some!(error, "The command accepted invalid settings");
        let message = error.to_string();
        let preserved = fs::read_to_string(&output_path);
        let preserved = assert_ok!(preserved, "Invalid settings must leave the output readable");

        assert!(message.contains(diagnostic), "Settings must fail before opening analysis: {error:#}");
        assert_eq!(preserved, CONTENTS_EXISTING, "Invalid settings changed an existing output");
        assert!(output.is_empty(), "Invalid settings must not produce a diagram");
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
        let system_under_test = Cli::command().try_get_matches_from([
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
