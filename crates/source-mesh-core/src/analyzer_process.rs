use std::collections::BTreeSet;
use std::fmt::Display;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::bail;
use derive_new::new;

use crate::analyzer_options::Options as AnalyzerOptions;

const MAXIMUM_EXAMPLES: usize = 3;
const MAXIMUM_LINE_CHARACTERS: usize = 240;
pub(crate) const VERBOSE_HINT: &str = "Use --verbose for diagnostic details.";
const DEPENDENCY_GRAPH_FAILURE: &str = "Cannot build the dependency graph";
const PROJECT_LOADING_ERRORS: &str = "project loading errors";
const UNSPECIFIED_FAILURE: &str = "The analyzer exited without a specific error message.";
const INDEXING_STARTED: &str = "Generating SCIP start...";
const INDEXING_FINISHED_PREFIX: &str = "Generating SCIP finished ";
const LOADING_PREFIX: &str = "rust-analyzer: Loading ";
const EMPTY_CONFIGURATION_ERRORS: &str = "ERROR Config Error(s) error_sink=ConfigErrors([])";
const DUPLICATE_SYMBOL_PREFIX: &str = "Duplicate symbol: ";
const CONFIGURATION_ERRORS_PREFIX: &str = "ERROR Config Error(s) error_sink=ConfigErrors([";
const MISSING_DEFINITION_PREFIX: &str = "ERROR Bug: definition at ";
const MISSING_DEFINITION_SUFFIX: &str = " should have been in an SCIP document but was not.";
const UNNAMED_DEFINITION_PREFIX: &str = "ERROR Encountered enclosing definition with no name ";
const DUPLICATE_NOTICE_LINES: &[&str] = &[
    "Encountered duplicate scip symbols, indicating an internal rust-analyzer bug. These duplicates are",
    "included in the output, but this causes information lookup to be ambiguous and so information about",
    "these symbols presented by downstream tools may be incorrect.",
    "Known rust-analyzer bugs that can cause this:",
    "* Definitions in crate example binaries which have the same symbol as definitions in the library",
    "or some other example.",
    "* Struct/enum/const/static/impl definitions nested in a function do not mention the function name.",
    "See #18771.",
    "Duplicate symbols encountered:",
];

#[derive(Clone, Copy)]
pub enum Phase {
    Configured,
    Production,
}

impl Phase {
    #[must_use]
    const fn label(self) -> &'static str {
        match self {
            Self::Configured => "configured analysis",
            Self::Production => "analysis without tests",
        }
    }
}

#[derive(new)]
pub struct ScipProcess<'a> {
    project: &'a Path,
    configuration_path: Option<&'a Path>,
    options: &'a AnalyzerOptions,
}

