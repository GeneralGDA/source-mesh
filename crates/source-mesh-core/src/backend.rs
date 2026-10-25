use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use derive_new::new;
use protobuf::Message as _;
use serde_json::Map;
use serde_json::Value;

use crate::analyzer::config::without_test_cfg;
use crate::analyzer::options::Options as AnalyzerOptions;
use crate::analyzer::parallel::try_pair;
use crate::analyzer::prerequisites;
use crate::analyzer::process::Phase;
use crate::analyzer::process::ScipProcess;
use crate::analyzer::repair::IndexRepair;
use crate::diagnostics::emit;
use crate::scip::index::ParsedIndex;
use crate::scip::test_dependencies::mark_test_references;

const NAME_CARGO_MANIFEST: &str = "Cargo.toml";
const NAME_RUST_PROJECT_FILE: &str = "rust-project.json";
const MESSAGE_PROJECT_INPUT_ERROR: &str = "Expected a project directory or Cargo.toml";
const MESSAGE_SCIP_READ_ERROR: &str = "Cannot read SCIP index";

pub trait IndexSource {
    fn index(&self) -> Result<Vec<u8>>;
}

#[must_use]
pub fn contains_project_file(directory: &Path) -> bool {
    directory.join(NAME_CARGO_MANIFEST).is_file() || directory.join(NAME_RUST_PROJECT_FILE).is_file()
}

#[derive(new)]
pub struct RustAnalyzer<'a> {
    project: &'a Path,
    configuration_path_original: Option<&'a Path>,
    options: &'a AnalyzerOptions,
}

impl IndexSource for RustAnalyzer<'_> {
    fn index(&self) -> Result<Vec<u8>> {
        self.index_parsed()?.into_index().write_to_bytes().context("Cannot serialize classified SCIP index")
    }
}

impl RustAnalyzer<'_> {
    pub(crate) fn index_parsed(&self) -> Result<ParsedIndex> {
        let project = resolve_project_directory(self.project)?;
        prerequisites::ensure_available(&project)?;
        let index_directory = tempfile::tempdir().context("Cannot create temporary index directory")?;
        let index_file_original = index_directory.path().join("original.scip");
        let index_file_without_test = index_directory.path().join("without_test.scip");
        let configuration_path_original = self.configuration_path_original.map(dunce::canonicalize).transpose();
        let configuration_path_original = configuration_path_original.context("Cannot resolve original rust-analyzer config")?;
        let configuration_original = configuration_path_original.as_ref().map_or_else(
            || Ok(Value::Object(Map::default())),
            |path| -> Result<Value> {
                let contents = fs::read(path).context("Cannot read original rust-analyzer config")?;
                serde_json::from_slice(&contents).context("Cannot parse original rust-analyzer config")
            },
        )?;
        let configuration_path_without_test = index_directory.path().join("without_test.json");
        let configuration_without_test = serde_json::to_vec(&without_test_cfg(configuration_original)?)?;
        let configuration_write_without_test = fs::write(&configuration_path_without_test, configuration_without_test);
        configuration_write_without_test.context("Cannot write analysis configuration without cfg(test)")?;
        let analyzer_original = ScipProcess::new(&project, configuration_path_original.as_deref(), self.options);
        let analyzer_without_test = ScipProcess::new(&project, Some(&configuration_path_without_test), self.options);
        let indexing_started = Instant::now();
        emit(format_args!("Analyzing Rust dependencies..."));
        if self.options.verbose() {
            emit(format_args!("Indexing original cfg and without cfg(test) in parallel (Cargo may serialize shared builds)..."));
        }
        let (index_original, index_without_test) = try_pair(
            || {
                let index = analyzer_original.generate_index(&index_file_original, Phase::Original)?;
                ParsedIndex::parse(&index).context("Cannot decode original dependency index")
            },
            || {
                let index = analyzer_without_test.generate_index(&index_file_without_test, Phase::WithoutTest)?;
                ParsedIndex::parse(&index).context("Cannot decode dependency index without cfg(test)")
            },
        )?;
        if self.options.verbose() {
            emit(format_args!("SCIP processes completed in {:.2}s.", indexing_started.elapsed().as_secs_f64()));
        }
        let repair = IndexRepair::new(
            &project, configuration_path_original.as_deref(), &configuration_path_without_test, self.options,
        );
        let (index_original, index_without_test) = repair.perform(index_original, index_without_test)?;
        if self.options.verbose() {
            emit(format_args!("Comparing SCIP references to classify test dependencies..."));
        }
        let classification_started = Instant::now();
        let classified = mark_test_references(index_original, &index_without_test)?;
        if self.options.verbose() {
            emit(format_args!("Test classification completed in {:.2}s.", classification_started.elapsed().as_secs_f64()));
        }
        emit(format_args!("Dependency analysis completed in {:.2}s.", indexing_started.elapsed().as_secs_f64()));
        Ok(classified)
    }
}

