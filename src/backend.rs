use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::thread;
use std::time::Instant;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::ensure;
use derive_new::new;
use serde_json::Map;
use serde_json::Value;

use crate::analyzer_config::without_test_cfg;
use crate::analyzer_options::Options as AnalyzerOptions;
use crate::analyzer_process::Phase;
use crate::analyzer_process::ScipProcess;
use crate::scip_repair::IndexRepair;
use crate::test_dependencies::mark_test_references;

pub trait IndexSource {
    fn index(&self) -> Result<Vec<u8>>;
}

#[must_use]
pub fn contains_project_file(directory: &Path) -> bool {
    directory.join("Cargo.toml").is_file() || directory.join("rust-project.json").is_file()
}

#[derive(new)]
pub struct RustAnalyzer<'a> {
    project: &'a Path,
    configuration_path: Option<&'a Path>,
    options: &'a AnalyzerOptions,
}

impl IndexSource for RustAnalyzer<'_> {
    fn index(&self) -> Result<Vec<u8>> {
        let project = if self.project.is_file() {
            ensure!(
                self.project
                    .file_name()
                    .is_some_and(|name| name == "Cargo.toml"),
                "Expected a project directory or Cargo.toml"
            );
            self.project
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."))
        } else {
            self.project
        };
        let project =
            dunce::canonicalize(project).context("Cannot resolve Rust project directory")?;
        ensure!(
            contains_project_file(&project),
            "No Cargo.toml or rust-project.json in {}",
            project.display()
        );
        let index_directory =
            tempfile::tempdir().context("Cannot create temporary index directory")?;
        let index_file = index_directory.path().join("index.scip");
        let production_index_file = index_directory.path().join("production.scip");
        let configuration_path = self
            .configuration_path
            .map(dunce::canonicalize)
            .transpose()
            .context("Cannot resolve rust-analyzer config")?;
        let configuration = configuration_path.as_ref().map_or_else(
            || Ok(Value::Object(Map::default())),
            |path| -> Result<Value> {
                serde_json::from_slice(
                    &fs::read(path).context("Cannot read rust-analyzer config")?,
                ).context("Cannot parse rust-analyzer config")
            },
        )?;
        let production_configuration_path = index_directory.path().join("production.json");
        fs::write(
            &production_configuration_path,
            serde_json::to_vec(&without_test_cfg(configuration)?)?,
        ).context("Cannot write non-test analysis configuration")?;
        let configured_analyzer =
            ScipProcess::new(&project, configuration_path.as_deref(), self.options);
        let production_analyzer =
            ScipProcess::new(&project, Some(&production_configuration_path), self.options);
        let indexing_started = Instant::now();
        eprintln!("Analyzing Rust dependencies...");
        if self.options.verbose() {
            eprintln!("Indexing configured and non-test cfg in parallel (Cargo may serialize shared build work)...");
        }
        let (configured_result, production_result) = thread::scope(|scope| {
            let configured_pass =
                scope.spawn(|| configured_analyzer.generate_index(&index_file, Phase::Configured));
            let production_pass =
                scope.spawn(|| production_analyzer.generate_index(&production_index_file, Phase::Production));
            (configured_pass.join(), production_pass.join())
        });
        let index_bytes = configured_result.map_err(|_| anyhow!("Configured SCIP pass panicked"))??;
        let production_bytes = production_result.map_err(|_| anyhow!("Non-test SCIP pass panicked"))??;
        if self.options.verbose() {
            eprintln!("SCIP processes completed in {:.2}s.", indexing_started.elapsed().as_secs_f64());
        }
        let (index_bytes, production_bytes) = IndexRepair::new(
            &project,
            configuration_path.as_deref(),
            &production_configuration_path,
            self.options,
        ).repair(index_bytes, production_bytes)?;
        if self.options.verbose() {
            eprintln!("Comparing SCIP references to classify test dependencies...");
        }
        let classification_started = Instant::now();
        let classified = mark_test_references(&index_bytes, &production_bytes)?;
        if self.options.verbose() {
            eprintln!("Test classification completed in {:.2}s.", classification_started.elapsed().as_secs_f64());
        }
        eprintln!("Dependency analysis completed in {:.2}s.", indexing_started.elapsed().as_secs_f64());
        Ok(classified)
    }
}

#[derive(new)]
pub struct ScipFile {
    path: PathBuf,
}