impl ScipProcess<'_> {
    pub fn generate_index(&self, index_file: &Path, phase: Phase) -> Result<Vec<u8>> {
        let mut analyzer_command = Command::new("rust-analyzer");
        analyzer_command
            .current_dir(self.project)
            .args(["scip", ".", "--exclude-vendored-libraries", "--num-threads", "0"])
            .arg("--output")
            .arg(index_file);
        if let Some(configuration_path) = self.configuration_path {
            analyzer_command.arg("--config-path").arg(configuration_path);
        }
        let output = analyzer_command.output().with_context(|| {
            format!(
                "Cannot run rust-analyzer ({}); check the project directory and that rust-analyzer is installed and executable",
                phase.label()
            )
        })?;
        if self.options.verbose() {
            eprintln!(
                "rust-analyzer ({}) stdout:\n{}\nrust-analyzer ({}) stderr:\n{}",
                phase.label(),
                String::from_utf8_lossy(&output.stdout),
                phase.label(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let diagnostics = Diagnostics::from_streams(&output.stdout, &output.stderr);
        if !output.status.success() {
            bail!(diagnostics.failure_message(phase, &output.status, self.options.verbose()));
        }
        if diagnostics.has_blocking_errors() {
            bail!(diagnostics.failure_message(phase, &PROJECT_LOADING_ERRORS, self.options.verbose()));
        }
        fs::read(index_file).with_context(|| {
            format!("{DEPENDENCY_GRAPH_FAILURE}: {} produced no readable analysis data. {VERBOSE_HINT}", phase.label())
        })
    }
}

#[derive(Default)]
struct DiagnosticGroup {
    occurrences: usize,
    examples: BTreeSet<String>,
}

impl DiagnosticGroup {
    fn record(&mut self, message: &str) {
        assert!(!message.is_empty(), "Diagnostic examples must not be empty");
        self.occurrences += 1;
        if self.examples.len() < MAXIMUM_EXAMPLES {
            self.examples.insert(concise_line(message));
        }
    }

    #[must_use]
    fn summary(&self, description: &str) -> Option<String> {
        if self.occurrences == 0 {
            return None;
        }
        let examples = self.examples.iter().map(|example| format!("  {example}")).collect::<Vec<_>>().join("\n");
        Some(format!("{} {description}. Examples:\n{examples}", self.occurrences))
    }
}

#[derive(Default)]
struct Diagnostics {
    blocking_errors: DiagnosticGroup,
    errors: DiagnosticGroup,
    other: DiagnosticGroup,
}

impl Diagnostics {
    #[must_use]
    fn from_streams(stdout: &[u8], stderr: &[u8]) -> Self {
        let mut diagnostics = Self::default();
        for stream in [stdout, stderr] {
            let text = String::from_utf8_lossy(stream);
            let mut lines = text.lines().map(str::trim).peekable();
            while let Some(line) = lines.next() {
                let line = without_timestamp(line);
                if line.is_empty()
                    || line == INDEXING_STARTED
                    || line.starts_with(INDEXING_FINISHED_PREFIX)
                    || (line.starts_with(LOADING_PREFIX) && !contains_failure(line) && !line.split_whitespace().any(|word| word.trim_end_matches(':').eq_ignore_ascii_case("warning") || word.trim_end_matches(':').eq_ignore_ascii_case("warn")))
                    || line == EMPTY_CONFIGURATION_ERRORS
                    || (line.starts_with(MISSING_DEFINITION_PREFIX) && line.ends_with(MISSING_DEFINITION_SUFFIX))
                    || line.starts_with(UNNAMED_DEFINITION_PREFIX)
                    || line.starts_with(DUPLICATE_SYMBOL_PREFIX)
                    || DUPLICATE_NOTICE_LINES.contains(&line)
                    || (line.contains(".rs:") && lines.peek().is_some_and(|next| next.starts_with(DUPLICATE_SYMBOL_PREFIX)))
                {
                    continue;
                }
                if blocks_analysis(line) {
                    diagnostics.blocking_errors.record(line);
                } else if contains_failure(line) {
                    diagnostics.errors.record(line);
                } else {
                    diagnostics.other.record(line);
                }
            }
        }
        diagnostics
    }

    #[must_use]
    const fn has_blocking_errors(&self) -> bool {
        self.blocking_errors.occurrences != 0
    }
    #[must_use]
    fn summary(&self) -> String {
        let mut messages = Vec::new();
        if let Some(blocking) = self.blocking_errors.summary("analysis errors") {
            messages.push(blocking);
        }
        if let Some(errors) = self.errors.summary("error diagnostic lines") {
            messages.push(errors);
        }
        if let Some(other) = self.other.summary("additional diagnostic lines") {
            messages.push(other);
        }
        messages.join("\n")
    }

    #[must_use]
    fn failure_message(&self, phase: Phase, reason: &impl Display, verbose: bool) -> String {
        let failure = format!("{DEPENDENCY_GRAPH_FAILURE}: {} failed ({reason}).", phase.label());
        if !verbose {
            return format!("{failure}\n{VERBOSE_HINT}");
        }
        let summary = self.summary();
        let details = if summary.is_empty() { UNSPECIFIED_FAILURE } else { &summary };
        format!("{failure}\n{details}\n{VERBOSE_HINT}")
    }
}

#[must_use]
fn blocks_analysis(line: &str) -> bool {
    line.starts_with("error:") || line.starts_with("error[")
        || line.strip_prefix(CONFIGURATION_ERRORS_PREFIX)
            .and_then(|errors| errors.strip_suffix("])"))
            .is_some_and(|errors| !errors.trim().is_empty())
}
#[must_use]
fn contains_failure(line: &str) -> bool {
    line.split_whitespace().any(|word| {
        let word = word.trim_matches(|character: char| !character.is_ascii_alphanumeric() && character != '[' && character != ']');
        word.starts_with("error[") || ["error", "failed", "panic", "panicked"].iter().any(|marker| word.eq_ignore_ascii_case(marker))
    })
}
#[must_use]
fn without_timestamp(line: &str) -> &str {
    line.split_once(' ').map_or(line, |(timestamp, message)| {
        if timestamp.starts_with(|character: char| character.is_ascii_digit()) && timestamp.contains('T') {
            message.trim_start()
        } else {
            line
        }
    })
}

#[must_use]
fn concise_line(line: &str) -> String {
    let mut characters = line.chars();
    let mut preview: String = characters.by_ref().take(MAXIMUM_LINE_CHARACTERS).collect();
    if characters.next().is_some() {
        preview.push_str("...");
    }
    preview
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::Diagnostics;
    use super::CONFIGURATION_ERRORS_PREFIX;
    use super::DEPENDENCY_GRAPH_FAILURE;
    use super::DUPLICATE_NOTICE_LINES;
    use super::DUPLICATE_SYMBOL_PREFIX;
    use super::EMPTY_CONFIGURATION_ERRORS;
    use super::INDEXING_FINISHED_PREFIX;
    use super::INDEXING_STARTED;
    use super::LOADING_PREFIX;
    use super::MAXIMUM_EXAMPLES;
    use super::MAXIMUM_LINE_CHARACTERS;
    use super::MISSING_DEFINITION_PREFIX;
    use super::MISSING_DEFINITION_SUFFIX;
    use super::Phase;
    use super::PROJECT_LOADING_ERRORS;
    use super::UNNAMED_DEFINITION_PREFIX;
    use super::UNSPECIFIED_FAILURE;
    use super::VERBOSE_HINT;
    use super::concise_line;

    #[must_use]
    fn internal_diagnostics() -> String {
        format!("{MISSING_DEFINITION_PREFIX}source.rs:1:0-1:3{MISSING_DEFINITION_SUFFIX}\n{UNNAMED_DEFINITION_PREFIX}def=Module\n")
    }
    #[test]
    fn test_progress_and_empty_configuration_diagnostics() {
        let fixture = format!("{INDEXING_STARTED}\n{LOADING_PREFIX}cargo metadata: ?\n2026-09-26T00:06:16+06:00 {EMPTY_CONFIGURATION_ERRORS}\n{INDEXING_FINISHED_PREFIX}12s\n");
        let system_under_test = Diagnostics::from_streams(&[], fixture.as_bytes());

        assert_eq!(system_under_test.summary(), "");
        assert!(!system_under_test.has_blocking_errors());
    }

    #[rstest]
    #[case::cargo("error: failed to run custom build command")]
    #[case::compiler("error[E0432]: unresolved import")]
    #[case::configuration(&format!("{CONFIGURATION_ERRORS_PREFIX}invalid field])"))]
    fn test_actionable_errors_block_successful_analysis(#[case] diagnostic: &str) {
        let system_under_test = Diagnostics::from_streams(&[], diagnostic.as_bytes());

        assert!(system_under_test.has_blocking_errors());
        assert!(system_under_test.summary().contains(diagnostic));
    }

    #[rstest]
    #[case::warning("warning: optional service unavailable")]
    #[case::internal_error("ERROR internal metric failed to initialize")]
    #[case::unknown("New analyzer diagnostic format")]
    #[case::empty_configuration(EMPTY_CONFIGURATION_ERRORS)]
    fn test_nonblocking_diagnostics_allow_successful_analysis(#[case] diagnostic: &str) {
        let system_under_test = Diagnostics::from_streams(&[], diagnostic.as_bytes());

        assert!(!system_under_test.has_blocking_errors());
    }
    #[rstest]
    #[case::metadata_error(&format!("{LOADING_PREFIX}cargo metadata: error: invalid workspace"))]
    #[case::build_failure(&format!("{LOADING_PREFIX}build script engine failed"))]
    #[case::loading_warning(&format!("{LOADING_PREFIX}cargo metadata: warning: incomplete metadata"))]
    fn test_loading_diagnostics_remain_visible(#[case] diagnostic: &str) {
        let system_under_test = Diagnostics::from_streams(&[], diagnostic.as_bytes());

        assert!(system_under_test.summary().contains(diagnostic));
    }

    #[rstest]
    #[case::thiserror("thiserror")]
    #[case::error_stack("error-stack")]
    #[case::failure("failure")]
    fn test_error_crate_names_are_progress(#[case] crate_name: &str) {
        let fixture = format!("{LOADING_PREFIX}build script {crate_name} run\n");
        let system_under_test = Diagnostics::from_streams(&[], fixture.as_bytes());

        assert_eq!(system_under_test.summary(), "");
        assert!(!system_under_test.has_blocking_errors());
    }
    #[test]
    fn test_duplicate_notices_leave_unknown_diagnostics_visible() {
        let fixture = format!("{}\nsource.rs:1:0-1:3\n  {DUPLICATE_SYMBOL_PREFIX}example main().\nwarning: failed to load build script\n", DUPLICATE_NOTICE_LINES.join("\n"));
        let system_under_test = Diagnostics::from_streams(&[], fixture.as_bytes());

        assert_eq!(system_under_test.errors.occurrences, 1);
        assert!(system_under_test.summary().contains("failed to load build script"));
        assert!(!system_under_test.summary().contains(DUPLICATE_SYMBOL_PREFIX));
    }

    #[test]
    fn test_both_output_streams_contribute_diagnostics() {
        let system_under_test = Diagnostics::from_streams(b"warning: stdout failure\n", b"ERROR stderr failure\n");

        assert_eq!(system_under_test.other.occurrences + system_under_test.errors.occurrences, 2);
        assert!(system_under_test.summary().contains("stdout failure"));
        assert!(system_under_test.summary().contains("stderr failure"));
    }

    #[test]
    fn test_configuration_errors_remain_visible() {
        let fixture = format!("2026-09-26T00:06:16+06:00 {CONFIGURATION_ERRORS_PREFIX}invalid field])\n");
        let system_under_test = Diagnostics::from_streams(&[], fixture.as_bytes());

        assert!(system_under_test.summary().contains("invalid field"));
    }

    #[test]
    fn test_repeated_internal_diagnostics_stay_quiet() {
        let fixture = internal_diagnostics().repeat(100);
        let system_under_test = Diagnostics::from_streams(&[], fixture.as_bytes());

        assert_eq!(system_under_test.summary(), "");
        assert!(!system_under_test.has_blocking_errors());
    }

    #[test]
    fn test_internal_diagnostics_preserve_unknown_and_build_problems() {
        let fixture = format!("{}warning: custom diagnostic\nerror: failed to read Cargo.toml\n", internal_diagnostics());
        let system_under_test = Diagnostics::from_streams(&[], fixture.as_bytes());

        let summary = system_under_test.summary();

        assert!(summary.contains("custom diagnostic"));
        assert!(summary.contains("failed to read Cargo.toml"));
        assert!(!summary.contains(MISSING_DEFINITION_PREFIX));
        assert!(!summary.contains(UNNAMED_DEFINITION_PREFIX));
    }
    #[rstest]
    #[case::normal(false)]
    #[case::verbose(true)]
    fn test_internal_diagnostics_do_not_hide_process_failure(#[case] verbose: bool) {
        let fixture = internal_diagnostics();
        let system_under_test = Diagnostics::from_streams(&[], fixture.as_bytes());

        let message = system_under_test.failure_message(Phase::Configured, &"exit code 1", verbose);

        assert!(message.contains(DEPENDENCY_GRAPH_FAILURE));
        assert!(message.contains("exit code 1"));
        assert!(message.contains(VERBOSE_HINT));
        assert!(!message.contains(MISSING_DEFINITION_PREFIX));
        assert!(!message.contains(UNNAMED_DEFINITION_PREFIX));
    }
    #[rstest]
    #[case::configured(Phase::Configured)]
    #[case::production(Phase::Production)]
    fn test_process_failure_without_actionable_diagnostics(#[case] phase: Phase, #[values(false, true)] verbose: bool) {
        let system_under_test = Diagnostics::from_streams(&[], INDEXING_STARTED.as_bytes());

        let message = system_under_test.failure_message(phase, &"exit code 1", verbose);

        assert!(message.contains(phase.label()));
        assert!(message.contains("exit code 1"));
        assert_eq!(message.contains(UNSPECIFIED_FAILURE), verbose);
        assert!(message.contains(VERBOSE_HINT));
    }

    #[rstest]
    #[case::normal(false)]
    #[case::verbose(true)]
    fn test_process_failure_diagnostic_visibility(
        #[case] verbose: bool,
        #[values("error: build script failed", "ERROR internal analyzer failure", "warning: analyzer warning")] diagnostic: &str,
        #[values("exit code 1", PROJECT_LOADING_ERRORS)] reason: &str,
    ) {
        let system_under_test = Diagnostics::from_streams(&[], diagnostic.as_bytes());

        let message = system_under_test.failure_message(Phase::Configured, &reason, verbose);

        assert_eq!(message.contains(diagnostic), verbose);
        assert!(message.contains(DEPENDENCY_GRAPH_FAILURE));
        assert!(message.contains(Phase::Configured.label()));
        assert!(message.contains(reason));
        assert!(message.contains(VERBOSE_HINT));
        assert!(system_under_test.summary().contains(diagnostic));
    }

    #[test]
    fn test_diagnostic_examples_are_bounded() {
        let fixture = "warning: first\nwarning: second\nwarning: third\nwarning: fourth\nwarning: fifth\n".repeat(20);
        let system_under_test = Diagnostics::from_streams(&[], fixture.as_bytes());

        assert_eq!(system_under_test.other.occurrences, 100);
        assert_eq!(system_under_test.other.examples.len(), MAXIMUM_EXAMPLES);
        assert_eq!(system_under_test.summary().lines().count(), MAXIMUM_EXAMPLES + 1);
    }

    #[test]
    fn test_repeated_unknown_diagnostics_share_one_example() {
        let fixture = "warning: optional service unavailable\n".repeat(12);
        let system_under_test = Diagnostics::from_streams(&[], fixture.as_bytes());

        assert_eq!(system_under_test.other.occurrences, 12);
        assert_eq!(system_under_test.other.examples.len(), 1);
        assert_eq!(system_under_test.summary().matches("optional service unavailable").count(), 1);
    }

    #[test]
    fn test_build_errors_remain_visible_after_many_warnings() {
        let fixture = format!("{}error[E0432]: unresolved import\n", "warning: optional service unavailable\n".repeat(100));
        let system_under_test = Diagnostics::from_streams(&[], fixture.as_bytes());

        assert_eq!(system_under_test.blocking_errors.occurrences, 1);
        assert!(system_under_test.summary().contains("error[E0432]: unresolved import"));
    }
    #[test]
    fn test_long_diagnostics_preserve_unicode_boundaries() {
        let fixture = "\u{1f980}".repeat(MAXIMUM_LINE_CHARACTERS + 1);

        let system_under_test = concise_line(&fixture);

        assert_eq!(system_under_test, format!("{}...", "\u{1f980}".repeat(MAXIMUM_LINE_CHARACTERS)));
    }
}
