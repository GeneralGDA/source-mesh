use std::ffi::OsString;
use std::io;
use std::io::ErrorKind;
use std::io::Write;
use std::num::NonZeroUsize;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::ensure;
use clap::ArgMatches;
use clap::Args;
use clap::FromArgMatches as _;
use clap::Parser;
use clap::ValueEnum;
use getset::CopyGetters;
use serde::Deserialize;
use source_mesh_core::CycleOptions;
use source_mesh_core::DEFAULT_CYCLE_LIMIT;
use source_mesh_core::Exclusions;
use source_mesh_core::Input;
use source_mesh_core::InputFile;
use source_mesh_core::OutgoingDependencyOptions;
use source_mesh_core::Project;
use source_mesh_core::Request;
use source_mesh_core::analyzer_options::Options as AnalyzerOptions;
use source_mesh_core::backend::project_directory;
use source_mesh_core::export::Format as ExportFormat;
use source_mesh_core::outgoing_dependencies::Mode as DependencyMode;

pub(super) const SETTINGS_FILE_NAME: &str = 
    "source-mesh.toml";
pub(super) const BOOLEAN_OPTIONS_DEFAULT_HELP: &str =
    "Without settings, boolean options are disabled. Use --flag=false to override settings.";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, ValueEnum)]
#[serde(try_from = "String")]
enum Format {
    Dot,
    Mermaid,
    Graphml,
}

impl TryFrom<String> for Format {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::from_str(&value, false)
    }
}

impl From<Format> for ExportFormat {
    fn from(format: Format) -> Self {
        match format {
            Format::Dot => Self::Dot,
            Format::Mermaid => Self::Mermaid,
            Format::Graphml => Self::Graphml,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, ValueEnum)]
#[serde(try_from = "String")]
enum OutgoingDependencyMode {
    Files,
    Folders,
}

impl TryFrom<String> for OutgoingDependencyMode {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::from_str(&value, false)
    }
}

impl From<OutgoingDependencyMode> for DependencyMode {
    fn from(mode: OutgoingDependencyMode) -> Self {
        match mode {
            OutgoingDependencyMode::Files => Self::Files,
            OutgoingDependencyMode::Folders => Self::Folders,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(from = "PathBuf")]
enum SettingsPath {
    CommandLine(PathBuf),
    SettingsFile(PathBuf),
}

impl From<OsString> for SettingsPath {
    fn from(path: OsString) -> Self {
        Self::CommandLine(path.into())
    }
}

impl From<PathBuf> for SettingsPath {
    fn from(path: PathBuf) -> Self {
        Self::SettingsFile(path)
    }
}

impl SettingsPath {
    #[must_use]
    fn resolve(self, directory: &Path) -> PathBuf {
        match self {
            Self::CommandLine(path) => path,
            Self::SettingsFile(path) => directory.join(path),
        }
    }
}

#[derive(Args, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
struct Settings {
    #[command(flatten)]
    graph: GraphSettings,
    #[command(flatten)]
    export: ExportSettings,
    #[command(flatten)]
    cycles: CycleSettings,
    #[command(flatten)]
    outgoing_dependencies: OutgoingDependencySettings,
    #[command(flatten)]
    analysis: AnalysisSettings,
}

#[derive(Args, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
struct GraphSettings {
    #[arg(long,
        help = "Restrict to a relative index/project subtree, e.g. src (default: .)")]
    root: Option<PathBuf>,
    #[arg(short, long,
        help = "Maximum folder levels below --root: 0 = root only; default = all")]
    depth: Option<usize>,
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true",
        help = "Keep edges between distinct files collapsed into the same folder")]
    include_self: Option<bool>,
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true",
        help = "Hide dependencies used only by tests")]
    exclude_tests: Option<bool>,
    #[arg(long, value_name = "GLOB",
        help = "Exclude project-relative files; repeat to replace the settings exclusion list")]
    exclude: Vec<String>,
}

#[derive(Args, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
struct ExportSettings {
    #[arg(short, long, value_enum,
        help = "Diagram format (default: dot)")]
    format: Option<Format>,
    #[arg(short, long,
        help = "Output file (otherwise stdout)")]
    output: Option<SettingsPath>,
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true",
        help = "Nest subfolders, hide shared ancestor groups, and keep their own files as separate nodes")]
    group_folders: Option<bool>,
}

#[derive(Args, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
struct CycleSettings {
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true",
        help = "Highlight cyclic folder components with distinct bright colors")]
    highlight_cycles: Option<bool>,
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true",
        help = "Exit unsuccessfully after writing outputs if file or folder dependency cycles are found")]
    fail_on_cycles: Option<bool>,
    #[arg(long,
        help = "Write every simple folder cycle to a Markdown report")]
    cycles_output: Option<SettingsPath>,
    #[arg(long,
        help = "Write file cycles before folder folding to a separate Markdown report")]
    file_cycles_output: Option<SettingsPath>,
    #[arg(long,
        help = format!("Maximum cycles per report (default: {DEFAULT_CYCLE_LIMIT}); fail without output if exceeded"))]
    cycle_limit: Option<NonZeroUsize>,
}

#[derive(Args, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
struct OutgoingDependencySettings {
    #[arg(id = "max_outgoing_dependencies", long = "max-outgoing-dependencies", value_name = "N",
        help = "Fail after writing outputs if any node has more than N distinct outgoing targets")]
    maximum: Option<usize>,
    #[arg(id = "outgoing_dependencies_mode", long = "outgoing-dependencies-mode", value_enum,
        help = "Check files before folder folding, or folders after root/depth filtering (default: folders)")]
    mode: Option<OutgoingDependencyMode>,
    #[arg(id = "outgoing_dependencies_output", long = "outgoing-dependencies-output", value_name = "FILE",
        help = "Write outgoing dependency violations to Markdown")]
    output: Option<SettingsPath>,
}

#[derive(Args, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
struct AnalysisSettings {
    #[arg(long,
        help = "Reuse saved analysis without reanalyzing sources")]
    from_scip: Option<SettingsPath>,
    #[arg(long,
        help = "Save the index for fast re-export at other depths/formats")]
    save_scip: Option<SettingsPath>,
    #[arg(long,
        help = "rust-analyzer JSON config (features, target, cfg, etc.)")]
    config: Option<SettingsPath>,
    #[arg(short, long, num_args = 0..=1, require_equals = true, default_missing_value = "true",
        help = "Show diagnostic details and analysis timings")]
    verbose: Option<bool>,
}

#[derive(Parser)]
#[command(
    version,
    about = "Draw Rust folder dependencies, detect cycles, and check outgoing dependency limits",
    after_help = BOOLEAN_OPTIONS_DEFAULT_HELP
)]
pub(super) struct Cli {
    #[arg(default_value = ".",
        help = "Project/workspace directory or Cargo.toml; also selects automatic settings")]
    path: PathBuf,
    #[arg(long, value_name = "FILE", conflicts_with = "no_settings",
        help = "Read TOML settings instead of source-mesh.toml")]
    settings: Option<PathBuf>,
    #[arg(long,
        help = "Do not load TOML settings")]
    no_settings: bool,
    #[command(flatten)]
    options: Settings,
}

impl Cli {
    #[must_use]
    pub(super) const fn verbose(&self) -> bool {
        matches!(self.options.analysis.verbose, Some(true))
    }
}

impl Settings {
    fn parse(contents: &str) -> Result<Self> {
        let table = toml::de::DeTable::parse(contents)?;
        for (section, value) in table.get_ref() {
            let section = section.get_ref();
            ensure!(value.get_ref().as_table().is_some(), "Settings section {section:?} must be a table");
        }
        Ok(Self::deserialize(toml::de::Deserializer::from(table))?)
    }