fn resolve_project_directory(project: &Path) -> Result<PathBuf> {
    let metadata = fs::metadata(project).with_context(|| format!("Cannot inspect Rust project path {}", project.display()))?;
    let directory = if metadata.is_dir() {
        project
    } else {
        ensure!(
            project.file_name().is_some_and(|name| name == NAME_CARGO_MANIFEST),
            "{MESSAGE_PROJECT_INPUT_ERROR}: {}", project.display()
        );
        project.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."))
    };
    let directory = dunce::canonicalize(directory).with_context(|| {
        format!("Cannot resolve Rust project directory {}", directory.display())
    })?;
    let manifest = directory.join(NAME_CARGO_MANIFEST);
    match fs::read(&manifest) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let project_file = directory.join(NAME_RUST_PROJECT_FILE);
            fs::read(&project_file).with_context(|| {
                format!(
                    "Cannot read project file {} after {NAME_CARGO_MANIFEST} was not found in {}",
                    project_file.display(),
                    directory.display()
                )
            })?;
        }
        Err(error) => {
            return Err(error).with_context(|| format!("Cannot read project manifest {}", manifest.display()));
        }
    }
    Ok(directory)
}

#[derive(new)]
pub struct ScipFile {
    path: PathBuf,
}

impl IndexSource for ScipFile {
    fn index(&self) -> Result<Vec<u8>> {
        fs::read(&self.path).with_context(|| format!("{MESSAGE_SCIP_READ_ERROR} {}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::path::PathBuf;

    use claims::assert_ok;
    use claims::assert_some;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use tempfile::tempdir;

    use super::NAME_CARGO_MANIFEST;
    use super::IndexSource as _;
    use super::MESSAGE_PROJECT_INPUT_ERROR;
    use super::NAME_RUST_PROJECT_FILE;
    use super::RustAnalyzer;
    use super::MESSAGE_SCIP_READ_ERROR;
    use super::ScipFile;
    use super::contains_project_file;
    use super::resolve_project_directory;
    use crate::SourcePath;
    use crate::analyzer::options::Options;
    use crate::model::DependencyKind;
    use crate::model::FileEdge;
    use crate::model::FileGraph;
    use crate::model::FileNode;
    use crate::scip::backend::file_graph;
    use crate::scip::backend::parse_scip;

    #[rstest]
    #[case::empty(&[], &[], false)]
    #[case::cargo(&[NAME_CARGO_MANIFEST], &[], true)]
    #[case::rust_project(&[NAME_RUST_PROJECT_FILE], &[], true)]
    #[case::both(&[NAME_CARGO_MANIFEST, NAME_RUST_PROJECT_FILE], &[], true)]
    #[case::unrelated(&["source.rs"], &[], false)]
    #[case::directories(&[], &[NAME_CARGO_MANIFEST, NAME_RUST_PROJECT_FILE], false)]
    #[case::cargo_directory(&[NAME_RUST_PROJECT_FILE], &[NAME_CARGO_MANIFEST], true)]
    fn test_project_file_detection(#[case] files: &[&str], #[case] directories: &[&str], #[case] expected: bool) {
        let fixture_creation = tempdir();
        let fixture = assert_ok!(fixture_creation, "The project-file detection fixture directory must be created");
        for filename in files {
            let file_write = fs::write(fixture.path().join(filename), "");
            assert_ok!(file_write, "The project-file detection fixture must write {filename}");
        }
        for directory in directories {
            let directory_creation = fs::create_dir_all(fixture.path().join(directory));
            assert_ok!(directory_creation, "The project-file detection fixture must create directory {directory}");
        }

        let system_under_test = contains_project_file(fixture.path());

        assert_eq!(system_under_test, expected, "Project detection must distinguish supported files from directories");
    }

    #[rstest]
    #[case::missing_project(false)]
    #[case::missing_project_files(true)]
    fn test_rust_analyzer_index_project_validation(#[case] create_project: bool) {
        let fixture_creation = tempdir();
        let fixture = assert_ok!(fixture_creation, "The analyzer project-validation fixture directory must be created");
        let project = fixture.path().join("project");
        if create_project {
            let project_creation = fs::create_dir_all(&project);
            assert_ok!(project_creation, "The project fixture without project files must be created");
        }
        let options = Options::new(false);
        let system_under_test = RustAnalyzer::new(&project, None, &options);

        let index_generation = system_under_test.index();

        assert!(
            index_generation.as_ref().is_err_and(|error| error.to_string().contains(&project.display().to_string())),
            "IndexSource::index must validate the project before producing bytes"
        );
        assert!(
            index_generation.is_err_and(|error| {
                error.downcast_ref::<io::Error>().is_some_and(|cause| cause.kind() == io::ErrorKind::NotFound)
            }),
            "IndexSource::index lost the project filesystem error"
        );
    }

    #[rstest]
    #[case::binary(&[0, 1, 255, 128, 13, 10, 42])]
    #[case::empty(&[])]
    fn test_scip_file_bytes(#[case] bytes: &[u8]) {
        let fixture_creation = tempdir();
        let fixture = assert_ok!(fixture_creation, "The SCIP byte-preservation fixture directory must be created");
        let path = fixture.path().join("input.scip");
        let index_write = fs::write(&path, bytes);
        assert_ok!(index_write, "The SCIP fixture bytes must be written before reading the index");
        let system_under_test = ScipFile::new(path);

        let index_read = system_under_test.index();
        let index = assert_ok!(index_read, "The file adapter must read the SCIP fixture bytes");

        assert_eq!(index, bytes, "The file adapter must preserve bytes without decoding or rewriting them");
    }

    #[rstest]
    #[case::missing(false)]
    #[case::directory(true)]
    fn test_scip_file_read_failure(#[case] unreadable_directory: bool) {
        let fixture_creation = tempdir();
        let fixture = assert_ok!(fixture_creation, "The SCIP read-failure fixture directory must be created");
        let path = fixture.path().join("input.scip");
        if unreadable_directory {
            let directory_creation = fs::create_dir_all(&path);
            assert_ok!(directory_creation, "The unreadable SCIP fixture must contain a directory at the index path");
        }
        let expected_message = format!("{MESSAGE_SCIP_READ_ERROR} {}", path.display());
        let system_under_test = ScipFile::new(path);

        let index_read = system_under_test.index();

        let error = index_read.as_ref().err();
        let error = assert_some!(error, "A missing SCIP file or directory at the index path must fail to read");
        assert_eq!(error.to_string(), expected_message, "SCIP read errors must identify the requested index path");
        let cause = error.downcast_ref::<io::Error>();
        let cause = assert_some!(cause, "SCIP read errors must retain their filesystem cause");
        assert_eq!(cause.kind() == io::ErrorKind::NotFound, !unreadable_directory, "Missing and unreadable indexes must differ");
    }

    #[rstest]
    #[case::directory(false)]
    #[case::manifest(true)]
    fn test_project_directory_selection(#[case] manifest_input: bool) {
        let (system_under_test, expected) = {
            let fixture_creation = tempdir();
            let fixture = assert_ok!(fixture_creation, "The project-directory selection fixture must be created");
            let manifest = fixture.path().join(NAME_CARGO_MANIFEST);
            let manifest_write = fs::write(&manifest, "");
            assert_ok!(manifest_write, "The project-directory selection fixture must contain a Cargo manifest");
            let project = if manifest_input { &manifest } else { fixture.path() };
            let project_resolution = resolve_project_directory(project);
            let directory = assert_ok!(project_resolution, "A project directory or its manifest must resolve successfully");
            let fixture_resolution = dunce::canonicalize(fixture.path());
            let expected = assert_ok!(fixture_resolution, "The selected project fixture must have a canonical path");
            (directory, expected)
        };

        assert_eq!(system_under_test, expected, "Project input must select its canonical directory");
    }

    #[test]
    fn test_project_path_io_error() {
        let system_under_test = {
            let fixture_creation = tempdir();
            let fixture = assert_ok!(fixture_creation, "The missing-project-path fixture directory must be created");
            resolve_project_directory(&fixture.path().join("missing"))
        };

        assert!(
            system_under_test.is_err_and(|error| {
                error.downcast_ref::<io::Error>().is_some_and(|cause| cause.kind() == io::ErrorKind::NotFound)
            }),
            "Missing project path lost its filesystem error"
        );
    }

    #[test]
    fn test_rust_project_file() {
        let (system_under_test, expected) = {
            let fixture_creation = tempdir();
            let fixture = assert_ok!(fixture_creation, "The rust-project.json fixture directory must be created");
            let project_file_write = fs::write(fixture.path().join(NAME_RUST_PROJECT_FILE), "{}");
            assert_ok!(project_file_write, "The rust-project.json fixture must be written before project resolution");
            let project_resolution = resolve_project_directory(fixture.path());
            let directory = assert_ok!(project_resolution, "A rust-project.json project must resolve without a manifest");
            let fixture_resolution = dunce::canonicalize(fixture.path());
            let expected = assert_ok!(fixture_resolution, "The rust-project.json fixture must have a canonical path");
            (directory, expected)
        };

        assert_eq!(system_under_test, expected, "A rust-project.json project must select its canonical directory");
    }

    #[rstest]
    #[case::manifest(NAME_CARGO_MANIFEST)]
    #[case::rust_project(NAME_RUST_PROJECT_FILE)]
    fn test_project_file_read_error(#[case] filename: &str) {
        let system_under_test = {
            let fixture_creation = tempdir();
            let fixture = assert_ok!(fixture_creation, "The unreadable project-file fixture directory must be created");
            let directory_creation = fs::create_dir_all(fixture.path().join(filename));
            assert_ok!(directory_creation, "The project fixture must contain a directory at the {filename} file path");
            resolve_project_directory(fixture.path())
        };

        assert!(
            system_under_test.is_err_and(|error| {
                error.downcast_ref::<io::Error>().is_some_and(|cause| cause.kind() != io::ErrorKind::NotFound)
            }),
            "An unreadable project file was reported as a missing file"
        );
    }

    #[test]
    fn test_missing_project_files() {
        let system_under_test = {
            let fixture_creation = tempdir();
            let fixture = assert_ok!(fixture_creation, "The project fixture without project files must be created");
            resolve_project_directory(fixture.path())
        };

        assert!(
            system_under_test.is_err_and(|error| {
                error.downcast_ref::<io::Error>().is_some_and(|cause| cause.kind() == io::ErrorKind::NotFound)
            }),
            "A directory without project files lost its missing-file error"
        );
    }

    #[test]
    fn test_non_manifest_project_file() {
        let system_under_test = {
            let fixture_creation = tempdir();
            let fixture = assert_ok!(fixture_creation, "The non-manifest project-file fixture directory must be created");
            let project = fixture.path().join("source.rs");
            let source_write = fs::write(&project, "");
            assert_ok!(source_write, "The non-manifest source file must exist before project resolution");
            resolve_project_directory(&project)
        };

        assert!(
            system_under_test.is_err_and(|error| error.to_string().contains(MESSAGE_PROJECT_INPUT_ERROR)),
            "A non-manifest project file was accepted"
        );
    }

    #[must_use]
    fn analyzer_fixture() -> (PathBuf, Options) {
        (PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src").join("fixtures"), Options::new(false))
    }

    #[test]
    #[ignore = "slow multi-pass rust-analyzer indexing; excluded to keep default tests fast; \
                run Test-All.ps1 from the repository root"]
    fn test_rust_analyzer_generated_method_dependencies() {
        let (fixture, options) = analyzer_fixture();
        let project = fixture.join("generated-methods");
        let source_directory = Path::new("src");
        let client_path_result = SourcePath::new(source_directory.join("client.rs"));
        let client_path = assert_ok!(client_path_result, "The generated-method caller must have a valid source path");
        let client = FileNode::new(client_path);
        let factory_path_result = SourcePath::new(source_directory.join("factory.rs"));
        let factory_path = assert_ok!(factory_path_result, "The generated-method factory must have a valid source path");
        let factory = FileNode::new(factory_path);
        let model_path_result = SourcePath::new(source_directory.join("model.rs"));
        let model_path = assert_ok!(model_path_result, "The generated-method model must have a valid source path");
        let model = FileNode::new(model_path);
        let validation_path_result = SourcePath::new(source_directory.join("validation.rs"));
        let validation_path = assert_ok!(validation_path_result, "The generated-method test caller path must be valid");
        let validation = FileNode::new(validation_path);
        let expected = BTreeSet::from([
            FileEdge::new(client.clone(), factory.clone(), DependencyKind::Production),
            FileEdge::new(client, model.clone(), DependencyKind::Production),
            FileEdge::new(factory.clone(), model.clone(), DependencyKind::Production),
            FileEdge::new(validation.clone(), factory, DependencyKind::Test),
            FileEdge::new(validation, model, DependencyKind::Test),
        ]);
        let system_under_test = RustAnalyzer::new(&project, None, &options);

        let index_generation = system_under_test.index_parsed();
        let index = assert_ok!(index_generation, "rust-analyzer must index the generated-method fixture");
        let graph_build = file_graph(&index);
        let graph = assert_ok!(graph_build, "The generated-method index must produce a file dependency graph");

        assert_eq!(
            graph.edges(),
            &expected,
            "Calls to generated accessors must retain their defining file and test classification"
        );
    }

    #[test]
    #[ignore = "slow multi-pass rust-analyzer indexing; excluded to keep default tests fast; \
                run Test-All.ps1 from the repository root"]
    fn test_rust_analyzer_cargo_target_collisions() {
        let (fixture, options) = analyzer_fixture();
        let project = fixture.join("cargo-targets");
        let library_path_result = SourcePath::new(Path::new("src").join("library.rs"));
        let library_path = assert_ok!(library_path_result, "The shared Cargo library must have a valid source path");
        let library = FileNode::new(library_path);
        let first_binary_path_result = SourcePath::new(Path::new("src").join("bin").join("first.rs"));
        let first_binary_path = assert_ok!(first_binary_path_result, "The first Cargo binary path must be valid");
        let second_binary_path_result = SourcePath::new(Path::new("src").join("bin").join("second.rs"));
        let second_binary_path = assert_ok!(second_binary_path_result, "The second Cargo binary path must be valid");
        let expected = BTreeSet::from([
            FileEdge::new(FileNode::new(first_binary_path), library.clone(), DependencyKind::Production),
            FileEdge::new(FileNode::new(second_binary_path), library, DependencyKind::Production),
        ]);
        let system_under_test = RustAnalyzer::new(&project, None, &options);

        let index_generation = system_under_test.index_parsed();
        let index = assert_ok!(index_generation, "rust-analyzer must index the colliding Cargo targets");
        let graph_build = file_graph(&index);
        let graph = assert_ok!(graph_build, "The Cargo-target index must produce a file dependency graph");

        assert_eq!(
            graph.edges(),
            &expected,
            "Cargo target collisions must not create links between binaries or hide library calls"
        );
    }
    #[test]
    #[ignore = "slow multi-pass rust-analyzer indexing; excluded to keep default tests fast; \
                run Test-All.ps1 from the repository root"]
    fn test_rust_analyzer_manifest_file_graph() {
        let (fixture, options) = analyzer_fixture();
        let manifest = fixture.join("dependency-graph").join(NAME_CARGO_MANIFEST);
        let expected = {
            let fixture_read = ScipFile::new(fixture.join("dependency-graph.scip")).index();
            let expected_index = assert_ok!(fixture_read, "The checked-in dependency-graph SCIP fixture must be readable");
            let fixture_parse = parse_scip(&expected_index);
            assert_ok!(fixture_parse, "The checked-in SCIP fixture must produce the expected dependency graph")
        };
        let system_under_test = RustAnalyzer::new(&manifest, None, &options);

        let index_generation = system_under_test.index();
        let index = assert_ok!(index_generation, "rust-analyzer must index a project selected by its Cargo manifest");
        let index_parse = parse_scip(&index);
        let graph = assert_ok!(index_parse, "The analyzer index bytes must parse into a file dependency graph");

        assert_eq!(graph.files(), expected.files(), "rust-analyzer indexing changed fixture files");
        assert_eq!(graph.edges(), expected.edges(), "rust-analyzer indexing changed fixture dependencies");
    }

    #[test]
    #[ignore = "slow multi-pass rust-analyzer indexing; excluded to keep default tests fast; \
                run Test-All.ps1 from the repository root"]
    fn test_rust_analyzer_test_configuration_classification() {
        let (fixture, options) = analyzer_fixture();
        let manifest = fixture.join("test-dependencies").join(NAME_CARGO_MANIFEST);
        let source_directory = Path::new("src");
        let module_file = "mod.rs";
        let client_path_result = SourcePath::new(source_directory.join("client").join("calls.rs"));
        let client_path = assert_ok!(client_path_result, "The classified fixture caller must have a valid source path");
        let client = FileNode::new(client_path);
        let shared_path_result = SourcePath::new(source_directory.join("shared").join("value.rs"));
        let shared_path = assert_ok!(shared_path_result, "The classified fixture shared value must have a valid path");
        let shared = FileNode::new(shared_path);
        let test_only_path_result = SourcePath::new(source_directory.join("test_only").join("value.rs"));
        let test_only_path = assert_ok!(test_only_path_result, "The classified fixture test-only value path must be valid");
        let test_only = FileNode::new(test_only_path);
        let validation_path_result = SourcePath::new(source_directory.join("validation").join("checks.rs"));
        let validation_path = assert_ok!(validation_path_result, "The classified fixture validation path must be valid");
        let validation = FileNode::new(validation_path);
        let library_path_result = SourcePath::new(source_directory.join("lib.rs"));
        let library = assert_ok!(library_path_result, "The classified fixture library must have a valid source path");
        let client_module_result = SourcePath::new(source_directory.join("client").join(module_file));
        let client_module = assert_ok!(client_module_result, "The classified fixture client module path must be valid");
        let shared_module_result = SourcePath::new(source_directory.join("shared").join(module_file));
        let shared_module = assert_ok!(shared_module_result, "The classified fixture shared module path must be valid");
        let test_only_module_result = SourcePath::new(source_directory.join("test_only").join(module_file));
        let test_only_module = assert_ok!(test_only_module_result, "The classified fixture test-only module path must be valid");
        let expected_graph_build = FileGraph::new(
            BTreeSet::from([
                FileNode::new(library),
                FileNode::new(client_module),
                FileNode::new(shared_module),
                FileNode::new(test_only_module),
                client.clone(),
                shared.clone(),
                test_only.clone(),
                validation.clone(),
            ]),
            BTreeSet::from([
                FileEdge::new(client.clone(), shared.clone(), DependencyKind::Production),
                FileEdge::new(client.clone(), shared, DependencyKind::Test),
                FileEdge::new(client, test_only.clone(), DependencyKind::Test),
                FileEdge::new(validation, test_only, DependencyKind::Test),
            ]),
        );
        let expected = assert_ok!(expected_graph_build, "Expected classified dependencies must form a valid file graph");
        let system_under_test = RustAnalyzer::new(&manifest, None, &options);

        let index_generation = system_under_test.index_parsed();
        let index = assert_ok!(index_generation, "rust-analyzer must index the test-configuration fixture");
        let graph_build = file_graph(&index);
        let graph = assert_ok!(graph_build, "The classified analyzer index must produce a file dependency graph");

        assert_eq!(graph.files(), expected.files(), "rust-analyzer analysis changed classified fixture files");
        assert_eq!(graph.edges(), expected.edges(), "rust-analyzer analysis changed classified fixture dependencies");
    }
}
