use std::env;
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
const RUST_SOURCE_PATH_VARIABLE: &str = "RUST_SRC_PATH";

pub(crate) fn ensure_available(project: &Path) -> Result<()> {
    let analyzer_probe = {
        let mut command = Command::new("rust-analyzer");
        command.current_dir(project).args(["scip", "--help"]);
        command.output().map(|output| output.status.success())
    };
    validate_analyzer_probe(project, analyzer_probe)?;

    let configured_source_directory = env::var_os(RUST_SOURCE_PATH_VARIABLE).map(PathBuf::from);
    validate_rust_sources(configured_source_directory.as_deref(), || active_rust_toolchain_directory(project))
}

fn validate_analyzer_probe(project: &Path, analyzer_probe: io::Result<bool>) -> Result<()> {
    let supports_scip = analyzer_probe.with_context(|| {
        format!(
            "Cannot run `rust-analyzer scip --help` in {}; check the project directory \
             and that rust-analyzer is installed and executable. {INSTALL_HINT}",
            project.display()
        )
    })?;
    ensure!(
        supports_scip,
        "rust-analyzer is unavailable for the active project toolchain or does not support SCIP indexing. {INSTALL_HINT}"
    );
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
            format!("Cannot read Rust sources configured by {RUST_SOURCE_PATH_VARIABLE} at {}", source_file.display())
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
    let output = Command::new("rustc").current_dir(project).args(["--print", "sysroot"]).output().with_context(|| {
        format!(
            "Cannot run `rustc --print sysroot` in {}; check the project directory \
             and that rustc is installed and executable",
            project.display()
        )
    })?;
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
#[expect(
    clippy::panic_in_result_fn,
    reason = "Tests use assertions for expectations while returning Result for fallible setup."
)]
mod tests {
    use std::fs;
    use std::io;
    use std::path::Path;

    use anyhow::Result;
    use rstest::rstest;
    use tempfile::tempdir;

    use super::standard_library_source_directory;
    use super::validate_analyzer_probe;
    use super::validate_rust_sources;

    fn write_rust_source_fixture(source_directory: &Path) -> Result<()> {
        let core_source_directory = source_directory.join("core").join("src");
        fs::create_dir_all(&core_source_directory)?;
        fs::write(core_source_directory.join("lib.rs"), "")?;
        Ok(())
    }

    #[test]
    fn test_available_analyzer() {
        let system_under_test = validate_analyzer_probe(Path::new("project"), Ok(true));

        assert!(system_under_test.is_ok(), "Available rust-analyzer was rejected");
    }

    #[rstest]
    #[case::missing(Err(io::Error::new(io::ErrorKind::NotFound, "missing executable")), "check the project directory")]
    #[case::unsupported(Ok(false), "does not support SCIP indexing")]
    fn test_unavailable_analyzer(#[case] analyzer_probe: io::Result<bool>, #[case] expected_message: &str) {
        let system_under_test = validate_analyzer_probe(Path::new("project"), analyzer_probe);

        assert!(
            system_under_test.as_ref().is_err_and(|error| error.to_string().contains(expected_message)),
            "Unavailable analyzer did not produce an actionable error"
        );
    }

    #[rstest]
    #[case::missing_path(io::ErrorKind::NotFound)]
    #[case::denied(io::ErrorKind::PermissionDenied)]
    #[case::invalid_directory(io::ErrorKind::NotADirectory)]
    fn test_analyzer_spawn_error_identity(#[case] kind: io::ErrorKind) {
        let failure = io::Error::new(kind, "Cannot launch probe");

        let system_under_test = validate_analyzer_probe(Path::new("project"), Err(failure));

        assert!(
            system_under_test.is_err_and(|error| error.downcast_ref::<io::Error>().is_some_and(|cause| cause.kind() == kind)),
            "Analyzer launch error lost its original cause"
        );
    }

    #[test]
    fn test_rust_source_read_error() {
        let system_under_test = (|| -> Result<()> {
            let fixture = tempdir()?;
            fs::create_dir_all(fixture.path().join("core").join("src").join("lib.rs"))?;
            validate_rust_sources(Some(fixture.path()), || {
                unreachable!("Configured sources must bypass Rust toolchain directory lookup")
            })
        })();

        assert!(
            system_under_test.is_err_and(|error| {
                error.downcast_ref::<io::Error>().is_some_and(|cause| cause.kind() != io::ErrorKind::NotFound)
            }),
            "An unreadable source file was reported as missing rust-src"
        );
    }

    #[test]
    fn test_installed_rust_sources() -> Result<()> {
        let rust_toolchain_directory = tempdir()?;
        let source_directory = standard_library_source_directory(rust_toolchain_directory.path());
        fs::create_dir_all(&source_directory)?;
        write_rust_source_fixture(&source_directory)?;
        let system_under_test = validate_rust_sources(None, || Ok(rust_toolchain_directory.path().to_path_buf()));

        assert!(system_under_test.is_ok(), "Installed rust-src was rejected");
        Ok(())
    }

    #[test]
    fn test_missing_rust_sources() -> Result<()> {
        let rust_toolchain_directory = tempdir()?;
        let system_under_test = validate_rust_sources(None, || Ok(rust_toolchain_directory.path().to_path_buf()));

        assert!(
            system_under_test.as_ref().is_err_and(|error| error.to_string().contains("rust-src is required")),
            "Missing rust-src did not produce an actionable error"
        );
        Ok(())
    }

    #[test]
    fn test_configured_rust_sources() -> Result<()> {
        let source_directory = tempdir()?;
        write_rust_source_fixture(source_directory.path())?;
        let system_under_test = validate_rust_sources(Some(source_directory.path()), || {
            unreachable!("Configured sources must bypass Rust toolchain directory lookup")
        });

        assert!(system_under_test.is_ok(), "Configured Rust sources were rejected");
        Ok(())
    }
}
