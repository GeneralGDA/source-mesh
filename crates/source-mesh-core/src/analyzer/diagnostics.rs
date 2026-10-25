use std::collections::BTreeSet;
use std::fmt::Display;

use crate::diagnostics::MESSAGE_VERBOSE_DIAGNOSTICS_HINT;

pub(crate) const MESSAGE_DEPENDENCY_GRAPH_FAILURE: &str = "Cannot build the dependency graph";
pub(crate) const MESSAGE_PROJECT_LOADING_FAILURE_REASON: &str = "project loading errors";
const MESSAGE_EMPTY_DIAGNOSTIC_SUMMARY: &str = "The analyzer exited without a specific error message.";
const MAXIMUM_DIAGNOSTIC_EXAMPLES_PER_GROUP: usize = 3;
const MAXIMUM_DIAGNOSTIC_EXAMPLE_CHARACTERS: usize = 240;
const MESSAGE_RUST_ANALYZER_SCIP_STARTED: &str = "Generating SCIP start...";
const MESSAGE_PREFIX_RUST_ANALYZER_SCIP_FINISHED: &str = "Generating SCIP finished ";
const MESSAGE_PREFIX_RUST_ANALYZER_LOADING: &str = "rust-analyzer: Loading ";
const MESSAGE_RUST_ANALYZER_EMPTY_CONFIGURATION_ERRORS: &str = "ERROR Config Error(s) error_sink=ConfigErrors([])";
const MESSAGE_PREFIX_RUST_ANALYZER_DUPLICATE_SYMBOL: &str = "Duplicate symbol: ";
const MESSAGE_PREFIX_RUST_ANALYZER_CONFIGURATION_ERRORS: &str = "ERROR Config Error(s) error_sink=ConfigErrors([";
const MESSAGE_PREFIX_RUST_ANALYZER_MISSING_SCIP_DEFINITION: &str = "ERROR Bug: definition at ";
const MESSAGE_SUFFIX_RUST_ANALYZER_MISSING_SCIP_DEFINITION: &str = " should have been in an SCIP document but was not.";
const MESSAGE_PREFIX_RUST_ANALYZER_UNNAMED_DEFINITION: &str = "ERROR Encountered enclosing definition with no name ";
const MESSAGE_RUST_ANALYZER_DUPLICATE_SYMBOL_NOTICE_LINES: &[&str] = &[
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

#[derive(Default)]
struct DiagnosticGroup {
    occurrences: usize,
    examples: BTreeSet<String>,
}

impl DiagnosticGroup {
    fn record(&mut self, message: &str) {
        assert!(!message.is_empty(), "Diagnostic examples must not be empty");
        self.occurrences += 1;
        if self.examples.len() < MAXIMUM_DIAGNOSTIC_EXAMPLES_PER_GROUP {
            self.examples.insert(diagnostic_line_preview(message));
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
pub(crate) struct ScipDiagnostics {
    blocking_errors: DiagnosticGroup,
    nonblocking_errors: DiagnosticGroup,
    other_messages: DiagnosticGroup,
}

impl ScipDiagnostics {
    #[must_use]
    pub(crate) fn from_streams(stdout: &[u8], stderr: &[u8]) -> Self {
        let mut diagnostics = Self::default();
        for stream in [stdout, stderr] {
            let text = String::from_utf8_lossy(stream);
            let mut lines = text.lines().map(str::trim).peekable();
            while let Some(line) = lines.next() {
                let line = without_timestamp(line);
                if line.is_empty()
                    || line == MESSAGE_RUST_ANALYZER_SCIP_STARTED
                    || line.starts_with(MESSAGE_PREFIX_RUST_ANALYZER_SCIP_FINISHED)
                    || (line.starts_with(MESSAGE_PREFIX_RUST_ANALYZER_LOADING)
                        && !contains_failure(line)
                        && !line.split_whitespace().any(|word| {
                            let word = word.trim_end_matches(':');
                            word.eq_ignore_ascii_case("warning") || word.eq_ignore_ascii_case("warn")
                        }))
                    || line == MESSAGE_RUST_ANALYZER_EMPTY_CONFIGURATION_ERRORS
                    || (line.starts_with(MESSAGE_PREFIX_RUST_ANALYZER_MISSING_SCIP_DEFINITION)
                        && line.ends_with(MESSAGE_SUFFIX_RUST_ANALYZER_MISSING_SCIP_DEFINITION))
                    || line.starts_with(MESSAGE_PREFIX_RUST_ANALYZER_UNNAMED_DEFINITION)
                    || line.starts_with(MESSAGE_PREFIX_RUST_ANALYZER_DUPLICATE_SYMBOL)
                    || MESSAGE_RUST_ANALYZER_DUPLICATE_SYMBOL_NOTICE_LINES.contains(&line)
                    || (line.contains(".rs:")
                        && lines.peek().is_some_and(|next| next.starts_with(MESSAGE_PREFIX_RUST_ANALYZER_DUPLICATE_SYMBOL)))
                {
                    continue;
                }
                if blocks_analysis(line) {
                    diagnostics.blocking_errors.record(line);
                } else if contains_failure(line) {
                    diagnostics.nonblocking_errors.record(line);
                } else {
                    diagnostics.other_messages.record(line);
                }
            }
        }
        diagnostics
    }

    #[must_use]
    pub(crate) const fn has_blocking_errors(&self) -> bool {
        self.blocking_errors.occurrences != 0
    }
    #[must_use]
    fn summary(&self) -> String {
        let mut messages = Vec::new();
        if let Some(blocking) = self.blocking_errors.summary("analysis errors") {
            messages.push(blocking);
        }
        if let Some(errors) = self.nonblocking_errors.summary("error diagnostic lines") {
            messages.push(errors);
        }
        if let Some(other) = self.other_messages.summary("additional diagnostic lines") {
            messages.push(other);
        }
        messages.join("\n")
    }

    #[must_use]
    pub(crate) fn failure_message(&self, analysis_label: &str, reason: &impl Display, verbose: bool) -> String {
        let failure = format!("{MESSAGE_DEPENDENCY_GRAPH_FAILURE}: {analysis_label} failed ({reason}).");
        if !verbose {
            return format!("{failure}\n{MESSAGE_VERBOSE_DIAGNOSTICS_HINT}");
        }
        let summary = self.summary();
        let details = if summary.is_empty() { MESSAGE_EMPTY_DIAGNOSTIC_SUMMARY } else { &summary };
        format!("{failure}\n{details}\n{MESSAGE_VERBOSE_DIAGNOSTICS_HINT}")
    }
}

#[must_use]
fn blocks_analysis(line: &str) -> bool {
    line.starts_with("error:") || line.starts_with("error[") || {
        let errors = line.strip_prefix(MESSAGE_PREFIX_RUST_ANALYZER_CONFIGURATION_ERRORS);
        let errors = errors.and_then(|errors| errors.strip_suffix("])"));
        errors.is_some_and(|errors| !errors.trim().is_empty())
    }
}
#[must_use]
fn contains_failure(line: &str) -> bool {
    line.split_whitespace().any(|word| {
        let word = word.trim_matches(|character: char| {
            !character.is_ascii_alphanumeric() && character != '[' && character != ']'
        });
        word.starts_with("error[") || {
            let markers = ["error", "failed", "panic", "panicked"];
            markers.iter().any(|marker| word.eq_ignore_ascii_case(marker))
        }
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
fn diagnostic_line_preview(line: &str) -> String {
    let mut characters = line.chars();
    let mut preview: String = characters.by_ref().take(MAXIMUM_DIAGNOSTIC_EXAMPLE_CHARACTERS).collect();
    if characters.next().is_some() {
        preview.push_str("...");
    }
    preview
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::MAXIMUM_DIAGNOSTIC_EXAMPLES_PER_GROUP;
    use super::MAXIMUM_DIAGNOSTIC_EXAMPLE_CHARACTERS;
    use super::MESSAGE_DEPENDENCY_GRAPH_FAILURE;
    use super::MESSAGE_EMPTY_DIAGNOSTIC_SUMMARY;
    use super::MESSAGE_PROJECT_LOADING_FAILURE_REASON;
    use super::MESSAGE_PREFIX_RUST_ANALYZER_CONFIGURATION_ERRORS;
    use super::MESSAGE_RUST_ANALYZER_DUPLICATE_SYMBOL_NOTICE_LINES;
    use super::MESSAGE_PREFIX_RUST_ANALYZER_DUPLICATE_SYMBOL;
    use super::MESSAGE_RUST_ANALYZER_EMPTY_CONFIGURATION_ERRORS;
    use super::MESSAGE_PREFIX_RUST_ANALYZER_LOADING;
    use super::MESSAGE_PREFIX_RUST_ANALYZER_MISSING_SCIP_DEFINITION;
    use super::MESSAGE_SUFFIX_RUST_ANALYZER_MISSING_SCIP_DEFINITION;
    use super::MESSAGE_PREFIX_RUST_ANALYZER_SCIP_FINISHED;
    use super::MESSAGE_RUST_ANALYZER_SCIP_STARTED;
    use super::MESSAGE_PREFIX_RUST_ANALYZER_UNNAMED_DEFINITION;
    use super::ScipDiagnostics;
    use super::diagnostic_line_preview;
    use super::without_timestamp;
    use crate::diagnostics::MESSAGE_VERBOSE_DIAGNOSTICS_HINT;

    #[must_use]
    fn internal_diagnostics() -> String {
        format!(
            "{MESSAGE_PREFIX_RUST_ANALYZER_MISSING_SCIP_DEFINITION}source.rs:1:0-1:3\
             {MESSAGE_SUFFIX_RUST_ANALYZER_MISSING_SCIP_DEFINITION}\n\
             {MESSAGE_PREFIX_RUST_ANALYZER_UNNAMED_DEFINITION}def=Module\n"
        )
    }
    #[test]
    fn test_progress_and_empty_configuration_diagnostics() {
        let fixture = format!(
            "{MESSAGE_RUST_ANALYZER_SCIP_STARTED}\n{MESSAGE_PREFIX_RUST_ANALYZER_LOADING}cargo metadata: ?\n\
             2026-09-26T00:06:16+06:00 {MESSAGE_RUST_ANALYZER_EMPTY_CONFIGURATION_ERRORS}\n\
             {MESSAGE_PREFIX_RUST_ANALYZER_SCIP_FINISHED}12s\n"
        );
        let system_under_test = ScipDiagnostics::from_streams(&[], fixture.as_bytes());

        assert_eq!(system_under_test.summary(), "");
        assert!(!system_under_test.has_blocking_errors());
    }

    #[rstest]
    #[case::cargo("error: failed to run custom build command")]
    #[case::compiler("error[E0432]: unresolved import")]
    #[case::configuration(&format!("{MESSAGE_PREFIX_RUST_ANALYZER_CONFIGURATION_ERRORS}invalid field])"))]
    fn test_actionable_errors_block_successful_analysis(#[case] diagnostic: &str) {
        let system_under_test = ScipDiagnostics::from_streams(&[], diagnostic.as_bytes());

        assert!(system_under_test.has_blocking_errors());
        assert!(system_under_test.summary().contains(diagnostic));
    }

    #[rstest]
    #[case::warning("warning: optional service unavailable")]
    #[case::internal_error("ERROR internal metric failed to initialize")]
    #[case::unknown("New analyzer diagnostic format")]
    #[case::empty_configuration(MESSAGE_RUST_ANALYZER_EMPTY_CONFIGURATION_ERRORS)]
    fn test_nonblocking_diagnostics_allow_successful_analysis(#[case] diagnostic: &str) {
        let system_under_test = ScipDiagnostics::from_streams(&[], diagnostic.as_bytes());

        assert!(!system_under_test.has_blocking_errors());
    }
    #[rstest]
    #[case::metadata_error(&format!("{MESSAGE_PREFIX_RUST_ANALYZER_LOADING}cargo metadata: error: invalid workspace"))]
    #[case::build_failure(&format!("{MESSAGE_PREFIX_RUST_ANALYZER_LOADING}build script engine failed"))]
    #[case::loading_warning(&format!("{MESSAGE_PREFIX_RUST_ANALYZER_LOADING}cargo metadata: warning: incomplete metadata"))]
    fn test_loading_diagnostics_remain_visible(#[case] diagnostic: &str) {
        let system_under_test = ScipDiagnostics::from_streams(&[], diagnostic.as_bytes());

        assert!(system_under_test.summary().contains(diagnostic));
    }

    #[rstest]
    #[case::thiserror("thiserror")]
    #[case::error_stack("error-stack")]
    #[case::failure("failure")]
    fn test_error_crate_names_are_progress(#[case] crate_name: &str) {
        let fixture = format!("{MESSAGE_PREFIX_RUST_ANALYZER_LOADING}build script {crate_name} run\n");
        let system_under_test = ScipDiagnostics::from_streams(&[], fixture.as_bytes());

        assert_eq!(system_under_test.summary(), "");
        assert!(!system_under_test.has_blocking_errors());
    }
    #[test]
    fn test_duplicate_notices_leave_unknown_diagnostics_visible() {
        let fixture = format!(
            "{}\nsource.rs:1:0-1:3\n  {MESSAGE_PREFIX_RUST_ANALYZER_DUPLICATE_SYMBOL}example main().\n\
             warning: failed to load build script\n",
            MESSAGE_RUST_ANALYZER_DUPLICATE_SYMBOL_NOTICE_LINES.join("\n")
        );
        let system_under_test = ScipDiagnostics::from_streams(&[], fixture.as_bytes());

        assert_eq!(system_under_test.nonblocking_errors.occurrences, 1);
        assert_eq!(system_under_test.other_messages.occurrences, 0, "Known duplicate notices must not leak into the summary");
        assert_eq!(system_under_test.summary(), "1 error diagnostic lines. Examples:\n  warning: failed to load build script");
    }

    #[rstest]
    #[case::prefix_only(&format!("{MESSAGE_PREFIX_RUST_ANALYZER_MISSING_SCIP_DEFINITION}source.rs:1:0 has an unknown problem"))]
    #[case::suffix_only(&format!("warning: index entry{MESSAGE_SUFFIX_RUST_ANALYZER_MISSING_SCIP_DEFINITION}"))]
    fn test_partial_internal_diagnostic_matches(#[case] diagnostic: &str) {
        let system_under_test = ScipDiagnostics::from_streams(&[], diagnostic.as_bytes());

        assert!(system_under_test.summary().contains(diagnostic), "Only a complete known diagnostic may be suppressed");
    }

    #[rstest]
    #[case::standalone_location("source.rs:1:0-1:3", "")]
    #[case::location_before_warning("source.rs:1:0-1:3", "warning: unknown analyzer diagnostic")]
    #[case::warning_before_duplicate("warning: unknown analyzer diagnostic", MESSAGE_PREFIX_RUST_ANALYZER_DUPLICATE_SYMBOL)]
    fn test_duplicate_notice_context(#[case] diagnostic: &str, #[case] following: &str) {
        let fixture = format!("{diagnostic}\n{following}");
        let system_under_test = ScipDiagnostics::from_streams(&[], fixture.as_bytes());

        assert!(system_under_test.summary().contains(diagnostic), "Location and duplicate context must both match to suppress");
    }

    #[rstest]
    #[case::timestamp("2026-09-26T00:06:16+06:00   warning: fixture", "warning: fixture")]
    #[case::numeric_prefix("2026 warnings remain", "2026 warnings remain")]
    #[case::text_prefix("TRACE warnings remain", "TRACE warnings remain")]
    #[case::no_separator("2026-09-26T00:06:16+06:00", "2026-09-26T00:06:16+06:00")]
    fn test_diagnostic_timestamp(#[case] diagnostic: &str, #[case] expected: &str) {
        let system_under_test = without_timestamp(diagnostic);

        assert_eq!(system_under_test, expected);
    }

    #[test]
    fn test_both_output_streams_contribute_diagnostics() {
        let system_under_test = ScipDiagnostics::from_streams(b"warning: stdout failure\n", b"ERROR stderr failure\n");

        assert_eq!(system_under_test.other_messages.occurrences + system_under_test.nonblocking_errors.occurrences, 2);
        assert!(system_under_test.summary().contains("stdout failure"));
        assert!(system_under_test.summary().contains("stderr failure"));
    }

    #[test]
    fn test_configuration_errors_remain_visible() {
        let fixture = format!("2026-09-26T00:06:16+06:00 {MESSAGE_PREFIX_RUST_ANALYZER_CONFIGURATION_ERRORS}invalid field])\n");
        let system_under_test = ScipDiagnostics::from_streams(&[], fixture.as_bytes());

        assert!(system_under_test.summary().contains("invalid field"));
    }

    #[test]
    fn test_repeated_internal_diagnostics_stay_quiet() {
        let fixture = internal_diagnostics().repeat(100);
        let system_under_test = ScipDiagnostics::from_streams(&[], fixture.as_bytes());

        assert_eq!(system_under_test.summary(), "");
        assert!(!system_under_test.has_blocking_errors());
    }

    #[test]
    fn test_internal_diagnostics_preserve_unknown_and_build_problems() {
        let fixture = format!("{}warning: custom diagnostic\nerror: failed to read Cargo.toml\n", internal_diagnostics());
        let system_under_test = ScipDiagnostics::from_streams(&[], fixture.as_bytes());

        let summary = system_under_test.summary();

        assert!(summary.contains("custom diagnostic"));
        assert!(summary.contains("failed to read Cargo.toml"));
        assert!(!summary.contains(MESSAGE_PREFIX_RUST_ANALYZER_MISSING_SCIP_DEFINITION));
        assert!(!summary.contains(MESSAGE_PREFIX_RUST_ANALYZER_UNNAMED_DEFINITION));
    }
    #[rstest]
    #[case::normal(false)]
    #[case::verbose(true)]
    fn test_internal_diagnostics_do_not_hide_process_failure(#[case] verbose: bool) {
        let fixture = internal_diagnostics();
        let system_under_test = ScipDiagnostics::from_streams(&[], fixture.as_bytes());

        let message = system_under_test.failure_message("analysis with internal diagnostics", &"exit code 1", verbose);

        assert!(message.contains(MESSAGE_DEPENDENCY_GRAPH_FAILURE));
        assert!(message.contains("exit code 1"));
        assert!(message.contains(MESSAGE_VERBOSE_DIAGNOSTICS_HINT));
        assert!(!message.contains(MESSAGE_PREFIX_RUST_ANALYZER_MISSING_SCIP_DEFINITION));
        assert!(!message.contains(MESSAGE_PREFIX_RUST_ANALYZER_UNNAMED_DEFINITION));
    }
    #[rstest]
    #[case::first_analysis("first analysis")]
    #[case::second_analysis("second analysis")]
    fn test_process_failure_without_actionable_diagnostics(#[case] analysis_label: &str, #[values(false, true)] verbose: bool) {
        let system_under_test = ScipDiagnostics::from_streams(&[], MESSAGE_RUST_ANALYZER_SCIP_STARTED.as_bytes());

        let message = system_under_test.failure_message(analysis_label, &"exit code 1", verbose);

        assert!(message.contains(analysis_label));
        assert!(message.contains("exit code 1"));
        assert_eq!(message.contains(MESSAGE_EMPTY_DIAGNOSTIC_SUMMARY), verbose);
        assert!(message.contains(MESSAGE_VERBOSE_DIAGNOSTICS_HINT));
    }

    #[rstest]
    #[case::normal(false)]
    #[case::verbose(true)]
    fn test_process_failure_diagnostic_visibility(
        #[case] verbose: bool,
        #[values("error: build script failed", "ERROR internal analyzer failure", "warning: analyzer warning")] diagnostic: &str,
        #[values("exit code 1", MESSAGE_PROJECT_LOADING_FAILURE_REASON)] reason: &str,
    ) {
        let analysis_label = "analysis fixture";
        let system_under_test = ScipDiagnostics::from_streams(&[], diagnostic.as_bytes());

        let message = system_under_test.failure_message(analysis_label, &reason, verbose);

        assert_eq!(message.contains(diagnostic), verbose);
        assert!(message.contains(MESSAGE_DEPENDENCY_GRAPH_FAILURE));
        assert!(message.contains(analysis_label));
        assert!(message.contains(reason));
        assert!(message.contains(MESSAGE_VERBOSE_DIAGNOSTICS_HINT));
        assert!(system_under_test.summary().contains(diagnostic));
    }

    #[test]
    fn test_diagnostic_examples_are_bounded() {
        let fixture = "warning: first\nwarning: second\nwarning: third\nwarning: fourth\nwarning: fifth\n".repeat(20);
        let system_under_test = ScipDiagnostics::from_streams(&[], fixture.as_bytes());

        assert_eq!(system_under_test.other_messages.occurrences, 100);
        assert_eq!(system_under_test.other_messages.examples.len(), MAXIMUM_DIAGNOSTIC_EXAMPLES_PER_GROUP);
        assert_eq!(system_under_test.summary().lines().count(), MAXIMUM_DIAGNOSTIC_EXAMPLES_PER_GROUP + 1);
    }

    #[test]
    fn test_repeated_unknown_diagnostics_share_one_example() {
        let fixture = "warning: optional service unavailable\n".repeat(12);
        let system_under_test = ScipDiagnostics::from_streams(&[], fixture.as_bytes());

        assert_eq!(system_under_test.other_messages.occurrences, 12);
        assert_eq!(system_under_test.other_messages.examples.len(), 1);
        assert_eq!(system_under_test.summary().matches("optional service unavailable").count(), 1);
    }

    #[test]
    fn test_build_errors_remain_visible_after_many_warnings() {
        let fixture = format!("{}error[E0432]: unresolved import\n", "warning: optional service unavailable\n".repeat(100));
        let system_under_test = ScipDiagnostics::from_streams(&[], fixture.as_bytes());

        assert_eq!(system_under_test.blocking_errors.occurrences, 1);
        assert!(system_under_test.summary().contains("error[E0432]: unresolved import"));
    }
    #[test]
    fn test_long_diagnostics_preserve_unicode_boundaries() {
        let fixture = "\u{1f980}".repeat(MAXIMUM_DIAGNOSTIC_EXAMPLE_CHARACTERS + 1);

        let system_under_test = diagnostic_line_preview(&fixture);

        assert_eq!(system_under_test, format!("{}...", "\u{1f980}".repeat(MAXIMUM_DIAGNOSTIC_EXAMPLE_CHARACTERS)));
    }
}
