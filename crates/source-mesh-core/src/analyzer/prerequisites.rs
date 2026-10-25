use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;

const INSTALL_HINT: &str =
    "Install the required components for the active project toolchain with `rustup component add rust-analyzer rust-src`.";
const ENV_RUST_SOURCE_PATH: &str = "RUST_SRC_PATH";
const MESSAGE_ANALYZER_LAUNCH_ADVICE: &str = "check the project directory and that rust-analyzer is installed and executable.";
const MESSAGE_ANALYZER_UNAVAILABLE: &str =
    "rust-analyzer is unavailable for the active project toolchain or does not support SCIP indexing.";

#[derive(Debug)]
enum PrerequisiteLaunchError {
    AnalyzerProbe(PathBuf),
    RustToolchainQuery(PathBuf),
}

impl fmt::Display for PrerequisiteLaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::AnalyzerProbe(ref project) => write!(
                formatter,
                "Cannot run `rust-analyzer scip --help` in {}; {MESSAGE_ANALYZER_LAUNCH_ADVICE} {INSTALL_HINT}",
                project.display()
            ),
            Self::RustToolchainQuery(ref project) => write!(
                formatter,
                "Cannot run `rustc --print sysroot` in {}; check the project directory \
                 and that rustc is installed and executable",
                project.display()
            ),
        }
    }
}

impl Error for PrerequisiteLaunchError {}

pub(crate) fn ensure_available(project: &Path) -> Result<()> {
    let analyzer_probe = {
        let mut command = Command::new("rust-analyzer");
        command.current_dir(project).args(["scip", "--help"]);
        command.output().map(|output| output.status.success())
    };
    validate_analyzer_probe(project, analyzer_probe)?;

    let configured_source_directory = env::var_os(ENV_RUST_SOURCE_PATH).map(PathBuf::from);
    validate_rust_sources(configured_source_directory.as_deref(), || active_rust_toolchain_directory(project))
}

fn validate_analyzer_probe(project: &Path, analyzer_probe: io::Result<bool>) -> Result<()> {
    let supports_scip = analyzer_probe.with_context(|| PrerequisiteLaunchError::AnalyzerProbe(project.to_path_buf()))?;
    ensure!(supports_scip, "{MESSAGE_ANALYZER_UNAVAILABLE} {INSTALL_HINT}");
    Ok(())
}

fn validate_rust_sources(
    configured_source_directory: Option<&Path>,
    active_rust_toolchain_directory: impl FnOnce() -> Result<PathBuf>,
) -> Result<()> {
    let source_directory = if let Some(source_directory) = configured_source_directory {
        source_directory.to_path_buf()
    } else {
        standard_library_source_directory(&active_rust_toolchain_directory()?)
    };
    let source_file = source_directory.join("core").join("src").join("lib.rs");
    fs::read(&source_file).with_context(|| {
        if configured_source_directory.is_some() {
            format!("Cannot read Rust sources configured by {ENV_RUST_SOURCE_PATH} at {}", source_file.display())
        } else {
            format!(
                "rust-src is required for the active project toolchain; cannot read {}. {INSTALL_HINT}",
                source_file.display()
            )
        }
    })?;
    Ok(())
}

#[must_use]
fn standard_library_source_directory(rust_toolchain_directory: &Path) -> PathBuf {
    assert!(!rust_toolchain_directory.as_os_str().is_empty(), "Standard library source lookup needs a Rust toolchain directory");
    rust_toolchain_directory.join(PathBuf::from_iter(["lib", "rustlib", "src", "rust", "library"]))
}

