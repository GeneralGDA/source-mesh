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

use crate::analyzer_config::without_test_cfg;
use crate::analyzer_options::Options as AnalyzerOptions;
use crate::analyzer_prerequisites;
use crate::analyzer_process::Phase;
use crate::analyzer_process::ScipProcess;
use crate::diagnostics::emit;
use crate::parallel::try_pair;
use crate::scip_index::ParsedIndex;
use crate::scip_repair::IndexRepair;
use crate::test_dependencies::mark_test_references;

const CARGO_MANIFEST: &str = "Cargo.toml";
const RUST_PROJECT_FILE: &str = "rust-project.json";
const PROJECT_INPUT_ERROR: &str = "Expected a project directory or Cargo.toml";

pub trait IndexSource {
    fn index(&self) -> Result<Vec<u8>>;
}

#[must_use]
pub fn contains_project_file(directory: &Path) -> bool {
    directory.join(CARGO_MANIFEST).is_file() || directory.join(RUST_PROJECT_FILE).is_file()
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
        analyzer_prerequisites::ensure_available(&project)?;
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
        ensure!(project.file_name().is_some_and(|name| name == CARGO_MANIFEST), "{PROJECT_INPUT_ERROR}: {}", project.display());
        project.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."))
    };
    let directory = dunce::canonicalize(directory).with_context(|| {
        format!("Cannot resolve Rust project directory {}", directory.display())
    })?;
    let manifest = directory.join(CARGO_MANIFEST);
    match fs::read(&manifest) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let project_file = directory.join(RUST_PROJECT_FILE);
            fs::read(&project_file).with_context(|| {
                format!(
                    "Cannot read project file {} after {CARGO_MANIFEST} was not found in {}",
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
        fs::read(&self.path).with_context(|| format!("Cannot read SCIP index {}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::path::PathBuf;

    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use tempfile::tempdir;

    use super::CARGO_MANIFEST;
    use super::IndexSource as _;
    use super::PROJECT_INPUT_ERROR;
    use super::RUST_PROJECT_FILE;
    use super::RustAnalyzer;
    use super::ScipFile;
    use super::resolve_project_directory;
    use crate::SourcePath;
    use crate::analyzer_options::Options;
    use crate::model::DependencyKind;
    use crate::model::FileEdge;
    use crate::model::FileGraph;
    use crate::model::FileNode;
    use crate::scip_backend::file_graph;
    use crate::scip_backend::parse_scip;

    #[rstest]
    #[case::directory(false)]
    #[case::manifest(true)]
    fn test_project_directory_selection(#[case] manifest_input: bool) {
        let system_under_test = (|| -> Result<_> {
            let fixture = tempdir()?;
            let manifest = fixture.path().join(CARGO_MANIFEST);
            fs::write(&manifest, "")?;
            let project = if manifest_input { &manifest } else { fixture.path() };
            let directory = resolve_project_directory(project)?;
            Ok((directory, dunce::canonicalize(fixture.path())?))
        })();

        assert!(
            system_under_test.is_ok_and(|(directory, expected)| directory == expected),
            "Project input did not select its directory"
        );
    }

    #[test]
    fn test_project_path_io_error() {
        let system_under_test = (|| -> Result<_> {
            let fixture = tempdir()?;
            resolve_project_directory(&fixture.path().join("missing"))
        })();

        assert!(
            system_under_test.is_err_and(|error| {
                error.downcast_ref::<io::Error>().is_some_and(|cause| cause.kind() == io::ErrorKind::NotFound)
            }),
            "Missing project path lost its filesystem error"
        );
    }

    #[test]
    fn test_rust_project_file() {
        let system_under_test = (|| -> Result<_> {
            let fixture = tempdir()?;
            fs::write(fixture.path().join(RUST_PROJECT_FILE), "{}")?;
            let directory = resolve_project_directory(fixture.path())?;
            Ok((directory, dunce::canonicalize(fixture.path())?))
        })();

        assert!(
            system_under_test.is_ok_and(|(directory, expected)| directory == expected),
            "A rust-project.json project was rejected"
        );
    }

    #[rstest]
    #[case::manifest(CARGO_MANIFEST)]
    #[case::rust_project(RUST_PROJECT_FILE)]
    fn test_project_file_read_error(#[case] filename: &str) {
        let system_under_test = (|| -> Result<_> {
            let fixture = tempdir()?;
            fs::create_dir_all(fixture.path().join(filename))?;
            resolve_project_directory(fixture.path())
        })();

        assert!(
            system_under_test.is_err_and(|error| {
                error.downcast_ref::<io::Error>().is_some_and(|cause| cause.kind() != io::ErrorKind::NotFound)
            }),
            "An unreadable project file was reported as a missing file"
        );
    }

    #[test]
    fn test_missing_project_files() {
        let system_under_test = (|| -> Result<_> {
            let fixture = tempdir()?;
            resolve_project_directory(fixture.path())
        })();

        assert!(
            system_under_test.is_err_and(|error| {
                error.downcast_ref::<io::Error>().is_some_and(|cause| cause.kind() == io::ErrorKind::NotFound)
            }),
            "A directory without project files lost its missing-file error"
        );
    }

    #[test]
    fn test_non_manifest_project_file() {
        let system_under_test = (|| -> Result<_> {
            let fixture = tempdir()?;
            let project = fixture.path().join("source.rs");
            fs::write(&project, "")?;
            resolve_project_directory(&project)
        })();

        assert!(
            system_under_test.is_err_and(|error| error.to_string().contains(PROJECT_INPUT_ERROR)),
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
    fn test_rust_analyzer_generated_method_dependencies() -> Result<()> {
        let (fixture, options) = analyzer_fixture();
        let project = fixture.join("generated-methods");
        let source_directory = Path::new("src");
        let client = FileNode::new(SourcePath::new(source_directory.join("client.rs"))?);
        let factory = FileNode::new(SourcePath::new(source_directory.join("factory.rs"))?);
        let model = FileNode::new(SourcePath::new(source_directory.join("model.rs"))?);
        let validation = FileNode::new(SourcePath::new(source_directory.join("validation.rs"))?);
        let expected = BTreeSet::from([
            FileEdge::new(client.clone(), factory.clone(), DependencyKind::Production),
            FileEdge::new(client, model.clone(), DependencyKind::Production),
            FileEdge::new(factory.clone(), model.clone(), DependencyKind::Production),
            FileEdge::new(validation.clone(), factory, DependencyKind::Test),
            FileEdge::new(validation, model, DependencyKind::Test),
        ]);
        let system_under_test = RustAnalyzer::new(&project, None, &options);

        let index = system_under_test.index_parsed()?;
        let graph = file_graph(&index)?;

        assert_eq!(
            graph.edges(),
            &expected,
            "Calls to generated accessors must retain their defining file and test classification"
        );
        Ok(())
    }

    #[test]
    #[ignore = "slow multi-pass rust-analyzer indexing; excluded to keep default tests fast; \
                run Test-All.ps1 from the repository root"]
    fn test_rust_analyzer_cargo_target_collisions() -> Result<()> {
        let (fixture, options) = analyzer_fixture();
        let project = fixture.join("cargo-targets");
        let library = FileNode::new(SourcePath::new(Path::new("src").join("library.rs"))?);
        let expected = BTreeSet::from([
            FileEdge::new(
                FileNode::new(SourcePath::new(Path::new("src").join("bin").join("first.rs"))?),
                library.clone(),
                DependencyKind::Production,
            ),
            FileEdge::new(
                FileNode::new(SourcePath::new(Path::new("src").join("bin").join("second.rs"))?),
                library,
                DependencyKind::Production,
            ),
        ]);
        let system_under_test = RustAnalyzer::new(&project, None, &options);

        let index = system_under_test.index_parsed()?;
        let graph = file_graph(&index)?;

        assert_eq!(
            graph.edges(),
            &expected,
            "Cargo target collisions must not create links between binaries or hide library calls"
        );
        Ok(())
    }
    #[test]
    #[ignore = "slow multi-pass rust-analyzer indexing; excluded to keep default tests fast; \
                run Test-All.ps1 from the repository root"]
    fn test_rust_analyzer_manifest_file_graph() -> Result<()> {
        let (fixture, options) = analyzer_fixture();
        let manifest = fixture.join("dependency-graph").join(CARGO_MANIFEST);
        let expected = parse_scip(&ScipFile::new(fixture.join("dependency-graph.scip")).index()?)?;
        let system_under_test = RustAnalyzer::new(&manifest, None, &options);

        let index = system_under_test.index()?;
        let graph = parse_scip(&index)?;

        assert_eq!(graph.files(), expected.files(), "rust-analyzer indexing changed fixture files");
        assert_eq!(graph.edges(), expected.edges(), "rust-analyzer indexing changed fixture dependencies");
        Ok(())
    }

    #[test]
    #[ignore = "slow multi-pass rust-analyzer indexing; excluded to keep default tests fast; \
                run Test-All.ps1 from the repository root"]
    fn test_rust_analyzer_test_configuration_classification() -> Result<()> {
        let (fixture, options) = analyzer_fixture();
        let manifest = fixture.join("test-dependencies").join(CARGO_MANIFEST);
        let source_directory = Path::new("src");
        let module_file = "mod.rs";
        let client = FileNode::new(SourcePath::new(source_directory.join("client").join("calls.rs"))?);
        let shared = FileNode::new(SourcePath::new(source_directory.join("shared").join("value.rs"))?);
        let test_only = FileNode::new(SourcePath::new(source_directory.join("test_only").join("value.rs"))?);
        let validation = FileNode::new(SourcePath::new(source_directory.join("validation").join("checks.rs"))?);
        let expected = FileGraph::new(
            BTreeSet::from([
                FileNode::new(SourcePath::new(source_directory.join("lib.rs"))?),
                FileNode::new(SourcePath::new(source_directory.join("client").join(module_file))?),
                FileNode::new(SourcePath::new(source_directory.join("shared").join(module_file))?),
                FileNode::new(SourcePath::new(source_directory.join("test_only").join(module_file))?),
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
        )?;
        let system_under_test = RustAnalyzer::new(&manifest, None, &options);

        let index = system_under_test.index_parsed()?;
        let graph = file_graph(&index)?;

        assert_eq!(graph.files(), expected.files(), "rust-analyzer analysis changed classified fixture files");
        assert_eq!(graph.edges(), expected.edges(), "rust-analyzer analysis changed classified fixture dependencies");
        Ok(())
    }
}