    fn merge(self, mut cli: Cli, mut matches: ArgMatches) -> Result<Cli> {
        cli.options = self;
        cli.options.update_from_arg_matches_mut(&mut matches)?;
        Ok(cli)
    }
}

pub(super) struct LoadedSettings {
    cli: Cli,
    directory: PathBuf,
    protected_inputs: Vec<InputFile>,
}

impl LoadedSettings {
    pub(super) fn load(cli: Cli, matches: ArgMatches) -> Result<Self> {
        let mut loaded = Self { cli, directory: PathBuf::new(), protected_inputs: Vec::new() };
        if loaded.cli.no_settings {
            return Ok(loaded);
        }
        let explicit = loaded.cli.settings.is_some();
        let path = match loaded.cli.settings.as_ref() {
            Some(path) => path.clone(),
            None => project_directory(&loaded.cli.path)?.join(SETTINGS_FILE_NAME),
        };
        let mut input = match InputFile::open(&path) {
            Ok(input) => input,
            Err(error) => {
                if !explicit && error.downcast_ref::<io::Error>().is_some_and(|error| error.kind() == ErrorKind::NotFound) {
                    return Ok(loaded);
                }
                return Err(error).with_context(|| format!("Cannot load settings {}", path.display()));
            }
        };
        let contents = input.read()?;
        let contents = std::str::from_utf8(&contents).with_context(|| format!("Settings {} must be UTF-8", path.display()))?;
        let settings = Settings::parse(contents).map_err(|error| anyhow!("Cannot parse settings {}: {error}", path.display()))?;
        loaded.directory = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
        loaded.cli = settings.merge(loaded.cli, matches)?;
        loaded.protected_inputs.push(input);
        Ok(loaded)
    }

    #[must_use]
    pub(super) const fn verbose(&self) -> bool {
        self.cli.verbose()
    }

    #[must_use]
    pub(super) const fn has_settings(&self) -> bool {
        !self.protected_inputs.is_empty()
    }

    pub(super) fn into_command(self) -> Result<Command> {
        let Self { cli, directory, protected_inputs } = self;
        let Cli { path: project, options, settings: _, no_settings: _ } = cli;
        let Settings {
            graph: GraphSettings { root, depth, include_self, exclude_tests, exclude },
            export: ExportSettings { format, output, group_folders },
            cycles: CycleSettings { highlight_cycles, fail_on_cycles, cycles_output, file_cycles_output, cycle_limit },
            outgoing_dependencies: OutgoingDependencySettings { maximum, mode, output: outgoing_output },
            analysis: AnalysisSettings { from_scip, save_scip, config, verbose },
        } = options;
        ensure!(from_scip.is_none() || save_scip.is_none(), "--from-scip cannot be combined with --save-scip");
        ensure!(from_scip.is_none() || config.is_none(), "--from-scip cannot be combined with --config");
        ensure!(
            maximum.is_some() || (mode.is_none() && outgoing_output.is_none()),
            "Outgoing dependency settings require --max-outgoing-dependencies or [outgoing-dependencies].maximum"
        );
        let resolve = |path: SettingsPath| path.resolve(&directory);
        let from_scip = from_scip.map(resolve);
        let input = from_scip.map_or_else(|| Input::Project(Project::new(project, config.map(resolve))), Input::ScipFile);
        let verbose = verbose.unwrap_or(false);
        let cycles = CycleOptions::builder()
            .maybe_highlight_cycles(highlight_cycles).maybe_fail_on_cycles(fail_on_cycles).maybe_cycle_limit(cycle_limit)
            .maybe_cycles_output(cycles_output.map(resolve)).maybe_file_cycles_output(file_cycles_output.map(resolve)).build();
        let outgoing = maximum.map(|maximum| {
            OutgoingDependencyOptions::builder().maximum(maximum).maybe_mode(mode.map(Into::into))
                .maybe_output(outgoing_output.map(resolve)).build()
        });
        let request = Request::builder()
            .input(input).exclusions(Exclusions::new(&exclude)?).protected_inputs(protected_inputs)
            .analyzer(AnalyzerOptions::new(verbose)).maybe_root(root).maybe_depth(depth)
            .maybe_include_self(include_self).maybe_exclude_tests(exclude_tests)
            .maybe_format(format.map(Into::into)).maybe_output(output.map(resolve)).maybe_group_folders(group_folders)
            .cycles(cycles).maybe_outgoing_dependencies(outgoing).maybe_save_scip(save_scip.map(resolve)).build()?;
        Ok(Command { request, verbose })
    }
}

#[derive(CopyGetters)]
pub(super) struct Command {
    request: Request,
    #[getset(get_copy = "pub(super)")]
    verbose: bool,
}

