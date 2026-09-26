#[cfg(test)]
mod repository_tests {
    #![expect(
        clippy::panic_in_result_fn,
        reason = "Tests use assertions for expectations while returning Result for fallible setup."
    )]

    use std::env;
    use std::env::VarError;
    use std::fs;
    use std::path::Component;
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::Command;
    use std::process::Output;

    use anyhow::Context as _;
    use anyhow::Error;
    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use roxmltree::Document;
    use rstest::fixture;
    use rstest::rstest;
    use tempfile::TempDir;
    use tempfile::tempdir;

    const UPDATE_EXPECTED_OUTPUTS: &str = "SOURCE_MESH_UPDATE_EXPECTED_OUTPUTS";
    const REPOSITORY_INDEX_NAME: &str = "repository.scip";
    const FOLDER_CYCLE_REPORT_NAME: &str = "folder-cycles.md";
    const FILE_CYCLE_REPORT_NAME: &str = "file-cycles.md";

    struct RepositoryFixture {
        index: Vec<u8>,
        live_diagram: String,
        update_expected_outputs: bool,
    }

    impl RepositoryFixture {
        fn new() -> Result<Self> {
            let update_expected_outputs = match env::var(UPDATE_EXPECTED_OUTPUTS) {
                Ok(value) => {
                    if value != "1" {
                        return Err(Error::msg(format!(
                            "{UPDATE_EXPECTED_OUTPUTS} must be 1 or unset"
                        )));
                    }
                    true
                }
                Err(VarError::NotPresent) => false,
                Err(error) => return Err(error.into()),
            };
            let directory = tempdir()?;
            let saved_index = directory.path().join(REPOSITORY_INDEX_NAME);
            let mut system_under_test = Command::new(env!("CARGO_BIN_EXE_source-mesh"));
            system_under_test.current_dir(repository_directory())
                .args(["Cargo.toml", "--format", "graphml", "--save-scip"]).arg(&saved_index);

            let output = system_under_test.output().context("Cannot run live repository analysis")?;
            let diagnostics = String::from_utf8_lossy(&output.stderr);

            assert!(!["SCIP", "rust-analyzer", "warning:", "ERROR"].iter().any(|marker| diagnostics.contains(marker)),
                "Normal repository analysis exposed backend diagnostics: {diagnostics}");
            let live_diagram = successful_stdout(output)?;
            let index = fs::read(saved_index)?;
            directory.close()?;

            Ok(Self { index, live_diagram, update_expected_outputs })
        }

        fn check_expected_output(&self, relative_path: &Path, actual: &str) -> Result<()> {
            assert!(
                relative_path.file_name().is_some()
                    && relative_path
                        .components()
                        .all(|component| matches!(component, Component::Normal(_))),
                "Expected output path must be a relative file path with normal components"
            );
            let path = repository_directory().join("tests").join("expected").join(relative_path);
            let actual = actual.replace("\r\n", "\n");
            if self.update_expected_outputs {
                fs::create_dir_all(path.parent().context("Expected output path has no parent directory")?)?;
                fs::write(&path, &actual)?;
                eprintln!("Updated {}", path.display());
            } else {
                let expected = fs::read_to_string(&path)
                    .with_context(|| format!("Cannot read expected output {}", path.display()))?
                    .replace("\r\n", "\n");

                assert_eq!(
                    actual,
                    expected,
                    "Output mismatch at {}. Review changes before regenerating with {UPDATE_EXPECTED_OUTPUTS}=1.",
                    path.display()
                );
            }
            Ok(())
        }
    }

    #[must_use]
    fn repository_directory() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    fn successful_stdout(output: Output) -> Result<String> {
        assert!(
            output.status.success(),
            "CLI failed; stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).context("CLI stdout is not UTF-8")
    }

    fn replay_fixture(index: &[u8]) -> Result<(TempDir, PathBuf)> {
        let directory = tempdir()?;
        let saved_index = directory.path().join(REPOSITORY_INDEX_NAME);
        fs::write(&saved_index, index)?;
        Ok((directory, saved_index))
    }

    #[fixture]
    #[once]
    fn repository_fixture() -> Result<RepositoryFixture, String> {
        RepositoryFixture::new().map_err(|error| {
            eprintln!("{error:#}");
            "Live repository analysis failed; see fixture initialization diagnostics".to_owned()
        })
    }

    #[rstest]
    #[case::including_tests(true)]
    #[case::excluding_tests(false)]
    #[ignore = "indexes the current repository with rust-analyzer; run explicitly in release mode"]
    fn test_repository_dependency_architecture(
        repository_fixture: &Result<RepositoryFixture, String>,
        #[case] include_tests: bool,
    ) -> Result<()> {
        let fixture = repository_fixture.as_ref().map_err(|error| Error::msg(error.clone()))?;
        let (directory, saved_index) = replay_fixture(&fixture.index)?;
        let maximum_directory_depth = {
            let document = Document::parse(&fixture.live_diagram)
                .context("Cannot read indexed repository folders from GraphML")?;
            document.descendants()
                .filter(|node| node.has_tag_name(("http://graphml.graphdrawing.org/xmlns", "data"))
                    && node.attribute("key") == Some("label"))
                .filter_map(|node| node.text())
                .map(|label| label.split('/').filter(|part| *part != ".").count())
                .max().context("Repository diagram has no indexed folders")?
        };

        for depth in 0..=maximum_directory_depth {
            let mut system_under_test = Command::new(env!("CARGO_BIN_EXE_source-mesh"));
            system_under_test.current_dir(repository_directory())
                .arg("--from-scip").arg(&saved_index)
                .arg("--depth").arg(depth.to_string())
                .arg("--fail-on-cycles");
            if !include_tests {
                system_under_test.arg("--exclude-tests");
            }

            let output = system_under_test.output()
                .with_context(|| format!("Cannot run repository cycle analysis at depth {depth}"))?;

            assert!(
                output.status.success(),
                "Repository dependency architecture failed at depth {depth} (include_tests={include_tests}); stdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                !output.stdout.is_empty(),
                "Repository cycle analysis did not produce a diagram"
            );
        }

        directory.close()?;
        Ok(())
    }

    #[rstest]
    #[case::all_folders("all-folders", &[])]
    #[case::production("production", &["--exclude-tests"])]
    #[case::top_level("top-level", &["--depth", "1", "--exclude-tests"])]
    #[case::grouped("grouped", &["--group-folders"])]
    #[case::grouped_production("grouped-production", &["--group-folders", "--exclude-tests"])]
    #[case::depth_zero("depth-zero", &["--depth", "0", "--include-self"])]
    #[case::source_subtree("source-subtree", &["--root", "src", "--exclude-tests"])]
    #[case::source_top_level("source-top-level", &["--root", "src", "--depth", "1", "--exclude-tests"])]
    #[case::cycles("cycles", &["--highlight-cycles", "--include-self"])]
    #[case::production_cycles("production-cycles", &["--highlight-cycles", "--include-self", "--exclude-tests"])]
    #[ignore = "indexes the current repository with rust-analyzer; run explicitly in release mode"]
    fn test_repository_exports(
        repository_fixture: &Result<RepositoryFixture, String>,
        #[case] scenario: &str,
        #[case] arguments: &[&str],
        #[values("dot", "mermaid", "graphml")] format: &str,
    ) -> Result<()> {
        let fixture = repository_fixture.as_ref().map_err(|error| Error::msg(error.clone()))?;
        let (directory, saved_index) = replay_fixture(&fixture.index)?;
        let diagram_name = format!("diagram.{format}");
        let diagram_path = directory.path().join(&diagram_name);
        let folder_report_path = directory.path().join(FOLDER_CYCLE_REPORT_NAME);
        let file_report_path = directory.path().join(FILE_CYCLE_REPORT_NAME);
        let reports = format == "graphml" && arguments.contains(&"--highlight-cycles");
        let mut system_under_test = Command::new(env!("CARGO_BIN_EXE_source-mesh"));
        system_under_test.current_dir(directory.path())
            .arg("--from-scip").arg(saved_index)
            .args(["--format", format]).args(arguments)
            .arg("--output").arg(&diagram_path);
        if reports {
            system_under_test.arg("--cycles-output").arg(&folder_report_path)
                .arg("--file-cycles-output").arg(&file_report_path);
        }

        let output = system_under_test.output()?;
        let stdout = successful_stdout(output)?;
        let diagram = fs::read_to_string(&diagram_path)?;
        let directory_entry_count = fs::read_dir(directory.path())?
            .try_fold(0_usize, |count, entry| entry.map(|_| count + 1))?;
        let cycle_reports = reports
            .then(|| -> Result<_> {
                let folder_report = fs::read_to_string(folder_report_path)?;
                let file_report = fs::read_to_string(file_report_path)?.replace("&#92;", "/");
                Ok((folder_report, file_report))
            })
            .transpose()?;

        assert!(stdout.is_empty(), "File export also wrote to stdout");
        assert_eq!(directory_entry_count, if reports { 4 } else { 2 }, "Diagram export created unrequested files");
        if arguments.is_empty() && format == "graphml" {
            assert_eq!(diagram, fixture.live_diagram, "Live indexing and replay produced different diagrams");
        }
        fixture.check_expected_output(&Path::new(scenario).join(diagram_name), &diagram)?;
        if let Some((folder_report, file_report)) = cycle_reports {
            fixture.check_expected_output(&Path::new(scenario).join(FOLDER_CYCLE_REPORT_NAME), &folder_report)?;
            fixture.check_expected_output(&Path::new(scenario).join(FILE_CYCLE_REPORT_NAME), &file_report)?;
        }

        directory.close()?;
        Ok(())
    }

    #[rstest]
    #[case::folder("--cycles-output", "report-only", FOLDER_CYCLE_REPORT_NAME)]
    #[case::file("--file-cycles-output", "cycles", FILE_CYCLE_REPORT_NAME)]
    #[ignore = "indexes the current repository with rust-analyzer; run explicitly in release mode"]
    fn test_repository_report_without_highlighting(
        repository_fixture: &Result<RepositoryFixture, String>,
        #[case] output_argument: &str,
        #[case] report_scenario: &str,
        #[case] report_name: &str,
    ) -> Result<()> {
        let fixture = repository_fixture.as_ref().map_err(|error| Error::msg(error.clone()))?;
        let (directory, saved_index) = replay_fixture(&fixture.index)?;
        let report_path = directory.path().join(report_name);
        let mut system_under_test = Command::new(env!("CARGO_BIN_EXE_source-mesh"));
        system_under_test.current_dir(directory.path())
            .arg("--from-scip").arg(saved_index)
            .arg(output_argument).arg(&report_path);

        let output = system_under_test.output()?;
        let diagram = successful_stdout(output)?;
        let report = fs::read_to_string(report_path)?;
        let directory_entry_count = fs::read_dir(directory.path())?
            .try_fold(0_usize, |count, entry| entry.map(|_| count + 1))?;

        assert_eq!(directory_entry_count, 2, "Report-only export created unrequested files");
        fixture.check_expected_output(&Path::new("all-folders").join("diagram.dot"), &diagram)?;
        fixture.check_expected_output(&Path::new(report_scenario).join(report_name), &report)?;

        directory.close()?;
        Ok(())
    }
}
