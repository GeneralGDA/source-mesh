#[cfg(test)]
mod repository_tests {
    use std::env;
    use std::env::VarError;
    use std::fs;
    use std::path::Component;
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::Command;
    use std::process::Output;
    use std::sync::LazyLock;

    use anyhow::Result;
    use claims::assert_ok;
    use claims::assert_some;
    use pretty_assertions::assert_eq;
    use roxmltree::Document;
    use rstest::fixture;
    use rstest::rstest;
    use tempfile::TempDir;
    use tempfile::tempdir;

    const ENV_UPDATE_EXPECTED_OUTPUTS: &str = "SOURCE_MESH_UPDATE_EXPECTED_OUTPUTS";
    const NAME_REPOSITORY_INDEX: &str = "repository.scip";
    const NAME_CYCLE_REPORT_FOLDER: &str = "folder-cycles.md";
    const NAME_CYCLE_REPORT_FILE: &str = "file-cycles.md";

    struct RepositoryFixture {
        index: Vec<u8>,
        project_analysis_diagram: String,
        update_expected_outputs: bool,
    }

    impl RepositoryFixture {
        fn new() -> Self {
            let update_expected_outputs = env::var(ENV_UPDATE_EXPECTED_OUTPUTS);
            let update_expected_outputs = match update_expected_outputs {
                Ok(value) => Ok(Some(value)),
                Err(VarError::NotPresent) => Ok(None),
                Err(error) => Err(error),
            };
            let update_expected_outputs = assert_ok!(update_expected_outputs, "Expected-output flag must be valid Unicode");
            let update_expected_outputs = update_expected_outputs.is_some_and(|value| {
                assert_eq!(value, "1", "{ENV_UPDATE_EXPECTED_OUTPUTS} must be 1 or unset");
                true
            });
            let directory = tempdir();
            let directory = assert_ok!(directory, "Repository analysis must have a temporary index directory");
            let saved_index = directory.path().join(NAME_REPOSITORY_INDEX);
            let mut system_under_test = Command::new(env!("CARGO_BIN_EXE_source-mesh"));
            system_under_test.current_dir(repository_directory());
            system_under_test.args(["Cargo.toml", "--format", "graphml", "--save-scip"]).arg(&saved_index);

            let output = system_under_test.output();
            let output = assert_ok!(output, "Repository analysis command must launch and collect rust-analyzer output");
            let diagnostics = String::from_utf8_lossy(&output.stderr);

            let backend_markers = ["SCIP", "rust-analyzer", "warning:", "ERROR"];
            let exposes_backend = backend_markers.iter().any(|marker| diagnostics.contains(marker));
            assert!(!exposes_backend, "Normal repository analysis exposed backend diagnostics: {diagnostics}");
            let project_analysis_diagram = successful_stdout(output);
            let index = fs::read(saved_index);
            let index = assert_ok!(index, "Repository analysis must save a readable SCIP index for shared replay");
            let result = directory.close();
            assert_ok!(result, "Repository analysis must clean up its temporary directory after retaining the shared index");

            Self { index, project_analysis_diagram, update_expected_outputs }
        }

        fn check_expected_output(&self, relative_path: &Path, actual: &str) {
            let normal_components = relative_path.components().all(|component| matches!(component, Component::Normal(_)));
            let relative_file_path = relative_path.file_name().is_some() && normal_components;
            assert!(relative_file_path, "Expected output path must be a relative file path with normal components");
            let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("expected").join(relative_path);
            let actual = actual.replace("\r\n", "\n");
            if self.update_expected_outputs {
                let directory = path.parent();
                let directory = assert_some!(directory, "Expected output path must retain its constructed parent directory");
                let directory_creation_result = fs::create_dir_all(directory);
                assert_ok!(directory_creation_result, "Expected-output directory must be created: {}", directory.display());
                let expected_output_write_result = fs::write(&path, &actual);
                assert_ok!(expected_output_write_result, "Expected output must be updated at {}", path.display());
                eprintln!("Updated {}", path.display());
            } else {
                let contents = fs::read_to_string(&path);
                let contents = assert_ok!(contents, "Expected output must be readable at {}", path.display());
                let expected = contents.replace("\r\n", "\n");

                assert_eq!(
                    actual,
                    expected,
                    "Output mismatch at {}. Review changes before regenerating with {ENV_UPDATE_EXPECTED_OUTPUTS}=1.",
                    path.display()
                );
            }
        }
    }