impl IndexSource for ScipFile {
    fn index(&self) -> Result<Vec<u8>> {
        fs::read(&self.path)
            .with_context(|| format!("Cannot read SCIP index {}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::path::PathBuf;

    use anyhow::Result;
    use pretty_assertions::assert_eq;

    use super::IndexSource as _;
    use super::RustAnalyzer;
    use super::ScipFile;
    use crate::analyzer_options::Options;
    use crate::model::DependencyKind;
    use crate::model::FileEdge;
    use crate::model::FileGraph;
    use crate::model::FileNode;
    use crate::scip_backend::parse_scip;

    #[must_use]
    fn analyzer_fixture() -> (PathBuf, Options) {
        (
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("fixtures"),
            Options::new(false),
        )
    }

    #[test]
    #[ignore = "requires rust-analyzer and the Rust toolchain components"]
    fn test_live_generated_method_dependencies() -> Result<()> {
        let (fixture, options) = analyzer_fixture();
        let project = fixture.join("generated-methods");
        let source_directory = Path::new("src");
        let client = FileNode::new(source_directory.join("client.rs"))?;
        let factory = FileNode::new(source_directory.join("factory.rs"))?;
        let model = FileNode::new(source_directory.join("model.rs"))?;
        let validation = FileNode::new(source_directory.join("validation.rs"))?;
        let expected = BTreeSet::from([
            FileEdge::new(client.clone(), factory.clone(), DependencyKind::Production),
            FileEdge::new(client, model.clone(), DependencyKind::Production),
            FileEdge::new(factory.clone(), model.clone(), DependencyKind::Production),
            FileEdge::new(validation.clone(), factory, DependencyKind::Test),
            FileEdge::new(validation, model, DependencyKind::Test),
        ]);
        let system_under_test = RustAnalyzer::new(&project, None, &options);

        let index = system_under_test.index()?;
        let graph = parse_scip(&index)?;

        assert_eq!(graph.edges(), &expected, "Calls to generated accessors must retain their defining file and test classification");
        Ok(())
    }

    #[test]
    #[ignore = "requires rust-analyzer and the Rust toolchain components"]
    fn test_live_cargo_target_collisions() -> Result<()> {
        let (fixture, options) = analyzer_fixture();
        let project = fixture.join("cargo-targets");
        let library = FileNode::new(Path::new("src").join("library.rs"))?;
        let expected = BTreeSet::from([
            FileEdge::new(FileNode::new(Path::new("src").join("bin").join("first.rs"))?, library.clone(), DependencyKind::Production),
            FileEdge::new(FileNode::new(Path::new("src").join("bin").join("second.rs"))?, library, DependencyKind::Production),
        ]);
        let system_under_test = RustAnalyzer::new(&project, None, &options);

        let index = system_under_test.index()?;
        let graph = parse_scip(&index)?;

        assert_eq!(graph.edges(), &expected, "Cargo target collisions must not create links between binaries or hide library calls");
        Ok(())
    }
    #[test]
    #[ignore = "requires rust-analyzer and the Rust toolchain components"]
    fn test_live_manifest_file_graph() -> Result<()> {
        let (fixture, options) = analyzer_fixture();
        let manifest = fixture.join("dependency-graph").join("Cargo.toml");
        let expected = parse_scip(&ScipFile::new(fixture.join("dependency-graph.scip")).index()?)?;
        let system_under_test = RustAnalyzer::new(&manifest, None, &options);

        let index = system_under_test.index()?;
        let graph = parse_scip(&index)?;

        assert_eq!(
            graph.files(),
            expected.files(),
            "Live indexing changed fixture files"
        );
        assert_eq!(
            graph.edges(),
            expected.edges(),
            "Live indexing changed fixture dependencies"
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires rust-analyzer and the Rust toolchain components"]
    fn test_live_test_configuration_classification() -> Result<()> {
        let (fixture, options) = analyzer_fixture();
        let manifest = fixture.join("test-dependencies").join("Cargo.toml");
        let source_directory = Path::new("src");
        let module_file = "mod.rs";
        let client = FileNode::new(source_directory.join("client").join("calls.rs"))?;
        let shared = FileNode::new(source_directory.join("shared").join("value.rs"))?;
        let test_only = FileNode::new(source_directory.join("test_only").join("value.rs"))?;
        let validation = FileNode::new(source_directory.join("validation").join("checks.rs"))?;
        let expected = FileGraph::new(
            BTreeSet::from([
                FileNode::new(source_directory.join("lib.rs"))?,
                FileNode::new(source_directory.join("client").join(module_file))?,
                FileNode::new(source_directory.join("shared").join(module_file))?,
                FileNode::new(source_directory.join("test_only").join(module_file))?,
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

        let index = system_under_test.index()?;
        let graph = parse_scip(&index)?;

        assert_eq!(
            graph.files(),
            expected.files(),
            "Live analysis changed classified fixture files"
        );
        assert_eq!(
            graph.edges(),
            expected.edges(),
            "Live analysis changed classified fixture dependencies"
        );
        Ok(())
    }
}
