use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::bail;

use crate::analyzer::diagnostics::MESSAGE_DEPENDENCY_GRAPH_FAILURE;
use crate::analyzer::diagnostics::MESSAGE_PROJECT_LOADING_FAILURE_REASON;
use crate::analyzer::diagnostics::ScipDiagnostics;
use crate::analyzer::options::Options as AnalyzerOptions;
use crate::diagnostics::MESSAGE_VERBOSE_DIAGNOSTICS_HINT;
use crate::diagnostics::emit;

const PHASE_LABEL_ORIGINAL: &str = "analysis with original configuration";
const PHASE_LABEL_WITHOUT_TEST: &str = "analysis without cfg(test)";
const MESSAGE_ANALYZER_LAUNCH_FAILURE: &str = "Cannot run rust-analyzer";
const MESSAGE_UNREADABLE_ANALYSIS_DATA: &str = "produced no readable analysis data";

#[derive(Clone, Copy)]
pub enum Phase {
    Original,
    WithoutTest,
}

impl Phase {
    #[must_use]
    const fn label(self) -> &'static str {
        match self {
            Self::Original => PHASE_LABEL_ORIGINAL,
            Self::WithoutTest => PHASE_LABEL_WITHOUT_TEST,
        }
    }
}

pub struct ScipProcess<'a> {
    analyzer_command: Command,
    configuration_path: Option<&'a Path>,
    options: &'a AnalyzerOptions,
}

impl<'a> ScipProcess<'a> {
    #[must_use]
    pub fn new(project: &Path, configuration_path: Option<&'a Path>, options: &'a AnalyzerOptions) -> Self {
        let mut analyzer_command = Command::new("rust-analyzer");
        analyzer_command.current_dir(project);
        Self { analyzer_command, configuration_path, options }
    }

