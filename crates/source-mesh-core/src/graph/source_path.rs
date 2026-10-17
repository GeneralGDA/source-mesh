use std::path::Component;
use std::path::PathBuf;

use anyhow::Result;
use anyhow::ensure;

/**
Validation is lexical only, not a guarantee of existence, file type, or accessibility.
The supplied path spelling is preserved.
*/
pub struct SourcePath {
    path: PathBuf,
}

impl SourcePath {
    pub fn new(path: PathBuf) -> Result<Self> {
        ensure!(
            !path.as_os_str().is_empty() && path.components().all(|component| matches!(component, Component::Normal(_))),
            "Source path must be nonempty and relative, with only normal path components: {}",
            path.display()
        );
        Ok(Self { path })
    }

    #[must_use]
    pub(crate) fn into_path(self) -> PathBuf {
        self.path
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::path::PathBuf;

    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::SourcePath;

    #[rstest]
    #[case("")]
    #[case(".")]
    #[case("..")]
    #[case("../outside.rs")]
    #[case("./source.rs")]
    #[case("src/../source.rs")]
    #[case("src/..")]
    #[case("/absolute.rs")]
    #[case("/")]
    fn test_source_path_preconditions(#[case] source_path: &str) {
        let path = PathBuf::from(source_path);

        let system_under_test = SourcePath::new(path);

        assert!(system_under_test.is_err(), "Invalid source path was accepted: {source_path}");
    }

    #[cfg(windows)]
    #[rstest]
    #[case("C:source.rs")]
    #[case("C:\\source.rs")]
    #[case("\\source.rs")]
    #[case("\\\\server\\share\\source.rs")]
    #[case("\\\\?\\C:\\source.rs")]
    fn test_windows_source_path_prefixes(#[case] source_path: &str) {
        let path = PathBuf::from(source_path);

        let system_under_test = SourcePath::new(path);

        assert!(system_under_test.is_err(), "Prefixed or rooted source path was accepted: {source_path}");
    }

    #[rstest]
    #[case("source.rs")]
    #[case("src/module.rs")]
    #[case("src/")]
    #[case("src/./source.rs")]
    #[case("src//source.rs/")]
    #[case("file\0.rs")]
    #[case("file:name.rs")]
    #[case("CON")]
    #[case("file?.rs")]
    #[case("file*.rs")]
    #[case("file. ")]
    #[case("..hidden.rs")]
    fn test_source_path_spelling(#[case] source_path: &str) -> Result<()> {
        let path = PathBuf::from(source_path);

        let system_under_test = SourcePath::new(path)?;

        assert_eq!(
            Path::new(source_path).as_os_str(),
            system_under_test.path.as_os_str(),
            "Lexical validation changed the supplied path spelling"
        );
        Ok(())
    }
}