impl Command {
    pub(super) fn run(&self, standard_output: &mut dyn Write) -> Result<()> {
        source_mesh_core::run(&self.request, standard_output)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::iter;
    use std::num::NonZeroUsize;
    use std::path::Path;
    use std::path::PathBuf;

    use anyhow::Context as _;
    use anyhow::Error;
    use anyhow::Result;
    use claims::assert_ok;
    use claims::assert_some;
    use clap::ArgMatches;
    use clap::Args as _;
    use clap::Command;
    use clap::CommandFactory as _;
    use clap::FromArgMatches as _;
    use clap::Parser as _;
    use clap::error::ErrorKind;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use source_mesh_core::backend::project_directory;
    use tempfile::TempDir;
    use tempfile::tempdir;

    use super::AnalysisSettings;
    use super::Cli;
    use super::CycleSettings;
    use super::ExportSettings;
    use super::Format;
    use super::GraphSettings;
    use super::LoadedSettings;
    use super::OutgoingDependencyMode;
    use super::OutgoingDependencySettings;
    use super::SETTINGS_FILE_NAME;
    use super::Settings;
    use super::SettingsPath;
    use super::SettingsPath::CommandLine;
    use super::SettingsPath::SettingsFile;

    const CONTENTS_COMPLETE_SETTINGS_TEST_FIXTURE: &str = r#"
[graph]
root = "src"
depth = 3
include-self = true
exclude-tests = true
exclude = ["src/generated/**", "**/tests/**"]
[export]
format = "graphml"
output = "diagram.graphml"
group-folders = true
[cycles]
highlight-cycles = true
fail-on-cycles = true
cycles-output = "folders.md"
file-cycles-output = "files.md"
cycle-limit = 7
[outgoing-dependencies]
maximum = 9
mode = "files"
output = "outgoing.md"
[analysis]
from-scip = "input.scip"
save-scip = "saved.scip"
config = "analyzer.json"
verbose = true
"#;

    #[must_use]
    fn saved_index_path_test_fixture() -> PathBuf {
        PathBuf::from_iter([env!("CARGO_MANIFEST_DIR"), "..", "source-mesh-core", "src", "fixtures", "dependency-graph.scip"])
    }

    struct CliTestFixture {
        cli: Cli,
        matches: ArgMatches,
    }

    impl CliTestFixture {
        fn new(cli_arguments: &[&str]) -> Result<Self> {
            let arguments = iter::once(env!("CARGO_PKG_NAME")).chain(cli_arguments.iter().copied());
            let matches = Cli::command().try_get_matches_from(arguments)?;
            let cli = Cli::from_arg_matches(&matches)?;
            Ok(Self { cli, matches })
        }
    }

    struct SettingsMergeTestFixture {
        settings: Settings,
        cli: Cli,
        matches: ArgMatches,
    }

    impl SettingsMergeTestFixture {
        fn new(contents: &str, cli_arguments: &[&str]) -> Result<Self> {
            let settings = Settings::parse(contents).context("Settings merge test fixture must contain valid settings")?;
            let fixture = CliTestFixture::new(cli_arguments);
            let CliTestFixture { cli, matches } = fixture.context("Settings merge test fixture arguments must parse")?;
            Ok(Self { settings, cli, matches })
        }
    }

    struct SettingsDirectoryTestFixture {
        directory: TempDir,
        settings_path: PathBuf,
    }

    impl SettingsDirectoryTestFixture {
        fn new(contents: Option<&[u8]>) -> Result<Self> {
            let directory = tempdir()?;
            let settings_path = directory.path().join(SETTINGS_FILE_NAME);
            if let Some(contents) = contents {
                fs::write(&settings_path, contents)?;
            }
            Ok(Self { directory, settings_path })
        }
    }

    struct SettingsLoadingTestFixture {
        directory: TempDir,
        settings_path: PathBuf,
        cli: Cli,
        matches: ArgMatches,
    }

    impl SettingsLoadingTestFixture {
        fn new(settings_file_contents: Option<&[u8]>, cli_arguments: &[&str]) -> Result<Self> {
            let files = SettingsDirectoryTestFixture::new(settings_file_contents);
            let files = files.context("Settings loading test files must be prepared")?;
            let fixture = CliTestFixture::new(cli_arguments);
            let CliTestFixture { mut cli, matches } = fixture.context("Settings loading test fixture arguments must parse")?;
            cli.path = files.directory.path().to_path_buf();
            Ok(Self { directory: files.directory, settings_path: files.settings_path, cli, matches })
        }
    }

    #[track_caller]
    fn assert_missing_file_error(error: &Error, expected_path: &Path) {
        let cause = error.downcast_ref::<io::Error>();
        let cause = assert_some!(cause, "Missing-file errors must retain their I/O cause: {error:#}");
        let diagnostic = error.to_string();
        let path = expected_path.display().to_string();
        assert_eq!(cause.kind(), io::ErrorKind::NotFound, "Missing-file errors must retain their I/O kind");
        assert!(diagnostic.contains(&path), "Missing-file errors must identify {path}: {diagnostic}");
    }

    #[test]
    fn test_settings_arguments_have_no_defaults() {
        let mut system_under_test = Settings::augment_args(Command::new(env!("CARGO_PKG_NAME")));
        system_under_test.build();

        for argument in system_under_test.get_arguments() {
            assert!(argument.get_default_values().is_empty(),
                "Settings argument {} must not override file values when absent", argument.get_id());
        }
    }

    #[rstest]
    #[case(&["--depth=-1"])]
    #[case(&["--depth", "not-a-number"])]
    #[case(&["--format", "unknown"])]
    #[case(&["--unknown-option"])]
    #[case(&["--cycle-limit", "0"])]
    #[case(&["--max-outgoing-dependencies=-1"])]
    #[case(&["--max-outgoing-dependencies", "not-a-number"])]
    #[case(&["--max-outgoing-dependencies", "1.5"])]
    #[case(&["--max-outgoing-dependencies", "18446744073709551616"])]
    #[case(&["--max-outgoing-dependencies"])]
    #[case(&["--max-outgoing-dependencies", "1", "--outgoing-dependencies-mode", "unknown"])]
    fn test_invalid_arguments(#[case] arguments: &[&str]) {
        let replay_arguments = [env!("CARGO_PKG_NAME"), "--from-scip", "input.scip"];
        let system_under_test = Cli::try_parse_from(replay_arguments.into_iter().chain(arguments.iter().copied()));

        let error = system_under_test.err();
        let error = assert_some!(error, "CLI accepted invalid or conflicting arguments");
        let diagnostic = error.to_string();
        assert_ne!(error.kind(), ErrorKind::DisplayHelp, "CLI displayed help instead of rejecting arguments");
        assert!(!diagnostic.is_empty(), "CLI omitted invalid-argument diagnostics");
    }

    #[rstest]
    #[case::disabled(
        &[],
        None, None, false,
    )]
    #[case::zero(
        &["--max-outgoing-dependencies", "0"],
        Some(0), None, false,
    )]
    #[case::files(
        &["--max-outgoing-dependencies", "5", "--outgoing-dependencies-mode", "files"],
        Some(5), Some(OutgoingDependencyMode::Files), false,
    )]
    #[case::folders(
        &["--max-outgoing-dependencies", "7", "--outgoing-dependencies-mode", "folders"],
        Some(7), Some(OutgoingDependencyMode::Folders), false,
    )]
    #[case::report(
        &["--max-outgoing-dependencies", "3", "--outgoing-dependencies-output", "dependencies.md"],
        Some(3), None, true,
    )]
    fn test_outgoing_dependency_arguments(
        #[case] arguments: &[&str],
        #[case] maximum: Option<usize>,
        #[case] mode: Option<OutgoingDependencyMode>,
        #[case] report: bool,
    ) {
        let system_under_test = Cli::try_parse_from(iter::once(env!("CARGO_PKG_NAME")).chain(arguments.iter().copied()));
        let system_under_test = assert_ok!(system_under_test, "CLI must accept outgoing dependency arguments: {arguments:?}");
        let expected = OutgoingDependencySettings {
            maximum, mode, output: report.then(|| CommandLine("dependencies.md".into())),
        };

        assert_eq!(system_under_test.options.outgoing_dependencies, expected);
    }

    #[rstest]
    #[case(&["--analyzer-threads", "0"])]
    #[case(&["--analysis-mode", "sequential"])]
    fn test_removed_analyzer_arguments(#[case] arguments: &[&str]) {
        let system_under_test = Cli::try_parse_from(iter::once(env!("CARGO_PKG_NAME")).chain(arguments.iter().copied()));

        let error = system_under_test.err();
        let error = assert_some!(error, "CLI accepted a removed analyzer option");
        assert_eq!(error.kind(), ErrorKind::UnknownArgument, "Removed analyzer options must be unknown arguments");
    }

    #[rstest]
    #[case::default(&[], false)]
    #[case::verbose(&["--verbose"], true)]
    #[case::short(&["-v"], true)]
    fn test_verbose_argument(#[case] arguments: &[&str], #[case] enabled: bool) {
        let system_under_test = Cli::try_parse_from(iter::once(env!("CARGO_PKG_NAME")).chain(arguments.iter().copied()));
        let system_under_test = assert_ok!(system_under_test, "CLI must accept supported verbosity arguments: {arguments:?}");

        let verbose = system_under_test.verbose();

        assert_eq!(verbose, enabled, "CLI verbosity must follow the supplied arguments");
    }

    #[test]
    fn test_cli_numeric_range_survives_merge() {
        let contents = "[graph]\ndepth = 1\n[cycles]\ncycle-limit = 1\n[outgoing-dependencies]\nmaximum = 1";
        let maximum = usize::MAX.to_string();
        let saved_index_path = saved_index_path_test_fixture().display().to_string();
        let fixture = SettingsMergeTestFixture::new(contents, &[
            "--from-scip", &saved_index_path, "--depth", &maximum, "--cycle-limit", &maximum,
            "--max-outgoing-dependencies", &maximum,
        ]);
        let fixture = assert_ok!(fixture, "CLI numbers must support the complete native usize range");

        let system_under_test = fixture.settings.merge(fixture.cli, fixture.matches);
        let system_under_test = assert_ok!(system_under_test, "Native CLI numbers must merge without narrowing their range");

        let options = &system_under_test.options;
        assert_eq!(options.graph.depth, Some(usize::MAX));
        assert_eq!(options.cycles.cycle_limit, Some(NonZeroUsize::MAX));
        assert_eq!(options.outgoing_dependencies.maximum, Some(usize::MAX));
        let loaded = LoadedSettings { cli: system_under_test, directory: PathBuf::new(), protected_inputs: Vec::new() };
        let command = loaded.into_command();
        assert_ok!(command, "Builder projection must not constrain CLI numbers to TOML's signed integer range");
    }

    #[cfg(unix)]
    fn non_utf8_path_test_fixture() -> PathBuf {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        PathBuf::from(OsString::from_vec(b"native-\xff".to_vec()))
    }

    #[cfg(windows)]
    fn non_utf8_path_test_fixture() -> PathBuf {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt as _;

        PathBuf::from(OsString::from_wide(&[0x006e, 0xd800]))
    }

    #[cfg(any(unix, windows))]
    #[rstest]
    #[case::native_input(true)]
    #[case::native_output(false)]
    fn test_native_cli_paths_survive_merge_and_projection(#[case] native_input: bool) {
        use std::ffi::OsStr;

        let contents = b"[export]\noutput = 'file.dot'\n[analysis]\nfrom-scip = 'file.scip'";
        let fixture = SettingsDirectoryTestFixture::new(Some(contents));
        let fixture = assert_ok!(fixture, "Native path settings must be written");
        let input_path = if native_input {
            fixture.directory.path().join(non_utf8_path_test_fixture()).with_extension("scip")
        } else {
            saved_index_path_test_fixture()
        };
        let output_path = fixture.directory.path().join(non_utf8_path_test_fixture()).with_extension("dot");
        assert!(output_path.to_str().is_none(), "The fixture must exercise a genuinely non-UTF-8 output path");
        assert_eq!(input_path.to_str().is_none(), native_input, "Only the native-input case may use a non-UTF-8 input path");
        let arguments = [
            OsStr::new(env!("CARGO_PKG_NAME")), OsStr::new("--settings"), fixture.settings_path.as_os_str(),
            OsStr::new("--from-scip"), input_path.as_os_str(), OsStr::new("--output"), output_path.as_os_str(),
        ];
        let matches = Cli::command().try_get_matches_from(arguments);
        let matches = assert_ok!(matches, "Native path arguments must parse without UTF-8 conversion");
        let cli = Cli::from_arg_matches(&matches);
        let cli = assert_ok!(cli, "Native path matches must convert to CLI options without UTF-8 conversion");
        let system_under_test = LoadedSettings::load(cli, matches);
        let system_under_test = assert_ok!(system_under_test, "Native paths must override the file layer without serialization");

        let options = &system_under_test.cli.options;
        let paths = [options.analysis.from_scip.clone(), options.export.output.clone()];
        let paths = paths.map(|path| path.map(|path| path.resolve(&system_under_test.directory)));
        let expected = [Some(input_path), Some(output_path)];
        assert_eq!(paths, expected);
        let command = system_under_test.into_command();
        if native_input {
            let error = command.err();
            let error = assert_some!(error, "An absent native input path must fail during request construction");
            assert!(error.is::<io::Error>(), "Native input paths must reach file I/O without UTF-8 conversion: {error:#}");
        } else {
            assert_ok!(command, "Native output paths must survive builder projection without serialization or output I/O");
        }
    }

    #[rstest]
    #[case::defaults(false)]
    #[case::partial_overrides(true)]
    fn test_cli_settings_merge(#[case] overridden: bool) {
        let mut expected = Settings {
            graph: GraphSettings {
                root: Some("src".into()), depth: Some(3), include_self: Some(true), exclude_tests: Some(true),
                exclude: vec!["src/generated/**".into(), "**/tests/**".into()],
            },
            export: ExportSettings {
                format: Some(Format::Graphml), output: Some(SettingsFile("diagram.graphml".into())), group_folders: Some(true),
            },
            cycles: CycleSettings {
                highlight_cycles: Some(true), fail_on_cycles: Some(true), cycle_limit: NonZeroUsize::new(7),
                cycles_output: Some(SettingsFile("folders.md".into())),
                file_cycles_output: Some(SettingsFile("files.md".into())),
            },
            outgoing_dependencies: OutgoingDependencySettings {
                maximum: Some(9), mode: Some(OutgoingDependencyMode::Files), output: Some(SettingsFile("outgoing.md".into())),
            },
            analysis: AnalysisSettings {
                from_scip: Some(SettingsFile("input.scip".into())), save_scip: Some(SettingsFile("saved.scip".into())),
                config: Some(SettingsFile("analyzer.json".into())), verbose: Some(true),
            },
        };
        let arguments: &[&str] = if overridden {
            expected.graph.depth = Some(0);
            expected.export.output = Some(CommandLine("cli.dot".into()));
            expected.cycles.fail_on_cycles = Some(false);
            expected.outgoing_dependencies.mode = Some(OutgoingDependencyMode::Folders);
            expected.analysis.verbose = Some(false);
            &[
                "--depth", "0", "--output", "cli.dot", "--fail-on-cycles=false",
                "--outgoing-dependencies-mode", "folders", "--verbose=false",
            ]
        } else { &[] };
        let fixture = SettingsMergeTestFixture::new(CONTENTS_COMPLETE_SETTINGS_TEST_FIXTURE, arguments);
        let fixture = assert_ok!(fixture, "Complete settings and CLI arguments must parse before merging");

        let system_under_test = fixture.settings.merge(fixture.cli, fixture.matches);
        let system_under_test = assert_ok!(system_under_test, "CLI options must merge independently per field");

        assert_eq!(system_under_test.path, Path::new("."), "Settings must not change the selected project");
        assert_eq!(system_under_test.options, expected, "Only explicitly supplied fields may override file settings");
    }

    #[test]
    fn test_explicit_cli_values_override_settings() {
        let fixture = SettingsMergeTestFixture::new(CONTENTS_COMPLETE_SETTINGS_TEST_FIXTURE, &[
            "--root", ".", "--depth", "0", "--include-self=false", "--exclude-tests=false",
            "--exclude", "cli/**", "--exclude", "**/*.generated.rs", "--format", "dot", "--output", "cli.dot",
            "--group-folders=false", "--highlight-cycles=false", "--fail-on-cycles=false", "--cycle-limit", "1",
            "--cycles-output", "cli-folders.md", "--file-cycles-output", "cli-files.md",
            "--max-outgoing-dependencies", "0", "--outgoing-dependencies-mode", "folders",
            "--outgoing-dependencies-output", "cli-outgoing.md", "--from-scip", "cli-input.scip",
            "--save-scip", "cli-saved.scip", "--config", "cli-analyzer.json", "--verbose=false",
        ]);
        let fixture = assert_ok!(fixture, "Explicit CLI values must parse before cross-field validation");

        let system_under_test = fixture.settings.merge(fixture.cli, fixture.matches);
        let system_under_test = assert_ok!(system_under_test, "Explicit CLI values must replace their file settings");

        let expected = Settings {
            graph: GraphSettings {
                root: Some(".".into()), depth: Some(0), include_self: Some(false), exclude_tests: Some(false),
                exclude: vec!["cli/**".into(), "**/*.generated.rs".into()],
            },
            export: ExportSettings {
                format: Some(Format::Dot), output: Some(CommandLine("cli.dot".into())), group_folders: Some(false),
            },
            cycles: CycleSettings {
                highlight_cycles: Some(false), fail_on_cycles: Some(false), cycle_limit: NonZeroUsize::new(1),
                cycles_output: Some(CommandLine("cli-folders.md".into())),
                file_cycles_output: Some(CommandLine("cli-files.md".into())),
            },
            outgoing_dependencies: OutgoingDependencySettings {
                maximum: Some(0), mode: Some(OutgoingDependencyMode::Folders),
                output: Some(CommandLine("cli-outgoing.md".into())),
            },
            analysis: AnalysisSettings {
                from_scip: Some(CommandLine("cli-input.scip".into())), save_scip: Some(CommandLine("cli-saved.scip".into())),
                config: Some(CommandLine("cli-analyzer.json".into())), verbose: Some(false),
            },
        };
        assert_eq!(system_under_test.options, expected, "Explicit CLI values, including false and zero, must override TOML");
    }

    #[rstest]
    #[case::absent(None, None)]
    #[case::bare(Some(""), Some(true))]
    #[case::true_value(Some("true"), Some(true))]
    #[case::false_value(Some("false"), Some(false))]
    fn test_boolean_setting_precedence(
        #[case] value: Option<&str>,
        #[case] explicit: Option<bool>,
        #[values(false, true)] configured: bool,
    ) {
        let contents = format!("
[graph]
include-self = {configured}
exclude-tests = {configured}
[export]
group-folders = {configured}
[cycles]
highlight-cycles = {configured}
fail-on-cycles = {configured}
[analysis]
verbose = {configured}
");
        let flags = [
            "--include-self", "--exclude-tests", "--group-folders", "--highlight-cycles", "--fail-on-cycles", "--verbose",
        ];
        let arguments: Vec<_> = value.into_iter().flat_map(|value| {
            flags.map(|flag| if value.is_empty() { flag.to_owned() } else { format!("{flag}={value}") })
        }).collect();
        let arguments: Vec<_> = arguments.iter().map(String::as_str).collect();
        let fixture = SettingsMergeTestFixture::new(&contents, &arguments);
        let fixture = assert_ok!(fixture, "Boolean settings and CLI flags must parse before merging");

        let fail_on_cycles = fixture.cli.options.cycles.fail_on_cycles;
        assert_eq!(fail_on_cycles, explicit, "CLI cycle-failure flags must preserve the supplied value");

        let system_under_test = fixture.settings.merge(fixture.cli, fixture.matches);
        let system_under_test = assert_ok!(system_under_test, "Boolean precedence must follow explicit CLI values");

        let enabled = Some(explicit.unwrap_or(configured));
        let expected = Settings {
            graph: GraphSettings { include_self: enabled, exclude_tests: enabled, ..GraphSettings::default() },
            export: ExportSettings { group_folders: enabled, ..ExportSettings::default() },
            cycles: CycleSettings { highlight_cycles: enabled, fail_on_cycles: enabled, ..CycleSettings::default() },
            analysis: AnalysisSettings { verbose: enabled, ..AnalysisSettings::default() },
            ..Settings::default()
        };
        assert_eq!(system_under_test.options, expected, "Boolean precedence must be consistent across all settings");
    }

    #[rstest]
    #[case::single(&["--exclude", "cli/**"], &["cli/**"])]
    #[case::duplicates(&["--exclude", "cli/**", "--exclude", "cli/**"], &["cli/**", "cli/**"])]
    fn test_exclusion_list_replacement(#[case] arguments: &[&str], #[case] expected: &[&str]) {
        let fixture = SettingsMergeTestFixture::new("[graph]\nexclude = ['file/**', '**/tests/**']", arguments);
        let fixture = assert_ok!(fixture, "Settings and CLI exclusion lists must parse before merging");

        let system_under_test = fixture.settings.merge(fixture.cli, fixture.matches);
        let system_under_test = assert_ok!(system_under_test, "Explicit CLI exclusions must replace the file list");

        let exclusions = system_under_test.options.graph.exclude;
        assert_eq!(exclusions, expected, "CLI exclusions must replace, not append to, the file list");
    }

    #[rstest]
    #[case::top_level("unknown = true")]
    #[case::unknown_section("[unknown]\nvalue = true")]
    #[case::graph_key("[graph]\nunknown = true")]
    #[case::export_key("[export]\nunknown = true")]
    #[case::cycles_key("[cycles]\nunknown = true")]
    #[case::outgoing_key("[outgoing-dependencies]\nunknown = true")]
    #[case::analysis_key("[analysis]\nunknown = true")]
    #[case::underscore_key("[graph]\ninclude_self = true")]
    #[case::outgoing_cli_key("[outgoing-dependencies]\nmax-outgoing-dependencies = 1")]
    #[case::graph_type("graph = false")]
    #[case::graph_array("graph = []")]
    #[case::graph_positional("graph = ['src']")]
    #[case::export_type("export = []")]
    #[case::export_positional("export = ['dot']")]
    #[case::cycles_type("cycles = 1")]
    #[case::cycles_array("cycles = []")]
    #[case::cycles_positional("cycles = [true]")]
    #[case::outgoing_type("outgoing-dependencies = 'files'")]
    #[case::outgoing_array("outgoing-dependencies = []")]
    #[case::outgoing_positional("outgoing-dependencies = [0]")]
    #[case::analysis_type("analysis = true")]
    #[case::analysis_array("analysis = []")]
    #[case::analysis_positional("analysis = ['saved.scip']")]
    #[case::root_type("[graph]\nroot = 1")]
    #[case::root_date("[graph]\nroot = 2026-11-04")]
    #[case::output_datetime("[export]\noutput = 2026-11-04T12:30:00Z")]
    #[case::exclude_time("[graph]\nexclude = [12:30:00]")]
    #[case::format_table("[export]\nformat = { dot = {} }")]
    #[case::mode_table("[outgoing-dependencies]\nmode = { files = [] }")]
    #[case::depth_type("[graph]\ndepth = '3'")]
    #[case::depth_fraction("[graph]\ndepth = 1.5")]
    #[case::depth_negative("[graph]\ndepth = -1")]
    #[case::include_self_type("[graph]\ninclude-self = 'true'")]
    #[case::exclude_tests_type("[graph]\nexclude-tests = 1")]
    #[case::exclude_type("[graph]\nexclude = 'src/**'")]
    #[case::exclude_element_type("[graph]\nexclude = ['src/**', 1]")]
    #[case::format_type("[export]\nformat = false")]
    #[case::format_variant("[export]\nformat = 'unknown'")]
    #[case::format_case("[export]\nformat = 'DOT'")]
    #[case::output_type("[export]\noutput = false")]
    #[case::group_folders_type("[export]\ngroup-folders = 'false'")]
    #[case::highlight_type("[cycles]\nhighlight-cycles = 1")]
    #[case::fail_on_cycles_type("[cycles]\nfail-on-cycles = 'true'")]
    #[case::cycles_output_type("[cycles]\ncycles-output = []")]
    #[case::file_cycles_output_type("[cycles]\nfile-cycles-output = 1")]
    #[case::cycle_limit_type("[cycles]\ncycle-limit = '10'")]
    #[case::cycle_limit_fraction("[cycles]\ncycle-limit = 1.5")]
    #[case::cycle_limit_negative("[cycles]\ncycle-limit = -1")]
    #[case::cycle_limit_zero("[cycles]\ncycle-limit = 0")]
    #[case::maximum_type("[outgoing-dependencies]\nmaximum = '0'")]
    #[case::maximum_fraction("[outgoing-dependencies]\nmaximum = 1.5")]
    #[case::maximum_negative("[outgoing-dependencies]\nmaximum = -1")]
    #[case::maximum_overflow("[outgoing-dependencies]\nmaximum = 18446744073709551616")]
    #[case::mode_type("[outgoing-dependencies]\nmode = 1")]
    #[case::mode_variant("[outgoing-dependencies]\nmode = 'unknown'")]
    #[case::mode_case("[outgoing-dependencies]\nmode = 'FILES'")]
    #[case::outgoing_output_type("[outgoing-dependencies]\noutput = false")]
    #[case::from_scip_type("[analysis]\nfrom-scip = []")]
    #[case::save_scip_type("[analysis]\nsave-scip = 1")]
    #[case::config_type("[analysis]\nconfig = true")]
    #[case::verbose_type("[analysis]\nverbose = 'true'")]
    #[case::duplicate("[graph]\ndepth = 1\ndepth = 2")]
    #[case::syntax("[graph")]
    fn test_settings_validation(#[case] contents: &str) {
        let fixture = SettingsLoadingTestFixture::new(Some(contents.as_bytes()), &[]);
        let mut fixture = assert_ok!(fixture, "Invalid settings fixture must be prepared successfully");
        fixture.cli.settings = Some(fixture.settings_path.clone());

        let system_under_test = LoadedSettings::load(fixture.cli, fixture.matches).err();
        let error = assert_some!(system_under_test, "Malformed settings were accepted: {contents}");
        let cause = error.downcast_ref::<io::Error>();
        let diagnostic = error.to_string();
        let file_name = fixture.settings_path.display().to_string();

        assert!(cause.is_none(), "Validation failed for an unrelated filesystem reason: {error:#}");
        assert!(diagnostic.contains(&file_name), "Settings errors must identify their file: {diagnostic}");
    }

    #[rstest]
    #[case::number("[graph]\ndepth = 'invalid'", &["--depth", "0"])]
    #[case::boolean("[analysis]\nverbose = 'invalid'", &["--verbose=false"])]
    #[case::choice("[export]\nformat = 'invalid'", &["--format", "dot"])]
    #[case::list("[graph]\nexclude = 1", &["--exclude", "src/**"])]
    #[case::path_date("[export]\noutput = 2026-11-04", &["--output", "cli.dot"])]
    #[case::list_date("[graph]\nexclude = [2026-11-04]", &["--exclude", "cli/**"])]
    #[case::choice_table("[export]\nformat = { dot = {} }", &["--format", "dot"])]
    fn test_cli_overrides_do_not_hide_invalid_settings(#[case] contents: &str, #[case] arguments: &[&str]) {
        let fixture = SettingsLoadingTestFixture::new(Some(contents.as_bytes()), arguments);
        let mut fixture = assert_ok!(fixture, "Invalid settings and valid CLI overrides must be prepared");
        fixture.cli.settings = Some(fixture.settings_path);

        let system_under_test = LoadedSettings::load(fixture.cli, fixture.matches).err();
        let error = assert_some!(system_under_test, "An override must not conceal an invalid lower-priority value");
        let diagnostic = error.to_string();

        assert!(diagnostic.contains("Cannot parse settings"), "Unexpected loading failure: {error:#}");
    }

    #[test]
    fn test_empty_settings_sections() {
        let contents = "[graph]\n[export]\n[cycles]\n[outgoing-dependencies]\n[analysis]";
        let fixture = SettingsMergeTestFixture::new(contents, &[]);
        let fixture = assert_ok!(fixture, "Empty settings sections and default CLI arguments must parse");

        let system_under_test = fixture.settings.merge(fixture.cli, fixture.matches);
        let system_under_test = assert_ok!(system_under_test, "Empty sections must not introduce settings values");

        let expected = Settings::default();
        assert_eq!(system_under_test.options, expected, "Empty sections must not introduce any explicit settings");
    }

    #[rstest]
    #[case::directory(false)]
    #[case::manifest(true)]
    fn test_automatic_discovery_uses_selected_project(#[case] manifest: bool) {
        let fixture = SettingsDirectoryTestFixture::new(Some(b"[graph]\ndepth = 99"));
        let fixture = assert_ok!(fixture, "An ancestor settings file must be created");
        let selected = fixture.directory.path().join("selected");
        let created = fs::create_dir_all(&selected);
        assert_ok!(created, "The selected project directory must be created");
        let contents = "[graph]\ndepth = 2\n[export]\nformat = 'mermaid'\noutput = 'selected.md'";
        let settings_write = fs::write(selected.join(SETTINGS_FILE_NAME), contents);
        assert_ok!(settings_write, "The selected project's settings must be written");
        let project = if manifest {
            let project = selected.join("Cargo.toml");
            let manifest_write = fs::write(&project, "");
            assert_ok!(manifest_write, "An explicit manifest must exist before project-directory discovery");
            project
        } else {
            selected.clone()
        };
        let directory = project_directory(&selected);
        let directory = assert_ok!(directory, "The selected project must have a canonical directory");
        let cli_fixture = CliTestFixture::new(&["--from-scip", "saved.scip"]);
        let mut cli_fixture = assert_ok!(cli_fixture, "Replay arguments must parse independently of project discovery");
        cli_fixture.cli.path = project.clone();

        let system_under_test = LoadedSettings::load(cli_fixture.cli, cli_fixture.matches);
        let system_under_test = assert_ok!(system_under_test, "Selected project or manifest parent settings must load");

        let options = system_under_test.cli.options;
        let paths = [options.export.output, options.analysis.from_scip];
        let paths = paths.map(|path| path.map(|path| path.resolve(&system_under_test.directory)));
        let expected = [Some(directory.join("selected.md")), Some(PathBuf::from("saved.scip"))];
        assert_eq!(system_under_test.cli.path, project, "Discovery must not replace the CLI project selection");
        assert_eq!(options.graph.depth, Some(2), "Discovery read the ancestor or working-directory settings");
        assert_eq!(options.export.format, Some(Format::Mermaid), "Selected project settings did not load");
        assert_eq!(paths, expected, "Only paths from settings must resolve relative to the settings directory");
        assert_eq!(system_under_test.protected_inputs.len(), 1, "The discovered settings input must remain protected");
    }

    #[test]
    fn test_absent_automatic_settings_do_not_search_ancestors() {
        let fixture = SettingsDirectoryTestFixture::new(Some(b"this is malformed TOML"));
        let fixture = assert_ok!(fixture, "A malformed ancestor settings file must be written");
        let selected = fixture.directory.path().join("selected");
        let created = fs::create_dir_all(&selected);
        assert_ok!(created, "The selected directory without settings must be created");
        let cli_fixture = CliTestFixture::new(&["--from-scip", "saved.scip", "--depth", "0"]);
        let mut cli_fixture = assert_ok!(cli_fixture, "Replay with explicit CLI settings must parse");
        cli_fixture.cli.path = selected;

        let system_under_test = LoadedSettings::load(cli_fixture.cli, cli_fixture.matches);
        let system_under_test = assert_ok!(system_under_test, "Absent automatic settings must not read ancestor files");

        let options = system_under_test.cli.options;
        assert_eq!(options.graph.depth, Some(0));
        assert!(options.graph.root.is_none(), "Absent settings must not manufacture an explicit root");
        assert!(options.export.format.is_none(), "Absent settings must leave format defaulting to the command boundary");
        assert!(system_under_test.protected_inputs.is_empty(), "Absent settings must not introduce a protected input");
    }

    #[rstest]
    fn test_explicit_settings_override_discovery(#[values(false, true)] missing_project: bool) {
        let fixture = SettingsDirectoryTestFixture::new(Some(b"malformed automatic settings"));
        let fixture = assert_ok!(fixture, "Malformed automatic settings must exist before explicit selection");
        let directory = fixture.directory.path().join("configuration");
        let created = fs::create_dir_all(&directory);
        assert_ok!(created, "The explicitly selected settings directory must be created");
        let path = directory.join("custom.toml");
        let written = fs::write(&path, "[graph]\ndepth = 4\n[export]\noutput = 'custom.dot'");
        assert_ok!(written, "Explicit settings must be written separately from the automatic file");
        let cli_fixture = CliTestFixture::new(&[]);
        let mut cli_fixture = assert_ok!(cli_fixture, "Explicit settings selection must start with valid arguments");
        let project = fixture.directory.path();
        cli_fixture.cli.path = if missing_project { project.join("missing") } else { project.to_path_buf() };
        cli_fixture.cli.settings = Some(path);

        let system_under_test = LoadedSettings::load(cli_fixture.cli, cli_fixture.matches);
        let system_under_test = assert_ok!(system_under_test, "Explicit settings must bypass automatic discovery");

        let options = system_under_test.cli.options;
        let output = options.export.output.map(|file_path| file_path.resolve(&system_under_test.directory));
        let expected = Some(directory.join("custom.dot"));
        assert_eq!(options.graph.depth, Some(4), "Explicit settings were replaced by automatic configuration");
        assert_eq!(output, expected, "Explicit paths used the project directory");
        assert_eq!(system_under_test.protected_inputs.len(), 1, "Only the explicitly loaded settings file should be protected");
    }

    enum SettingsPathTestSource {
        RelativeFile,
        AbsoluteFile,
        CommandLine,
    }

    #[rstest]
    #[case::diagram(
        "export", "output", "--output",
        |cli: Cli| cli.options.export.output,
    )]
    #[case::folder_cycles(
        "cycles", "cycles-output", "--cycles-output",
        |cli: Cli| cli.options.cycles.cycles_output,
    )]
    #[case::file_cycles(
        "cycles", "file-cycles-output", "--file-cycles-output",
        |cli: Cli| cli.options.cycles.file_cycles_output,
    )]
    #[case::outgoing(
        "outgoing-dependencies", "output", "--outgoing-dependencies-output",
        |cli: Cli| cli.options.outgoing_dependencies.output,
    )]
    #[case::replay(
        "analysis", "from-scip", "--from-scip",
        |cli: Cli| cli.options.analysis.from_scip,
    )]
    #[case::saved_index(
        "analysis", "save-scip", "--save-scip",
        |cli: Cli| cli.options.analysis.save_scip,
    )]
    #[case::analyzer(
        "analysis", "config", "--config",
        |cli: Cli| cli.options.analysis.config,
    )]
    fn test_file_path_resolution_and_cli_precedence(
        #[case] section: &str,
        #[case] key: &str,
        #[case] argument: &str,
        #[case] selected_path: fn(Cli) -> Option<SettingsPath>,
        #[values(SettingsPathTestSource::RelativeFile, SettingsPathTestSource::AbsoluteFile, SettingsPathTestSource::CommandLine)]
        source: SettingsPathTestSource,
    ) {
        let fixture = SettingsDirectoryTestFixture::new(None);
        let fixture = assert_ok!(fixture, "File-path settings fixture directory must be created");
        let configured = match source {
            SettingsPathTestSource::AbsoluteFile => fixture.directory.path().join("absolute-file"),
            SettingsPathTestSource::RelativeFile | SettingsPathTestSource::CommandLine => {
                Path::new("nested").join("from-settings")
            }
        };
        let encoded = serde_json::to_string(&configured);
        let encoded = assert_ok!(encoded, "Fixture paths must be representable as escaped TOML strings");
        let contents = format!("[graph]\nroot = 'src'\nexclude = ['src/generated/**']\n[{section}]\n{key} = {encoded}");
        let written = fs::write(&fixture.settings_path, contents);
        assert_ok!(written, "File-path settings must be written before resolution");
        let arguments: &[&str] = match source {
            SettingsPathTestSource::CommandLine => &[argument, "from-cli"],
            SettingsPathTestSource::RelativeFile | SettingsPathTestSource::AbsoluteFile => &[],
        };
        let cli_fixture = CliTestFixture::new(arguments);
        let mut cli_fixture = assert_ok!(cli_fixture, "CLI file-path overrides must parse");
        cli_fixture.cli.path = fixture.directory.path().join("different-project");
        cli_fixture.cli.settings = Some(fixture.settings_path);

        let system_under_test = LoadedSettings::load(cli_fixture.cli, cli_fixture.matches);
        let system_under_test = assert_ok!(system_under_test, "Path resolution must not require referenced files to exist");

        let expected = match source {
            SettingsPathTestSource::RelativeFile => fixture.directory.path().join("nested").join("from-settings"),
            SettingsPathTestSource::AbsoluteFile => fixture.directory.path().join("absolute-file"),
            SettingsPathTestSource::CommandLine => PathBuf::from("from-cli"),
        };
        let graph = &system_under_test.cli.options.graph;
        let expected_graph = GraphSettings {
            root: Some("src".into()), exclude: vec!["src/generated/**".into()], ..GraphSettings::default()
        };
        assert_eq!(*graph, expected_graph, "Graph roots and exclusion globs must remain index-relative");
        let configured_path = selected_path(system_under_test.cli);
        let resolved_path = configured_path.map(|file_path| file_path.resolve(&system_under_test.directory));
        assert_eq!(resolved_path, Some(expected), "Wrong base for {section}.{key}");
    }

    #[rstest]
    #[case::missing_file(None, false)]
    #[case::malformed_file(Some(b"[graph".as_slice()), false)]
    #[case::non_utf8_file(Some(b"\xff".as_slice()), false)]
    #[case::missing_project(None, true)]
    fn test_no_settings_bypasses_loading(#[case] contents: Option<&[u8]>, #[case] missing_project: bool) {
        let arguments = ["--no-settings", "--depth", "0", "--format", "dot", "--from-scip", "saved.scip"];
        let fixture = SettingsLoadingTestFixture::new(contents, &arguments);
        let mut fixture = assert_ok!(fixture, "Settings bypass test fixture must be prepared");
        if missing_project {
            fixture.cli.path = fixture.directory.path().join("missing");
        }

        let system_under_test = LoadedSettings::load(fixture.cli, fixture.matches);
        let system_under_test = assert_ok!(system_under_test, "Bypass must skip settings discovery and loading");

        let options = system_under_test.cli.options;
        assert_eq!(options.graph.depth, Some(0), "Settings bypass lost explicit CLI values");
        assert_eq!(options.export.format, Some(Format::Dot));
        let from_scip = options.analysis.from_scip.map(|path| path.resolve(&system_under_test.directory));
        assert_eq!(from_scip.as_deref(), Some(Path::new("saved.scip")));
        assert!(system_under_test.protected_inputs.is_empty(), "Bypassed settings must not be retained as protected inputs");
    }

    #[rstest]
    fn test_missing_settings_file(#[values(false, true)] explicit: bool) {
        let fixture = SettingsLoadingTestFixture::new(None, &[]);
        let mut fixture = assert_ok!(fixture, "Missing-settings test fixture must be prepared");
        fixture.cli.settings = explicit.then(|| fixture.settings_path.clone());

        let system_under_test = LoadedSettings::load(fixture.cli, fixture.matches);

        if explicit {
            let error = system_under_test.err();
            let error = assert_some!(error, "Explicitly selected missing settings must fail");
            assert_missing_file_error(&error, &fixture.settings_path);
        } else {
            let loaded = assert_ok!(system_under_test, "Absent automatic settings must be optional");
            let expected = Settings::default();
            assert_eq!(loaded.cli.options, expected, "Absence changed CLI defaults");
            assert!(loaded.protected_inputs.is_empty(), "Missing automatic settings must not retain an input");
        }
    }

    #[rstest]
    #[case::automatic_directory(None, false)]
    #[case::explicit_directory(None, true)]
    #[case::automatic_malformed(Some(b"[graph".as_slice()), false)]
    #[case::automatic_non_utf8(Some(b"\xff".as_slice()), false)]
    #[case::explicit_non_utf8(Some(b"\xff".as_slice()), true)]
    fn test_settings_loading_errors(#[case] contents: Option<&[u8]>, #[case] explicit: bool) {
        let fixture = SettingsLoadingTestFixture::new(contents, &[]);
        let mut fixture = assert_ok!(fixture, "Settings loading-error test fixture must be prepared");
        if contents.is_none() {
            let created = fs::create_dir_all(&fixture.settings_path);
            assert_ok!(created, "Unreadable settings fixture must place a directory at the settings path");
        }
        fixture.cli.settings = explicit.then(|| fixture.settings_path.clone());

        let system_under_test = LoadedSettings::load(fixture.cli, fixture.matches).err();
        let error = assert_some!(system_under_test, "Malformed or unreadable settings must not be treated as absent");
        let diagnostic = error.to_string();

        assert!(diagnostic.contains(SETTINGS_FILE_NAME), "Settings errors must identify the failing file: {diagnostic}");
        if contents.is_none() {
            let cause = error.downcast_ref::<io::Error>();
            let cause = assert_some!(cause, "Unreadable settings must retain their I/O cause");
            assert_ne!(cause.kind(), io::ErrorKind::NotFound, "An existing unreadable settings path was treated as missing");
        }
    }

    #[rstest]
    #[case::empty("", &[], true)]
    #[case::file_threshold("[outgoing-dependencies]\nmaximum = 0", &["--outgoing-dependencies-mode", "files"], true)]
    #[case::file_mode("[outgoing-dependencies]\nmode = 'files'", &["--max-outgoing-dependencies", "0"], true)]
    #[case::file_report("[outgoing-dependencies]\noutput = 'outgoing.md'", &["--max-outgoing-dependencies", "0"], true)]
    #[case::cli_report("[outgoing-dependencies]\nmaximum = 0", &["--outgoing-dependencies-output", "cli.md"], true)]
    #[case::file_complete("[outgoing-dependencies]\nmaximum = 0\nmode = 'folders'\noutput = 'outgoing.md'", &[], true)]
    #[case::missing_file_threshold("[outgoing-dependencies]\nmode = 'files'", &[], false)]
    #[case::missing_report_threshold("[outgoing-dependencies]\noutput = 'outgoing.md'", &[], false)]
    #[case::missing_cli_threshold("", &["--outgoing-dependencies-mode", "files"], false)]
    fn test_outgoing_validation_after_merge(#[case] contents: &str, #[case] arguments: &[&str], #[case] valid: bool) {
        let saved_index_path = saved_index_path_test_fixture().display().to_string();
        let replay_arguments = ["--from-scip", &saved_index_path];
        let arguments: Vec<_> = replay_arguments.into_iter().chain(arguments.iter().copied()).collect();
        let fixture = SettingsLoadingTestFixture::new(Some(contents.as_bytes()), &arguments);
        let mut fixture = assert_ok!(fixture, "Outgoing merge-validation test fixture must be prepared");
        fixture.cli.settings = Some(fixture.settings_path);
        let loaded = LoadedSettings::load(fixture.cli, fixture.matches);
        let loaded = assert_ok!(loaded, "Outgoing settings must load before cross-field validation");

        let system_under_test = loaded.into_command();

        if valid {
            assert_ok!(system_under_test, "Outgoing requirements must be checked against the merged settings and CLI values");
        } else {
            let error = system_under_test.err();
            let error = assert_some!(error, "An outgoing mode or report without a merged threshold must fail");
            let diagnostic = error.to_string();
            assert!(diagnostic.contains("--max-outgoing-dependencies"), "Unexpected validation failure: {diagnostic}");
        }
    }

    #[test]
    fn test_missing_replay_into_command() {
        const INPUT_FILE_NAME: &str = "missing.scip";
        let contents = format!("[analysis]\nfrom-scip = '{INPUT_FILE_NAME}'");
        let fixture = SettingsLoadingTestFixture::new(Some(contents.as_bytes()), &[]);
        let fixture = assert_ok!(fixture, "Missing replay fixture must contain valid settings");
        let loaded = LoadedSettings::load(fixture.cli, fixture.matches);
        let loaded = assert_ok!(loaded, "Loading settings must not require the saved analysis to exist");

        let system_under_test = loaded.into_command();

        let error = system_under_test.err();
        let error = assert_some!(error, "Command construction must propagate a missing saved-analysis error");
        let directory = project_directory(fixture.directory.path());
        let directory = assert_ok!(directory, "Missing replay fixture must have a canonical directory");
        assert_missing_file_error(&error, &directory.join(INPUT_FILE_NAME));
    }

    #[rstest]
    #[case::file_replay(
        "[analysis]\nfrom-scip = 'saved.scip'", &[], None,
    )]
    #[case::cli_replay(
        "[graph]\ndepth = 0", &["--from-scip", &saved_index_path_test_fixture().display().to_string()], None,
    )]
    #[case::filtered_replay(
        "[analysis]\nfrom-scip = 'saved.scip'", &["--root", "src", "--exclude", "src/generated/**"], None,
    )]
    #[case::file_replay_cli_save(
        "[analysis]\nfrom-scip = 'saved.scip'", &["--save-scip", "new.scip"], Some("--save-scip"),
    )]
    #[case::file_save_cli_replay(
        "[analysis]\nsave-scip = 'new.scip'", &["--from-scip", "saved.scip"], Some("--save-scip"),
    )]
    #[case::file_replay_cli_config(
        "[analysis]\nfrom-scip = 'saved.scip'", &["--config", "analyzer.json"], Some("--config"),
    )]
    #[case::file_config_cli_replay(
        "[analysis]\nconfig = 'analyzer.json'", &["--from-scip", "saved.scip"], Some("--config"),
    )]
    #[case::file_replay_save(
        "[analysis]\nfrom-scip = 'saved.scip'\nsave-scip = 'new.scip'", &[], Some("--save-scip"),
    )]
    #[case::file_replay_config(
        "[analysis]\nfrom-scip = 'saved.scip'\nconfig = 'analyzer.json'", &[], Some("--config"),
    )]
    fn test_replay_validation_after_merge(
        #[case] contents: &str,
        #[case] arguments: &[&str],
        #[case] conflict: Option<&str>,
    ) {
        let fixture = SettingsLoadingTestFixture::new(Some(contents.as_bytes()), arguments);
        let fixture = assert_ok!(fixture, "Replay validation must have settings but no project manifest");
        let written = fs::copy(saved_index_path_test_fixture(), fixture.directory.path().join("saved.scip"));
        assert_ok!(written, "Valid saved analysis must be available beside the replay settings");
        let loaded = LoadedSettings::load(fixture.cli, fixture.matches);
        let loaded = assert_ok!(loaded, "Replay settings must load before conflicting options are validated");

        let system_under_test = loaded.into_command();

        if let Some(conflict) = conflict {
            let error = system_under_test.err();
            let error = assert_some!(error, "Merged replay options must reject {conflict}");
            let diagnostic = error.to_string();
            assert!(diagnostic.contains(conflict), "Replay failed for an unexpected reason: {diagnostic}");
        } else {
            assert_ok!(system_under_test, "Saved-index replay settings must load without a project manifest or source files");
        }
    }
}