    #[must_use]
    fn repository_directory() -> &'static Path {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).parent().and_then(Path::parent);
        assert_some!(directory, "CLI crate directory must have a repository ancestor")
    }

    fn successful_stdout(output: Output) -> String {
        assert!(
            output.status.success(),
            "CLI failed; stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout);
        assert_ok!(stdout, "Successful repository command must emit UTF-8 on stdout")
    }

    fn replay_fixture(index: &[u8]) -> Result<(TempDir, PathBuf)> {
        let directory = tempdir()?;
        let saved_index = directory.path().join(NAME_REPOSITORY_INDEX);
        fs::write(&saved_index, index)?;
        Ok((directory, saved_index))
    }

    #[fixture]
    #[once]
    fn repository_fixture() -> LazyLock<RepositoryFixture> {
        /* Cache initialization failure too: rstest's OnceLock retries after a panic. */
        LazyLock::new(RepositoryFixture::new)
    }

    #[rstest]
    #[case::including_tests(true)]
    #[case::excluding_tests(false)]
    #[ignore = "slow whole-workspace indexing with rust-analyzer; excluded to keep default tests fast; \
                run Test-All.ps1 from the repository root"]
    fn test_repository_dependency_architecture(repository_fixture: &LazyLock<RepositoryFixture>, #[case] include_tests: bool) {
        let fixture = LazyLock::force(repository_fixture);
        let replay = replay_fixture(&fixture.index);
        let (directory, saved_index) = assert_ok!(replay, "Architecture check must prepare the shared index for replay");
        let maximum_directory_depth = {
            let document = Document::parse(&fixture.project_analysis_diagram);
            let document = assert_ok!(document, "Repository analysis must produce parseable GraphML folder metadata");
            let label_nodes = document.descendants().filter(|node| {
                node.has_tag_name(("http://graphml.graphdrawing.org/xmlns", "data")) && node.attribute("key") == Some("label")
            });
            let labels = label_nodes.filter_map(|node| node.text());
            let depths = labels.map(|label| label.split('/').filter(|part| *part != ".").count());
            let maximum_depth = depths.max();
            assert_some!(maximum_depth, "Repository diagram must contain indexed folders")
        };

        for depth in 0..=maximum_directory_depth {
            let mut system_under_test = Command::new(env!("CARGO_BIN_EXE_source-mesh"));
            system_under_test.current_dir(repository_directory()).arg("--from-scip").arg(&saved_index);
            system_under_test.arg("--depth").arg(depth.to_string()).arg("--fail-on-cycles");
            if !include_tests {
                system_under_test.arg("--exclude-tests");
            }

            let output = system_under_test.output();
            let output = assert_ok!(output, "Repository cycle analysis command must launch and collect output at depth {depth}");

            assert!(
                output.status.success(),
                "Repository dependency architecture failed at depth {depth} (include_tests={include_tests}); \
                 stdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!output.stdout.is_empty(), "Repository cycle analysis did not produce a diagram");
        }

        let result = directory.close();
        assert_ok!(result, "Architecture check must remove its temporary replay directory");
    }

    #[rstest]
    #[case::all_folders("all-folders", &[])]
    #[case::production("production", &["--exclude-tests"])]
    #[case::top_level("top-level", &["--depth", "1", "--exclude-tests"])]
    #[case::grouped("grouped", &["--group-folders"])]
    #[case::grouped_production("grouped-production", &["--group-folders", "--exclude-tests"])]
    #[case::depth_zero("depth-zero", &["--depth", "0", "--include-self"])]
    #[case::source_subtree("source-subtree", &["--root", "crates/source-mesh-core/src", "--exclude-tests"])]
    #[case::source_top_level("source-top-level", &["--root", "crates/source-mesh-core/src", "--depth", "1", "--exclude-tests"])]
    #[case::cycles("cycles", &["--highlight-cycles", "--include-self"])]
    #[case::production_cycles("production-cycles", &["--highlight-cycles", "--include-self", "--exclude-tests"])]
    #[ignore = "slow whole-workspace indexing with rust-analyzer; excluded to keep default tests fast; \
                run Test-All.ps1 from the repository root"]
    fn test_repository_exports(
        repository_fixture: &LazyLock<RepositoryFixture>,
        #[case] scenario: &str,
        #[case] arguments: &[&str],
        #[values("dot", "mermaid", "graphml")] format: &str,
    ) {
        let fixture = LazyLock::force(repository_fixture);
        let replay = replay_fixture(&fixture.index);
        let (directory, saved_index) = assert_ok!(replay, "Export scenario {scenario} must prepare the shared index for replay");
        let diagram_name = format!("diagram.{format}");
        let diagram_path = directory.path().join(&diagram_name);
        let folder_report_path = directory.path().join(NAME_CYCLE_REPORT_FOLDER);
        let file_report_path = directory.path().join(NAME_CYCLE_REPORT_FILE);
        let reports = format == "graphml" && arguments.contains(&"--highlight-cycles");
        let mut system_under_test = Command::new(env!("CARGO_BIN_EXE_source-mesh"));
        system_under_test.current_dir(directory.path()).arg("--from-scip").arg(saved_index);
        system_under_test.args(["--format", format]).args(arguments).arg("--output").arg(&diagram_path);
        if reports {
            system_under_test.arg("--cycles-output").arg(&folder_report_path).arg("--file-cycles-output").arg(&file_report_path);
        }

        let output = system_under_test.output();
        let output = assert_ok!(output, "Repository export must launch and collect output: {scenario}, {format}");
        let stdout = successful_stdout(output);
        let diagram = fs::read_to_string(&diagram_path);
        let diagram = assert_ok!(diagram, "Repository export must create a readable diagram: {scenario}, {format}");
        let directory_entry_count = {
            let entries = fs::read_dir(directory.path());
            let mut entries = assert_ok!(entries, "Export output directory must be readable: {scenario}, {format}");
            let count = entries.try_fold(0_usize, |count, entry| entry.map(|_| count + 1));
            assert_ok!(count, "Every export output entry must be readable")
        };
        let cycle_reports = reports.then(|| {
            let folder_report = fs::read_to_string(folder_report_path);
            let folder_report = assert_ok!(folder_report, "Requested folder cycle report must be readable: {scenario}");
            let file_report = fs::read_to_string(file_report_path);
            let file_report = assert_ok!(file_report, "Requested file cycle report must be readable: {scenario}");
            (folder_report, file_report.replace("&#92;", "/"))
        });

        assert!(stdout.is_empty(), "File export also wrote to stdout");
        assert_eq!(directory_entry_count, if reports { 4 } else { 2 }, "Diagram export created unrequested files");
        if arguments.is_empty() && format == "graphml" {
            let expected = &fixture.project_analysis_diagram;
            assert_eq!(&diagram, expected, "rust-analyzer indexing and saved-index replay produced different diagrams");
        }
        fixture.check_expected_output(&Path::new(scenario).join(diagram_name), &diagram);
        if let Some((folder_report, file_report)) = cycle_reports {
            fixture.check_expected_output(&Path::new(scenario).join(NAME_CYCLE_REPORT_FOLDER), &folder_report);
            fixture.check_expected_output(&Path::new(scenario).join(NAME_CYCLE_REPORT_FILE), &file_report);
        }

        let result = directory.close();
        assert_ok!(result, "Repository export must remove its temporary replay directory: {scenario}, {format}");
    }

    #[rstest]
    #[case::folder("--cycles-output", "report-only", NAME_CYCLE_REPORT_FOLDER)]
    #[case::file("--file-cycles-output", "cycles", NAME_CYCLE_REPORT_FILE)]
    #[ignore = "slow whole-workspace indexing with rust-analyzer; excluded to keep default tests fast; \
                run Test-All.ps1 from the repository root"]
    fn test_repository_report_without_highlighting(
        repository_fixture: &LazyLock<RepositoryFixture>,
        #[case] output_argument: &str,
        #[case] report_scenario: &str,
        #[case] report_name: &str,
    ) {
        let fixture = LazyLock::force(repository_fixture);
        let replay = replay_fixture(&fixture.index);
        let (directory, saved_index) = assert_ok!(replay, "Report-only scenario must prepare the shared index for replay");
        let report_path = directory.path().join(report_name);
        let mut system_under_test = Command::new(env!("CARGO_BIN_EXE_source-mesh"));
        system_under_test.current_dir(directory.path()).arg("--from-scip").arg(saved_index);
        system_under_test.arg(output_argument).arg(&report_path);

        let output = system_under_test.output();
        let output = assert_ok!(output, "Report-only command must launch and collect output for {report_name}");
        let diagram = successful_stdout(output);
        let report = fs::read_to_string(report_path);
        let report = assert_ok!(report, "Report-only export must create a readable {report_name}");
        let directory_entry_count = {
            let entries = fs::read_dir(directory.path());
            let mut entries = assert_ok!(entries, "Report-only output directory must be readable");
            let count = entries.try_fold(0_usize, |count, entry| entry.map(|_| count + 1));
            assert_ok!(count, "Every report-only output entry must be readable")
        };

        assert_eq!(directory_entry_count, 2, "Report-only export created unrequested files");
        fixture.check_expected_output(&Path::new("all-folders").join("diagram.dot"), &diagram);
        fixture.check_expected_output(&Path::new(report_scenario).join(report_name), &report);

        let result = directory.close();
        assert_ok!(result, "Report-only export must remove its temporary replay directory");
    }
}
