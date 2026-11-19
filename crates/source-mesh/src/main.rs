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
    use assertables::assert_contains;
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
        assert_contains!(help, "--group-folders", "CLI help omitted folder grouping");
        assert_contains!(help, BOOLEAN_OPTIONS_DEFAULT_HELP, "CLI help omitted boolean option defaults");
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
    use std::path::PathBuf;
    use std::process::ExitCode;

    use anyhow::Result;
    use assertables::assert_contains;
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

    struct CommandTestFixture {
        directory: TempDir,
    }

    impl CommandTestFixture {
        #[must_use]
        fn new() -> Self {
            let directory = tempdir();
            let directory = assert_ok!(directory, "Command fixture directory must be created");
            Self { directory }
        }

        #[must_use]
        fn path(&self) -> &Path {
            self.directory.path()
        }

        fn make_file(&self, path: impl AsRef<Path>, contents: Option<impl AsRef<[u8]>>) -> PathBuf {
            let path = self.path().join(path);
            if let Some(contents) = contents {
                let written = fs::write(&path, contents);
                assert_ok!(written, "Command fixture file must be written at {}", path.display());
            }
            path
        }
    }

    mod fixture_tests {
        use std::fs;
        use std::io::ErrorKind;

        use claims::assert_err;
        use claims::assert_ok;
        use pretty_assertions::assert_eq;
        use rstest::rstest;

        use super::CommandTestFixture;

        #[rstest]
        #[case::empty(b"", b"")]
        #[case::text(b"fixture contents", b"fixture contents")]
        #[case::binary(b"\0\xffSCIP", b"\0\xffSCIP")]
        fn test_make_file_contents(#[case] contents: &[u8], #[case] expected: &[u8]) {
            let system_under_test = CommandTestFixture::new();

            let path = system_under_test.make_file("input.dat", Some(contents));

            let actual = fs::read(path);
            let actual = assert_ok!(actual, "The created fixture file must be readable");
            assert_eq!(actual, expected, "Fixture files must preserve all explicitly supplied contents");
        }

        #[rstest]
        #[case::missing(false)]
        #[case::existing(true)]
        fn test_make_file_without_contents(#[case] existing: bool) {
            const CONTENTS_EXISTING: &[u8] = b"existing fixture contents";
            let system_under_test = CommandTestFixture::new();
            let file_to_make = "input.dat";
            system_under_test.make_file(file_to_make, existing.then_some(CONTENTS_EXISTING));

            let path = system_under_test.make_file(file_to_make, None::<&[u8]>);
            let actual = fs::read(path);

            if existing {
                let actual = assert_ok!(actual, "An absent content argument must leave the existing file readable");
                assert_eq!(actual, CONTENTS_EXISTING, "An absent content argument must not modify the existing file");
            } else {
                let error = assert_err!(actual, "An absent content argument must not create the file");
                assert_eq!(error.kind(), ErrorKind::NotFound, "The fixture file must remain absent");
            }
        }
    }

    fn run(matches: ArgMatches, output: &mut dyn std::io::Write) -> Result<()> {
        let cli = Cli::from_arg_matches(&matches)?;
        LoadedSettings::load(cli, matches)?.into_command()?.run(output)
    }

    fn automatic_settings_test_fixture(contents: Option<&str>, arguments: &[&str]) -> (CommandTestFixture, ArgMatches) {
        let fixture = CommandTestFixture::new();
        fixture.make_file(SETTINGS_FILE_NAME, contents);
        /* Select the invocation directory without changing the working directory shared by parallel tests. */
        let project_arguments = [OsStr::new(env!("CARGO_PKG_NAME")), fixture.path().as_os_str()];
        let arguments = project_arguments.into_iter().chain(arguments.iter().map(OsStr::new));
        let matches = Cli::command().try_get_matches_from(arguments);
        let matches = assert_ok!(matches, "Automatic settings fixture arguments must parse");
        (fixture, matches)
    }

    #[rstest]
    #[case::absent(None, true)]
    #[case::replay(Some("[analysis]\nfrom-scip = 'input.scip'"), false)]
    fn test_no_arguments_settings_discovery(#[case] contents: Option<&str>, #[case] display_help: bool) {
        let (fixture, matches) = automatic_settings_test_fixture(contents, &[]);
        fixture.make_file("input.scip", Some(acyclic_index_fixture()));
        let has_project_file = contains_project_file(fixture.path());

        let system_under_test = load_command(matches, false, has_project_file);
        let system_under_test = assert_ok!(system_under_test, "Automatic settings discovery must precede no-argument help");

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
        let (fixture, matches) = automatic_settings_test_fixture(Some(contents), &[]);
        let has_project_file = contains_project_file(fixture.path());
        let mut diagnostic = Vec::new();

        let message = load_command(matches, false, has_project_file).err();
        let message = assert_some!(message, "Invalid automatic settings must fail instead of selecting help");
        let system_under_test = report_error(&message, &mut diagnostic);
        let diagnostic = String::from_utf8(diagnostic);
        let diagnostic = assert_ok!(diagnostic, "Automatic-settings diagnostics must be UTF-8");

        assert_eq!(system_under_test, ExitCode::FAILURE, "Malformed automatic settings must fail the invocation");
        assert!(diagnostic.starts_with("error: Cannot parse settings"), "Startup must report the settings parse error");
        assert_contains!(diagnostic, SETTINGS_FILE_NAME, "The diagnostic must identify the automatic settings file");
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
        let (fixture, matches) = automatic_settings_test_fixture(Some(&contents), argument.as_slice());
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
        let fixture = CommandTestFixture::new();
        let settings_path = fixture.make_file("missing.toml", None::<&str>);
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
        let fixture = CommandTestFixture::new();
        let index = if malformed_index { b"this is not protobuf".to_vec() } else { acyclic_index_fixture() };
        let index_path = fixture.make_file("input.scip", Some(index));
        let diagram_path = fixture.make_file("diagram.dot", Some(CONTENTS_OUTPUT_EXISTING));
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

    #[must_use]
    fn acyclic_index_fixture() -> Vec<u8> {
        let index = Index {
            documents: vec![Document { relative_path: "src/item.rs".into(), ..Document::default() }],
            ..Index::default()
        }.write_to_bytes();
        assert_ok!(index, "Failed to serialize the acyclic index fixture")
    }

    #[must_use]
    fn cyclic_index_fixture() -> Vec<u8> {
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
        let index = Index { documents, ..Index::default() }.write_to_bytes();
        assert_ok!(index, "Failed to serialize the cyclic index fixture")
    }

    #[rstest]
    fn test_cycle_gate_failure_outputs(#[values(false, true)] diagram_to_file: bool) {
        const CONTENTS_OUTPUT_EXISTING: &str = "previous result";
        let fixture = CommandTestFixture::new();
        let index_path = fixture.make_file("input.scip", Some(cyclic_index_fixture()));
        let diagram_path = fixture.make_file("diagram.graphml", Some(CONTENTS_OUTPUT_EXISTING));
        let folder_report_path = fixture.make_file("folders.md", Some(CONTENTS_OUTPUT_EXISTING));
        let file_report_path = fixture.make_file("files.md", Some(CONTENTS_OUTPUT_EXISTING));

        let mut arguments = vec![
            OsStr::new(env!("CARGO_PKG_NAME")),
            OsStr::new("--from-scip"), index_path.as_os_str(),
            OsStr::new("--format"), OsStr::new("graphml"),
            OsStr::new("--no-settings"), OsStr::new("--cycle-limit"), OsStr::new("1"),
            OsStr::new("--highlight-cycles"), OsStr::new("--fail-on-cycles"),
            OsStr::new("--cycles-output"), folder_report_path.as_os_str(),
            OsStr::new("--file-cycles-output"), file_report_path.as_os_str(),
        ];
        if diagram_to_file {
            arguments.extend([OsStr::new("--output"), diagram_path.as_os_str()]);
        }
        let system_under_test = Cli::command().try_get_matches_from(arguments);
        let system_under_test = assert_ok!(system_under_test, "Failed to parse GraphML highlighting and cycle-report arguments");
        let mut output = Vec::new();

        let command_result = run(system_under_test, &mut output);
        assert_eq!(output.is_empty(), diagram_to_file, "Diagram output did not follow the selected destination");
        let diagram_file = fs::read_to_string(&diagram_path);
        let diagram_file = assert_ok!(diagram_file, "Failed to read the diagram file");
        let diagram = if diagram_to_file {
            diagram_file
        } else {
            assert_eq!(diagram_file, CONTENTS_OUTPUT_EXISTING, "Unrequested diagram file contents changed");
            let diagram = String::from_utf8(output);
            assert_ok!(diagram, "Failed to decode the stdout diagram as UTF-8")
        };
        let folder_report = fs::read_to_string(&folder_report_path);
        let folder_report = assert_ok!(folder_report, "Failed to read the folder cycle report");
        let file_report = fs::read_to_string(&file_report_path);
        let file_report = assert_ok!(file_report, "Failed to read the file cycle report");

        assert!(command_result.is_err(), "Command unexpectedly succeeded with the cycle gate enabled");
        assert_contains!(diagram, "cycle_group", "Diagram is missing the cycle_group marker");
        assert_eq!(folder_report.lines().filter(|line| line.starts_with("- ")).count(), 1);
        assert_eq!(file_report.lines().filter(|line| line.starts_with("- ")).count(), 1);
    }

    #[rstest]
    fn test_outgoing_dependency_failure_outputs(
        #[values("files", "folders")] mode: &str,
        #[values(false, true)] diagram_to_file: bool,
    ) {
        let fixture = CommandTestFixture::new();
        let index_path = fixture.make_file("input.scip", Some(cyclic_index_fixture()));
        let diagram_path = fixture.make_file("diagram.dot", None::<&str>);
        let report_path = fixture.make_file("outgoing.md", None::<&str>);
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
        assert_contains!(diagnostic, &expected_check, "Diagnostic must identify the check");
        assert_contains!(report, &expected_mode, "Report must use the requested dependency mode");
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
        let fixture = CommandTestFixture::new();
        let settings_path = fixture.make_file("settings.toml", Some(CONTENTS_SETTINGS));
        let index_path = fixture.make_file("input.scip", Some(acyclic_index_fixture()));
        let output_path = if alias {
            let path = fixture.make_file("alias.toml", None::<&str>);
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

        assert_contains!(message, "must not overwrite input", "Unexpected settings failure: {error:#}");
        assert_eq!(preserved, CONTENTS_SETTINGS, "The settings file changed");
        assert!(output.is_empty(), "An input conflict must be detected before writing the diagram");
    }

    #[rstest]
    #[case::unknown_key("[graph]\nexlcude = []", "exlcude")]
    #[case::invalid_glob("[graph]\nexclude = ['[']", "exclusion pattern")]
    #[case::missing_limit("[outgoing-dependencies]\nmode = 'files'", "require")]
    fn test_invalid_settings_output_preservation(#[case] settings: &str, #[case] diagnostic: &str) {
        const CONTENTS_EXISTING: &str = "previous diagram";
        let fixture = CommandTestFixture::new();
        let settings_path = fixture.make_file("settings.toml", Some(settings));
        let output_path = fixture.make_file("diagram.dot", Some(CONTENTS_EXISTING));
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

        assert_contains!(message, diagnostic, "Settings must fail before opening analysis: {error:#}");
        assert_eq!(preserved, CONTENTS_EXISTING, "Invalid settings changed an existing output");
        assert!(output.is_empty(), "Invalid settings must not produce a diagram");
    }

    #[test]
    fn test_acyclic_graph_cycle_gate() {
        let fixture = CommandTestFixture::new();
        let index_path = fixture.make_file("input.scip", Some(acyclic_index_fixture()));
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
