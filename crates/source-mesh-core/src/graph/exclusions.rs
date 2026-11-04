use std::path::Path;
use std::path::PathBuf;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use globset::GlobBuilder;
use globset::GlobSet;
use globset::GlobSetBuilder;


#[derive(Debug, Default)]
pub struct Exclusions {
    patterns: GlobSet,
}

impl Exclusions {
    pub fn new(patterns: &[String]) -> Result<Self> {
        let mut compiled = GlobSetBuilder::new();
        for pattern in patterns {
            ensure!(!pattern.is_empty(), "Invalid exclusion pattern {pattern:?}: patterns must not be empty");
            ensure!(!pattern.starts_with('!'), "Invalid exclusion pattern {pattern:?}: negation is not supported");
            let nonportable = pattern.chars().any(|character| {
                character.is_control() || matches!(character, '\\' | ':' | '<' | '>' | '"' | '|')
            });
            ensure!(!nonportable, "Invalid exclusion pattern {pattern:?}: use portable relative paths with '/' separators");
            ensure!(
                pattern.split('/').all(|component| !matches!(component, "" | "." | "..")),
                "Invalid exclusion pattern {pattern:?}: paths must be relative with nonempty components and no '.' or '..'; \
                 use 'directory/**' for directories"
            );
            let glob = GlobBuilder::new(pattern).literal_separator(true).backslash_escape(false).case_insensitive(false).build();
            compiled.add(glob.with_context(|| format!("Invalid exclusion pattern {pattern:?}"))?);
        }
        Ok(Self { patterns: compiled.build().context("Could not compile file exclusion patterns")? })
    }

    #[must_use]
    pub(super) fn matches(&self, path: &Path) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let path: PathBuf = path.components().collect();
        self.patterns.is_match(path)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use claims::assert_err;
    use claims::assert_ok;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::Exclusions;
    use crate::SourcePath;

    #[must_use]
    #[track_caller]
    fn validated_source_path_test_fixture(path: PathBuf) -> PathBuf {
        let source_path = SourcePath::new(path);
        let source_path = assert_ok!(source_path, "Exclusion test fixture must have a valid relative source path");
        source_path.into_path()
    }

    #[must_use]
    #[track_caller]
    fn portable_source_path_test_fixture(path: &str) -> PathBuf {
        validated_source_path_test_fixture(path.split('/').collect())
    }

    #[rstest]
    #[case("src/file.rs", "src/file.rs", true)]
    #[case("src/file.rs", "src/file.rs.bak", false)]
    #[case("src/file.rs", "nested/src/file.rs", false)]
    #[case("src/file.rs", "src/FILE.rs", false)]
    #[case("src", "src", true)]
    #[case("src", "src/file.rs", false)]
    #[case("*.rs", "file.rs", true)]
    #[case("*.rs", ".hidden.rs", true)]
    #[case("*.rs", "src/file.rs", false)]
    #[case("src/*.rs", "src/file.rs", true)]
    #[case("src/*.rs", "src/nested/file.rs", false)]
    #[case("src/*.rs", "src/file.RS", false)]
    #[case("src/*/file.rs", "src/nested/file.rs", true)]
    #[case("src/*/file.rs", "src/file.rs", false)]
    #[case("src/*/file.rs", "src/nested/deeper/file.rs", false)]
    #[case("**/*.rs", "file.rs", true)]
    #[case("**/*.rs", "src/nested/file.rs", true)]
    #[case("src/**/file.rs", "src/file.rs", true)]
    #[case("src/**/file.rs", "src/nested/deeper/file.rs", true)]
    #[case("src/generated/**", "src/generated/file.rs", true)]
    #[case("src/generated/**", "src/generated/nested/file.rs", true)]
    #[case("src/generated/**", "src/generated", false)]
    #[case("src/generated/**", "src/generated_other/file.rs", false)]
    #[case("src/generated/**", "nested/src/generated/file.rs", false)]
    #[case("SRC/**", "src/file.rs", false)]
    #[case("src/?.rs", "src/a.rs", true)]
    #[case("src/?.rs", "src/ab.rs", false)]
    #[case("src/[ab].rs", "src/a.rs", true)]
    #[case("src/{first,second}.rs", "src/second.rs", true)]
    #[case("src/some name.rs", "src/some name.rs", true)]
    #[case("src/\u{e9}*.rs", "src/\u{e9}tude.rs", true)]
    #[case("src/..hidden.rs", "src/..hidden.rs", true)]
    fn test_glob_semantics(#[case] pattern: &str, #[case] path: &str, #[case] expected: bool) {
        let file = portable_source_path_test_fixture(path);
        let system_under_test = Exclusions::new(&[pattern.to_owned()]);
        let system_under_test = assert_ok!(system_under_test, "Portable exclusion glob must compile: {pattern:?}");

        let matched = system_under_test.matches(&file);

        assert_eq!(expected, matched, "Pattern {pattern:?} matched path {path:?} incorrectly");
    }