fn active_rust_toolchain_directory(project: &Path) -> Result<PathBuf> {
    let toolchain_query = Command::new("rustc").current_dir(project).args(["--print", "sysroot"]).output();
    let output = toolchain_query.with_context(|| PrerequisiteLaunchError::RustToolchainQuery(project.to_path_buf()))?;
    ensure!(
        output.status.success(),
        "Cannot query the active Rust toolchain directory with `rustc --print sysroot`. {INSTALL_HINT}"
    );
    let output = std::str::from_utf8(&output.stdout).context("rustc returned a non-UTF-8 Rust toolchain directory")?;
    let rust_toolchain_directory = output.trim();
    ensure!(!rust_toolchain_directory.is_empty(), "rustc returned an empty Rust toolchain directory");
    Ok(PathBuf::from(rust_toolchain_directory))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::panic::catch_unwind;
    use std::path::Path;
    use std::path::PathBuf;

    use anyhow::Result;
    use claims::assert_ok;
    use claims::assert_some;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use tempfile::tempdir;

    use super::INSTALL_HINT;
    use super::MESSAGE_ANALYZER_LAUNCH_ADVICE;
    use super::MESSAGE_ANALYZER_UNAVAILABLE;
    use super::PrerequisiteLaunchError;
    use super::active_rust_toolchain_directory;
    use super::ensure_available;
    use super::standard_library_source_directory;
    use super::validate_analyzer_probe;
    use super::validate_rust_sources;

    fn write_rust_source_fixture(source_directory: &Path) -> Result<()> {
        let core_source_directory = source_directory.join("core").join("src");
        fs::create_dir_all(&core_source_directory)?;
        fs::write(core_source_directory.join("lib.rs"), "")?;
        Ok(())
    }

    #[rstest]
    #[case::analyzer_probe(PrerequisiteLaunchError::AnalyzerProbe)]
    #[case::toolchain_query(PrerequisiteLaunchError::RustToolchainQuery)]
    fn test_launch_error_project_diagnostic(#[case] failure: fn(PathBuf) -> PrerequisiteLaunchError) {
        const NAME_PROJECT: &str = "project with spaces";
        let system_under_test = failure(PathBuf::from(NAME_PROJECT));
        let project_context = format!(" in {NAME_PROJECT}; ");

        let diagnostic = system_under_test.to_string();

        assert!(diagnostic.contains(&project_context), "Launch diagnostics must identify the requested directory: {diagnostic}");
    }

    #[test]
    fn test_prerequisite_probe_launch_error_propagation() {
        let fixture_creation = tempdir();
        let fixture = assert_ok!(fixture_creation, "The prerequisite probe fixture directory must be created");
        let project = fixture.path().join("missing project");

        let system_under_test = ensure_available(&project);

        let error = assert_some!(system_under_test.err(), "The prerequisite probe must fail to launch");
        let launch_error = error.downcast_ref::<PrerequisiteLaunchError>();
        let launch_error = assert_some!(launch_error, "Prerequisite orchestration must retain its launch error: {error:#}");
        assert!(
            matches!(*launch_error, PrerequisiteLaunchError::AnalyzerProbe(ref directory) if directory == &project),
            "Prerequisite orchestration must retain the analyzer launch context and requested directory: {error:#}"
        );
        let cause = error.downcast_ref::<io::Error>();
        let cause = assert_some!(cause, "Prerequisite orchestration must preserve the analyzer probe I/O error");
        assert!(cause.raw_os_error().is_some(), "The analyzer probe must retain its native launch error");
    }

    #[test]
    fn test_standard_library_source_layout() {
        let expected = PathBuf::from_iter(["toolchain", "lib", "rustlib", "src", "rust", "library"]);

        let system_under_test = standard_library_source_directory(Path::new("toolchain"));

        assert_eq!(system_under_test, expected);
    }

    #[test]
    fn test_standard_library_source_toolchain_precondition() {
        let system_under_test = catch_unwind(|| standard_library_source_directory(Path::new("")));

        assert!(system_under_test.is_err(), "An empty toolchain directory violates the source-layout precondition");
    }

    #[test]
    fn test_active_toolchain_directory() {
        let project = Path::new(env!("CARGO_MANIFEST_DIR"));

        let toolchain_query = active_rust_toolchain_directory(project);
        let system_under_test = assert_ok!(toolchain_query, "The compiler must report its active toolchain directory");

        assert!(system_under_test.is_absolute(), "The active Rust toolchain must have a nonempty absolute sysroot");
        let directory_read = fs::read_dir(&system_under_test);
        let _directory_entries = assert_ok!(directory_read, "The reported Rust sysroot must be a readable directory");
    }

    #[test]
    fn test_active_toolchain_launch_error_propagation() {
        let fixture_creation = tempdir();
        let fixture = assert_ok!(fixture_creation, "The toolchain query fixture directory must be created");
        let project = fixture.path().join("missing project");

        let system_under_test = active_rust_toolchain_directory(&project);

        let error = assert_some!(system_under_test.err(), "The toolchain query must fail to launch");
        let launch_error = error.downcast_ref::<PrerequisiteLaunchError>();
        let launch_error = assert_some!(launch_error, "The toolchain query must retain its launch error: {error:#}");
        assert!(
            matches!(*launch_error, PrerequisiteLaunchError::RustToolchainQuery(ref directory) if directory == &project),
            "The toolchain query must retain its launch context and requested directory: {error:#}"
        );
        let cause = error.downcast_ref::<io::Error>();
        let cause = assert_some!(cause, "The toolchain query must preserve its I/O error");
        assert!(cause.raw_os_error().is_some(), "The toolchain query must retain its native launch error");
    }

    #[test]
    fn test_available_analyzer() {
        let system_under_test = validate_analyzer_probe(Path::new("project"), Ok(true));

        assert_ok!(system_under_test, "Available rust-analyzer must be accepted");
    }

    #[rstest]
    #[case::launch(Err(io::Error::new(io::ErrorKind::NotFound, "launch failure")), MESSAGE_ANALYZER_LAUNCH_ADVICE)]
    #[case::unsupported(Ok(false), MESSAGE_ANALYZER_UNAVAILABLE)]
    fn test_unavailable_analyzer_diagnostic(#[case] analyzer_probe: io::Result<bool>, #[case] expected_message: &str) {
        let system_under_test = validate_analyzer_probe(Path::new("project"), analyzer_probe);

        let error = assert_some!(system_under_test.err(), "An unavailable analyzer must be rejected");
        let message = error.to_string();
        assert!(message.contains(expected_message), "Unavailable analyzer did not produce an actionable error: {message}");
        assert!(message.contains(INSTALL_HINT), "Unavailable analyzer must include installation advice: {message}");
    }

    #[rstest]
    #[case::missing_path(io::ErrorKind::NotFound)]
    #[case::denied(io::ErrorKind::PermissionDenied)]
    #[case::invalid_directory(io::ErrorKind::NotADirectory)]
    fn test_analyzer_spawn_error_identity(#[case] kind: io::ErrorKind) {
        const CAUSE_MESSAGE: &str = "Cannot launch probe";
        let project = Path::new("project");
        let failure = io::Error::new(kind, CAUSE_MESSAGE);

        let system_under_test = validate_analyzer_probe(project, Err(failure));

        let error = assert_some!(system_under_test.err(), "A failed analyzer launch must be propagated");
        let launch_error = error.downcast_ref::<PrerequisiteLaunchError>();
        let launch_error = assert_some!(launch_error, "The analyzer probe must retain its launch error: {error:#}");
        assert!(
            matches!(*launch_error, PrerequisiteLaunchError::AnalyzerProbe(ref directory) if directory == project),
            "The analyzer probe must retain its launch context and requested directory: {error:#}"
        );
        let cause = error.downcast_ref::<io::Error>();
        let cause = assert_some!(cause, "Analyzer launch errors must remain downcastable to their I/O cause");
        assert_eq!(cause.kind(), kind, "The analyzer launch error must preserve its original I/O kind");
        assert_eq!(cause.to_string(), CAUSE_MESSAGE, "The analyzer launch error must preserve its original I/O message");
    }

    #[test]
    fn test_rust_source_read_error() {
        let system_under_test = {
            let fixture_creation = tempdir();
            let fixture = assert_ok!(fixture_creation, "The unreadable Rust source fixture directory must be created");
            let source_directory_creation = fs::create_dir_all(fixture.path().join("core").join("src").join("lib.rs"));
            assert_ok!(source_directory_creation, "The source-file path must contain a directory to provoke a read error");
            validate_rust_sources(Some(fixture.path()), || {
                unreachable!("Configured sources must bypass Rust toolchain directory lookup")
            })
        };

        assert!(
            system_under_test.is_err_and(|error| {
                error.downcast_ref::<io::Error>().is_some_and(|cause| cause.kind() != io::ErrorKind::NotFound)
            }),
            "An unreadable source file was reported as missing rust-src"
        );
    }

    #[test]
    fn test_installed_rust_sources() {
        let toolchain_creation = tempdir();
        let rust_toolchain_directory = assert_ok!(toolchain_creation, "The installed toolchain fixture must be created");
        let relative_source_directory = PathBuf::from_iter(["lib", "rustlib", "src", "rust", "library"]);
        let source_directory = rust_toolchain_directory.path().join(relative_source_directory);
        let source_directory_creation = fs::create_dir_all(&source_directory);
        assert_ok!(source_directory_creation, "The standard library source directory must be created");
        let source_write = write_rust_source_fixture(&source_directory);
        assert_ok!(source_write, "The installed rust-src fixture must be written");
        let system_under_test = validate_rust_sources(None, || Ok(rust_toolchain_directory.path().to_path_buf()));

        assert_ok!(system_under_test, "Installed rust-src must be accepted");
    }

    #[test]
    fn test_missing_rust_sources() {
        let toolchain_creation = tempdir();
        let rust_toolchain_directory = assert_ok!(toolchain_creation, "The toolchain fixture without rust-src must be created");
        let system_under_test = validate_rust_sources(None, || Ok(rust_toolchain_directory.path().to_path_buf()));

        assert!(
            system_under_test.as_ref().is_err_and(|error| error.to_string().contains("rust-src is required")),
            "Missing rust-src did not produce an actionable error"
        );
    }

    #[test]
    fn test_configured_rust_sources() {
        let source_directory_creation = tempdir();
        let source_directory = assert_ok!(source_directory_creation, "The configured Rust source fixture must be created");
        let source_write = write_rust_source_fixture(source_directory.path());
        assert_ok!(source_write, "The configured Rust sources must be written");
        let system_under_test = validate_rust_sources(Some(source_directory.path()), || {
            unreachable!("Configured sources must bypass Rust toolchain directory lookup")
        });

        assert_ok!(system_under_test, "Configured Rust sources must be accepted");
    }
}
