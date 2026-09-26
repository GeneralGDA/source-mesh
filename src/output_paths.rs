use std::path::Path;
use std::path::PathBuf;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use same_file::Handle;

struct PathIdentity {
    absolute: PathBuf,
    handle: Option<Handle>,
}

impl PathIdentity {
    fn resolve(path: &Path) -> Result<Self> {
        let name = path.file_name().context("Path must name a file")?;
        let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = dunce::canonicalize(parent)
            .with_context(|| format!("Cannot resolve parent directory of {}", path.display()))?;
        ensure!(parent.is_dir(), "Parent is not a directory: {}", parent.display());
        let absolute = parent.join(name);
        let handle = if absolute.try_exists()? {
            ensure!(!absolute.is_dir(), "Expected a file path: {}", path.display());
            Some(Handle::from_path(&absolute)
                .with_context(|| format!("Cannot identify file {}", path.display()))?)
        } else {
            None
        };
        Ok(Self { absolute, handle })
    }

    #[must_use]
    fn same_file(&self, other: &Self) -> bool {
        same_path_spelling(&self.absolute, &other.absolute)
            || matches!((&self.handle, &other.handle), (Some(left), Some(right)) if left == right)
    }
}

pub(crate) fn validate_output_paths(outputs: &[&Path], inputs: &[&Path]) -> Result<()> {
    let outputs = outputs.iter().map(|path| PathIdentity::resolve(path)).collect::<Result<Vec<_>>>()?;
    let inputs = inputs.iter().map(|path| PathIdentity::resolve(path)).collect::<Result<Vec<_>>>()?;
    for (index, output) in outputs.iter().enumerate() {
        for other in outputs.iter().skip(index + 1).chain(&inputs) {
            ensure!(!output.same_file(other),
                "Output files must use distinct paths and must not overwrite input files: {} and {}",
                output.absolute.display(), other.absolute.display());
        }
    }
    Ok(())
}

#[cfg(windows)]
#[must_use]
fn same_path_spelling(left: &Path, right: &Path) -> bool {
    left.as_os_str().eq_ignore_ascii_case(right.as_os_str())
}

#[cfg(not(windows))]
#[must_use]
fn same_path_spelling(left: &Path, right: &Path) -> bool {
    left == right
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Tests use assertions for expectations while returning Result for fallible setup."
)]
mod tests {
    use std::fs;
    use std::path::Path;

    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use tempfile::tempdir;

    use super::validate_output_paths;

    #[rstest]
    fn test_output_aliases(#[values(false, true)] hard_link: bool) -> Result<()> {
        const EXISTING_CONTENT: &str = "original";
        let fixture = tempdir()?;
        let first = fixture.path().join("cycles.md");
        let second = if hard_link {
            fixture.path().join("alias.md")
        } else {
            fixture.path().join(".").join("cycles.md")
        };
        fs::write(&first, EXISTING_CONTENT)?;
        if hard_link { fs::hard_link(&first, &second)?; }

        let system_under_test = validate_output_paths(&[&first, &second], &[]);
        let preserved = fs::read_to_string(first)?;

        assert!(system_under_test.is_err(), "Aliased outputs were accepted");
        assert_eq!(preserved, EXISTING_CONTENT, "Output validation changed the existing file");
        Ok(())
    }

    #[test]
    fn test_input_overwrite() -> Result<()> {
        let fixture = tempdir()?;
        let input = fixture.path().join("index.scip");
        let output = fixture.path().join("report.md");
        fs::write(&input, "input")?;
        fs::hard_link(&input, &output)?;

        let system_under_test = validate_output_paths(&[&output], &[&input]);

        assert!(system_under_test.is_err(), "Output can overwrite the input index through an alias");
        Ok(())
    }

    #[rstest]
    #[case("report.md", "REPORT.md", cfg!(windows))]
    #[case("folders.md", "files.md", false)]
    fn test_new_output_paths(
        #[case] first_name: &str,
        #[case] second_name: &str,
        #[case] conflict: bool,
    ) -> Result<()> {
        let fixture = tempdir()?;
        let first = fixture.path().join(first_name);
        let second = fixture.path().join(second_name);

        let system_under_test = validate_output_paths(&[&first, &second], &[]);

        assert_eq!(system_under_test.is_err(), conflict, "New output paths have incorrect identity");
        assert!(!first.exists() && !second.exists(), "Validation created output files");
        Ok(())
    }

    #[test]
    fn test_directory_output() -> Result<()> {
        let fixture = tempdir()?;

        let system_under_test = validate_output_paths(&[fixture.path()], &[]);

        assert!(system_under_test.is_err(), "A directory was accepted as an output file");
        Ok(())
    }

    #[test]
    fn test_empty_output_path() {
        let system_under_test = validate_output_paths(&[Path::new("")], &[]);

        assert!(system_under_test.is_err(), "An empty output path was accepted");
    }
}