    #[rstest]
    #[case("src/generated/file.rs", true)]
    #[case("src/nested/file.generated.rs", true)]
    #[case("root.rs", true)]
    #[case("src/root.rs", false)]
    #[case("src/main.rs", false)]
    fn test_combined_patterns(#[case] path: &str, #[case] expected: bool) {
        let file = portable_source_path_test_fixture(path);
        let patterns = ["src/generated/**", "**/*.generated.rs", "root.rs", "src/generated/**"].map(str::to_owned);
        let system_under_test = Exclusions::new(&patterns);
        let system_under_test = assert_ok!(system_under_test, "Overlapping and duplicate exclusion patterns must compile");

        let matched = system_under_test.matches(&file);
        let matched_again = system_under_test.matches(&file);

        assert_eq!(expected, matched, "Combined patterns matched path {path:?} incorrectly");
        assert_eq!(matched, matched_again, "Compiled exclusions must be reusable");
    }

    #[rstest]
    fn test_empty_exclusions(#[values(false, true)] compiled: bool) {
        let file = portable_source_path_test_fixture("src/file.rs");
        let system_under_test = if compiled {
            let exclusions = Exclusions::new(&[]);
            assert_ok!(exclusions, "An empty pattern list must compile")
        } else {
            Exclusions::default()
        };

        let matched = system_under_test.matches(&file);

        assert!(!matched, "Empty exclusions must not match any file");
    }

    #[rstest]
    #[case("")]
    #[case("/")]
    #[case("/src/**")]
    #[case("//server/share/**")]
    #[case(".")]
    #[case("./src/**")]
    #[case("src/./file.rs")]
    #[case("..")]
    #[case("../**")]
    #[case("src/../**")]
    #[case("src/..")]
    #[case("src//*.rs")]
    #[case("src/")]
    #[case("src\\**")]
    #[case("C:/src/**")]
    #[case("C:src/**")]
    #[case("\\src\\**")]
    #[case("\\\\server\\share\\**")]
    #[case("\\\\?\\C:\\src\\**")]
    #[case("src/file:stream")]
    #[case("src/<file>.rs")]
    #[case("src/\"file\".rs")]
    #[case("src/file|other.rs")]
    #[case("src/file\0.rs")]
    #[case("src/file\n.rs")]
    #[case("!src/**")]
    #[case("[")]
    #[case("src/[z-a].rs")]
    #[case("{src,tests")]
    fn test_pattern_validation(#[case] pattern: &str) {
        let patterns = ["valid/**".to_owned(), pattern.to_owned()];

        let system_under_test = Exclusions::new(&patterns);

        let error = assert_err!(system_under_test, "Invalid exclusion pattern was accepted: {pattern:?}");
        let diagnostic = error.to_string();
        let expected = format!("{pattern:?}");
        assert!(diagnostic.contains(&expected), "Validation must identify the invalid pattern: {diagnostic}");
    }

    #[rstest]
    #[case("src/file.rs")]
    #[case("src//file.rs")]
    #[case("src/./file.rs")]
    #[case("src//file.rs/")]
    fn test_validated_source_path_spelling(#[case] path: &str) {
        let expected_path = PathBuf::from(path);
        let file = validated_source_path_test_fixture(expected_path.clone());
        let system_under_test = Exclusions::new(&["src/file.rs".to_owned()]);
        let system_under_test = assert_ok!(system_under_test, "An exact project-relative file exclusion must compile");

        let matched = system_under_test.matches(&file);

        assert!(matched, "Equivalent validated source path spelling did not match: {path:?}");
        assert_eq!(expected_path.as_os_str(), file.as_os_str(), "Matching must not change stored path spelling");
    }

    #[cfg(unix)]
    #[rstest]
    #[case("src/*.rs", true)]
    #[case("src/f\u{fffd}.rs", false)]
    fn test_non_utf8_source_paths(#[case] pattern: &str, #[case] expected: bool) {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;
        use std::path::Path;

        let name = OsString::from_vec(vec![b'f', 0xff, b'.', b'r', b's']);
        let file = validated_source_path_test_fixture(Path::new("src").join(name));
        let system_under_test = Exclusions::new(&[pattern.to_owned()]);
        let system_under_test = assert_ok!(system_under_test, "Non-UTF-8 source paths must not affect glob compilation");

        let matched = system_under_test.matches(&file);

        assert_eq!(expected, matched, "Matching must preserve non-UTF-8 source path bytes");
    }
}