    pub fn generate_index(mut self, index_file: &Path, phase: Phase) -> Result<Vec<u8>> {
        let analyzer_command = &mut self.analyzer_command;
        analyzer_command.args(["scip", ".", "--exclude-vendored-libraries", "--num-threads", "0"]);
        analyzer_command.arg("--output").arg(index_file);
        if let Some(configuration_path) = self.configuration_path {
            analyzer_command.arg("--config-path").arg(configuration_path);
        }
        let output = analyzer_command.output().with_context(|| {
            format!(
                "{MESSAGE_ANALYZER_LAUNCH_FAILURE} ({}); check the project directory \
                 and that rust-analyzer is installed and executable",
                phase.label()
            )
        })?;
        if self.options.verbose() {
            emit(format_args!(
                "rust-analyzer ({}) stdout:\n{}\nrust-analyzer ({}) stderr:\n{}",
                phase.label(),
                String::from_utf8_lossy(&output.stdout),
                phase.label(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let diagnostics = ScipDiagnostics::from_streams(&output.stdout, &output.stderr);
        if !output.status.success() {
            bail!(diagnostics.failure_message(phase.label(), &output.status, self.options.verbose()));
        }
        if diagnostics.has_blocking_errors() {
            bail!(diagnostics.failure_message(phase.label(), &MESSAGE_PROJECT_LOADING_FAILURE_REASON, self.options.verbose()));
        }
        fs::read(index_file).with_context(|| {
            format!(
                "{MESSAGE_DEPENDENCY_GRAPH_FAILURE}: {} {MESSAGE_UNREADABLE_ANALYSIS_DATA}. {MESSAGE_VERBOSE_DIAGNOSTICS_HINT}",
                phase.label()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::ffi::OsStr;
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::process::Command;

    use claims::assert_ok;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use tempfile::TempDir;
    use tempfile::tempdir;

    use super::MESSAGE_ANALYZER_LAUNCH_FAILURE;
    use super::MESSAGE_UNREADABLE_ANALYSIS_DATA;
    use super::PHASE_LABEL_ORIGINAL;
    use super::Phase;
    use super::ScipProcess;
    use super::PHASE_LABEL_WITHOUT_TEST;
    use crate::analyzer::diagnostics::MESSAGE_DEPENDENCY_GRAPH_FAILURE;
    use crate::analyzer::diagnostics::MESSAGE_PROJECT_LOADING_FAILURE_REASON;
    use crate::analyzer::options::Options;
    use crate::diagnostics::MESSAGE_VERBOSE_DIAGNOSTICS_HINT;

    const INDEX_BYTES: &[u8] = b"\0\x01\xffSCIP\r\n";
    const PROJECT_DIRECTORY: &str = "project with spaces";

    fn create_rust_analyzer_test_fixture() -> (TempDir, Command) {
        let fixture_creation = tempdir();
        let fixture = assert_ok!(fixture_creation, "The rust-analyzer test fixture directory must be created");
        let test_analyzer_source = fixture.path().join("fake_rust_analyzer.rs");
        let source_write = fs::write(&test_analyzer_source, include_str!("process/test_fixtures/fake_rust_analyzer.rs"));
        assert_ok!(source_write, "The rust-analyzer test double source must be written");
        let test_analyzer_executable = fixture.path().join(format!("fake-rust-analyzer-test{}", env::consts::EXE_SUFFIX));
        let mut compiler = Command::new("rustc");
        /* Enable test-only code without a libtest harness: the double must accept rust-analyzer's command-line arguments. */
        compiler.current_dir(env!("CARGO_MANIFEST_DIR")).args(["--edition=2024", "--cfg", "test"]);
        let compilation = compiler.arg(test_analyzer_source).arg("-o").arg(&test_analyzer_executable).output();
        let compilation = assert_ok!(compilation, "The compiler must run for the rust-analyzer test double");
        let compilation_diagnostics = String::from_utf8_lossy(&compilation.stderr);
        assert!(compilation.status.success(), "rust-analyzer test double compilation failed: {compilation_diagnostics}");
        let project_creation = fs::create_dir_all(fixture.path().join(PROJECT_DIRECTORY));
        assert_ok!(project_creation, "The analyzer fixture project directory must be created");
        let index_write = fs::write(fixture.path().join("index"), INDEX_BYTES);
        assert_ok!(index_write, "The analyzer fixture index bytes must be written");
        let stdout_write = fs::write(fixture.path().join("stdout"), "");
        assert_ok!(stdout_write, "The analyzer fixture stdout must start empty");
        let stderr_write = fs::write(fixture.path().join("stderr"), "");
        assert_ok!(stderr_write, "The analyzer fixture stderr must start empty");
        let exit_code_write = fs::write(fixture.path().join("exit-code"), "0");
        assert_ok!(exit_code_write, "The analyzer fixture must default to a successful exit");
        let output_kind_write = fs::write(fixture.path().join("output-kind"), "file");
        assert_ok!(output_kind_write, "The analyzer fixture must default to writing an index file");
        let mut analyzer_command = Command::new(test_analyzer_executable);
        analyzer_command.current_dir(fixture.path().join(PROJECT_DIRECTORY));
        analyzer_command.env("SOURCE_MESH_RUST_ANALYZER_TEST_DIRECTORY", fixture.path());
        (fixture, analyzer_command)
    }

    #[rstest]
    #[case::original(Phase::Original, PHASE_LABEL_ORIGINAL)]
    #[case::without_test(Phase::WithoutTest, PHASE_LABEL_WITHOUT_TEST)]
    fn test_phase_label(#[case] phase: Phase, #[case] expected: &str) {
        let system_under_test = phase.label();

        assert_eq!(system_under_test, expected);
    }

    #[test]
    fn test_analyzer_command_project() {
        let project = Path::new(PROJECT_DIRECTORY);
        let configuration = Path::new("configuration with spaces.json");
        let options = Options::new(false);

        let system_under_test = ScipProcess::new(project, Some(configuration), &options);

        assert_eq!(system_under_test.analyzer_command.get_program(), OsStr::new("rust-analyzer"));
        assert_eq!(system_under_test.analyzer_command.get_current_dir(), Some(project));
        assert_eq!(system_under_test.configuration_path, Some(configuration));
    }

    #[rstest]
    #[case::original(Phase::Original, false, false)]
    #[case::without_test(Phase::WithoutTest, true, false)]
    #[case::verbose(Phase::Original, true, true)]
    fn test_index_command_and_bytes(#[case] phase: Phase, #[case] configured: bool, #[case] verbose: bool) {
        let (fixture, analyzer_command) = create_rust_analyzer_test_fixture();
        let index_file = fixture.path().join("analysis output.scip");
        let configuration = fixture.path().join("analyzer configuration.json");
        let configuration_write = fs::write(&configuration, "{}");
        assert_ok!(configuration_write, "The analyzer configuration fixture must be written");
        let stdout_write = fs::write(fixture.path().join("stdout"), "This is diagnostic output, not index bytes\n");
        assert_ok!(stdout_write, "The analyzer fixture must emit stdout distinct from index bytes");
        let stderr_write = fs::write(fixture.path().join("stderr"), "warning: optional service unavailable\n");
        assert_ok!(stderr_write, "The analyzer fixture warning must be written");
        let configuration_path = configured.then_some(configuration.as_path());
        let options = Options::new(verbose);
        let system_under_test = ScipProcess { analyzer_command, configuration_path, options: &options };
        let project_resolution = dunce::canonicalize(fixture.path().join(PROJECT_DIRECTORY));
        let expected_directory = assert_ok!(project_resolution, "The fixture project directory must have a canonical path");
        let mut expected_arguments: Vec<_> =
            ["scip", ".", "--exclude-vendored-libraries", "--num-threads", "0", "--output"].map(str::to_owned).into();
        expected_arguments.push(index_file.to_string_lossy().into_owned());
        if configured {
            expected_arguments.push("--config-path".to_owned());
            expected_arguments.push(configuration.to_string_lossy().into_owned());
        }

        let index_generation = system_under_test.generate_index(&index_file, phase);
        let index = assert_ok!(index_generation, "The analyzer must generate an index with nonblocking diagnostics");

        assert_eq!(index, INDEX_BYTES, "Only the requested index file supplies the returned bytes");
        let arguments_read = fs::read_to_string(fixture.path().join("arguments"));
        let arguments = assert_ok!(arguments_read, "The analyzer fixture must record its command-line arguments");
        assert_eq!(arguments.lines().collect::<Vec<_>>(), expected_arguments);
        let directory_read = fs::read_to_string(fixture.path().join("working-directory"));
        let recorded_directory = assert_ok!(directory_read, "The analyzer fixture must record its working directory");
        let directory_resolution = dunce::canonicalize(recorded_directory);
        let working_directory = assert_ok!(directory_resolution, "The recorded working directory must have a canonical path");
        assert_eq!(working_directory, expected_directory);
    }

    #[rstest]
    #[case::quiet(false)]
    #[case::verbose(true)]
    fn test_exit_status_with_readable_index(#[case] verbose: bool) {
        const DETAIL: &str = "backend fixture stopped";
        let (fixture, analyzer_command) = create_rust_analyzer_test_fixture();
        let index_file = fixture.path().join("analysis.scip");
        let exit_code_write = fs::write(fixture.path().join("exit-code"), "23");
        assert_ok!(exit_code_write, "The analyzer fixture must be configured to exit with status 23");
        let stderr_write = fs::write(fixture.path().join("stderr"), DETAIL);
        assert_ok!(stderr_write, "The failed analyzer fixture diagnostic must be written");
        let options = Options::new(verbose);
        let system_under_test = ScipProcess { analyzer_command, configuration_path: None, options: &options };

        let index_generation = system_under_test.generate_index(&index_file, Phase::Original);

        assert!(
            index_generation.as_ref().is_err_and(|error| {
                let message = error.to_string();
                message.contains(MESSAGE_DEPENDENCY_GRAPH_FAILURE) && message.contains(PHASE_LABEL_ORIGINAL)
                    && message.contains("23") && message.contains(MESSAGE_VERBOSE_DIAGNOSTICS_HINT)
            }),
            "A nonzero exit must fail even when the analyzer wrote readable index bytes"
        );
        assert_eq!(index_generation.as_ref().err().map(|error| error.to_string().contains(DETAIL)), Some(verbose));
        let index_read = fs::read(index_file);
        let written_index = assert_ok!(index_read, "The failed analyzer must still have written readable index bytes");
        assert_eq!(written_index, INDEX_BYTES);
    }

    #[rstest]
    #[case::stdout("stdout", Phase::Original)]
    #[case::stderr("stderr", Phase::WithoutTest)]
    fn test_blocking_process_diagnostics(#[case] stream: &str, #[case] phase: Phase) {
        const DETAIL: &str = "error: invalid fixture workspace";
        let (fixture, analyzer_command) = create_rust_analyzer_test_fixture();
        let index_file = fixture.path().join("analysis.scip");
        let diagnostic_write = fs::write(fixture.path().join(stream), DETAIL);
        assert_ok!(diagnostic_write, "The blocking analyzer diagnostic must be written to {stream}");
        let options = Options::new(false);
        let system_under_test = ScipProcess { analyzer_command, configuration_path: None, options: &options };

        let index_generation = system_under_test.generate_index(&index_file, phase);

        assert!(
            index_generation.as_ref().is_err_and(|error| {
                let message = error.to_string();
                message.contains(MESSAGE_PROJECT_LOADING_FAILURE_REASON) && message.contains(phase.label())
                    && message.contains(MESSAGE_VERBOSE_DIAGNOSTICS_HINT) && !message.contains(DETAIL)
            }),
            "A successful exit cannot make project-loading diagnostics safe to ignore"
        );
        let index_read = fs::read(index_file);
        let written_index = assert_ok!(index_read, "The analyzer with blocking diagnostics must still write readable index data");
        assert_eq!(written_index, INDEX_BYTES);
    }

    #[rstest]
    #[case::missing("missing", true)]
    #[case::directory("directory", false)]
    fn test_index_output_failure(#[case] output_kind: &str, #[case] missing: bool) {
        let (fixture, analyzer_command) = create_rust_analyzer_test_fixture();
        let index_file = fixture.path().join("analysis.scip");
        let output_kind_write = fs::write(fixture.path().join("output-kind"), output_kind);
        assert_ok!(output_kind_write, "The analyzer fixture output kind must be configured as {output_kind}");
        let options = Options::new(false);
        let system_under_test = ScipProcess { analyzer_command, configuration_path: None, options: &options };
        let expected_message = format!(
            "{MESSAGE_DEPENDENCY_GRAPH_FAILURE}: {PHASE_LABEL_WITHOUT_TEST} \
             {MESSAGE_UNREADABLE_ANALYSIS_DATA}. {MESSAGE_VERBOSE_DIAGNOSTICS_HINT}"
        );

        let index_generation = system_under_test.generate_index(&index_file, Phase::WithoutTest);

        assert_eq!(index_generation.as_ref().err().map(ToString::to_string), Some(expected_message));
        let cause = index_generation.as_ref().err().and_then(|error| error.downcast_ref::<io::Error>());
        assert!(cause.is_some(), "Unreadable analyzer output must retain its filesystem cause");
        assert_eq!(cause.map(io::Error::kind) == Some(io::ErrorKind::NotFound), missing);
    }

    #[rstest]
    #[case::executable(true, Phase::Original)]
    #[case::project_directory(false, Phase::WithoutTest)]
    fn test_process_launch_failure(#[case] missing_executable: bool, #[case] phase: Phase) {
        let fixture_creation = tempdir();
        let fixture = assert_ok!(fixture_creation, "The analyzer launch failure fixture directory must be created");
        let executable = if missing_executable {
            fixture.path().join("missing executable")
        } else {
            let executable_lookup = env::current_exe();
            assert_ok!(executable_lookup, "The test executable must be available to isolate the missing project error")
        };
        let mut analyzer_command = Command::new(executable);
        let project = if missing_executable { fixture.path().to_path_buf() } else { fixture.path().join("missing project") };
        analyzer_command.current_dir(project);
        let launch_error = analyzer_command.output();
        let expected_cause = launch_error.as_ref().err().map(|error| (error.kind(), error.raw_os_error()));
        assert!(expected_cause.is_some(), "The fixture must fail before launching the child");
        let options = Options::new(false);
        let system_under_test = ScipProcess { analyzer_command, configuration_path: None, options: &options };

        let index_generation = system_under_test.generate_index(&fixture.path().join("analysis.scip"), phase);

        assert!(
            index_generation.as_ref().is_err_and(|error| {
                let message = error.to_string();
                message.contains(MESSAGE_ANALYZER_LAUNCH_FAILURE) && message.contains(phase.label())
            }),
            "Analyzer launch failures must identify the failed phase"
        );
        let cause = index_generation.as_ref().err().and_then(|error| error.downcast_ref::<io::Error>());
        assert_eq!(cause.map(|error| (error.kind(), error.raw_os_error())), expected_cause);
        assert!(cause.is_some_and(|error| error.raw_os_error().is_some()), "The native error code must remain available");
    }
}
