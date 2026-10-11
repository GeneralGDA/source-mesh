use std::iter;

use anyhow::Error;
use anyhow::Result;
use clap::Parser as _;
use clap::error::ErrorKind;
use pretty_assertions::assert_eq;
use rstest::rstest;

use crate::BOOLEAN_OPTIONS_DEFAULT_HELP;
use crate::Cli;
use crate::error_message;
use crate::should_display_help;

#[rstest]
#[case(&["--depth=-1"])]
#[case(&["--depth", "not-a-number"])]
#[case(&["--format", "unknown"])]
#[case(&["--unknown-option"])]
#[case(&["--save-scip", "saved.scip"])]
#[case(&["--config", "config.json"])]
#[case(&["--cycle-limit", "0"])]
fn test_invalid_arguments(#[case] arguments: &[&str]) {
    let system_under_test = Cli::try_parse_from(
        [env!("CARGO_PKG_NAME"), "--from-scip", "input.scip"]
            .into_iter()
            .chain(arguments.iter().copied()),
    );

    assert!(system_under_test.is_err(), "CLI accepted invalid or conflicting arguments");
    if let Err(error) = system_under_test {
        assert_ne!(
            error.kind(),
            ErrorKind::DisplayHelp,
            "CLI displayed help instead of rejecting arguments"
        );
        assert!(
            !error.to_string().is_empty(),
            "CLI omitted invalid-argument diagnostics"
        );
    }
}

#[rstest]
#[case::no_arguments_without_project(false, false, true)]
#[case::arguments_without_project(true, false, false)]
#[case::no_arguments_with_project(false, true, false)]
#[case::arguments_with_project(true, true, false)]
fn test_help_display_conditions(
    #[case] has_arguments: bool,
    #[case] has_project_file: bool,
    #[case] expected: bool,
) {
    let system_under_test = should_display_help(has_arguments, has_project_file);

    assert_eq!(
        system_under_test, expected,
        "CLI help display policy changed"
    );
}

#[test]
fn test_folder_grouping_help() {
    let system_under_test = Cli::try_parse_from([env!("CARGO_PKG_NAME"), "--help"]);

    assert!(system_under_test.is_err(), "CLI did not return help");
    if let Err(error) = system_under_test {
        let help = error.to_string();

        assert_eq!(
            error.kind(),
            ErrorKind::DisplayHelp,
            "CLI rejected the help argument"
        );
        assert!(
            help.contains("--group-folders"),
            "CLI help omitted folder grouping"
        );
        assert!(
            help.contains(BOOLEAN_OPTIONS_DEFAULT_HELP),
            "CLI help omitted boolean option defaults"
        );
    }
}

#[test]
fn test_cycle_failure_argument() -> Result<()> {
    let system_under_test = Cli::try_parse_from([env!("CARGO_PKG_NAME"), "--fail-on-cycles"])?;

    assert!(
        system_under_test.cycles.fail_on_cycles,
        "CLI did not enable cycle failure"
    );
    Ok(())
}

#[rstest]
#[case(&["--analyzer-threads", "0"])]
#[case(&["--analysis-mode", "sequential"])]
fn test_removed_analyzer_arguments(#[case] arguments: &[&str]) {
    let system_under_test =
        Cli::try_parse_from(iter::once(env!("CARGO_PKG_NAME")).chain(arguments.iter().copied()));

    assert!(system_under_test.is_err(), "CLI accepted a removed analyzer option");
    if let Err(error) = system_under_test {
        assert_eq!(error.kind(), ErrorKind::UnknownArgument);
    }
}

#[rstest]
fn test_error_detail_visibility(#[values(false, true)] verbose: bool) {
    const FAILURE: &str = "Cannot resolve dependency definitions";
    const DIAGNOSTIC: &str = "Internal analyzer diagnostic";
    let fixture = Error::msg(DIAGNOSTIC).context(FAILURE);

    let system_under_test = error_message(&fixture, verbose);

    assert!(system_under_test.starts_with(FAILURE), "The failure reason is missing");
    assert_eq!(system_under_test.contains(DIAGNOSTIC), verbose, "Diagnostic details must require verbose output");
}

#[rstest]
#[case::default(&[], false)]
#[case::verbose(&["--verbose"], true)]
#[case::short(&["-v"], true)]
fn test_verbose_argument(#[case] arguments: &[&str], #[case] enabled: bool) -> Result<()> {
    let system_under_test = Cli::try_parse_from(iter::once(env!("CARGO_PKG_NAME")).chain(arguments.iter().copied()))?;

    assert_eq!(system_under_test.analyzer.verbose, enabled);
    Ok(())
}
