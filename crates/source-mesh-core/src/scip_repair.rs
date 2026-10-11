use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::thread;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::ensure;
use derive_new::new;
use protobuf::Message as _;
use scip::symbol::format_symbol;
use scip::types::Descriptor;
use scip::types::Index;
use scip::types::SymbolInformation;
use scip::types::SymbolRole;
use scip::types::descriptor::Suffix;

use crate::analyzer_definitions::DefinitionResolver;
use crate::analyzer_options::Options;
use crate::model::FileNode;
use crate::scip_definitions::decode_symbol;
use crate::scip_definitions::definitions;
use crate::scip_definitions::unresolved_symbols;
use crate::scip_path::source_path;

#[derive(new)]
pub(crate) struct IndexRepair<'a> {
    project_directory: &'a Path,
    configured_configuration_path: Option<&'a Path>,
    production_configuration_path: &'a Path,
    options: &'a Options,
}

impl IndexRepair<'_> {
    pub(crate) fn repair(&self, configured_bytes: Vec<u8>, production_bytes: Vec<u8>) -> Result<(Vec<u8>, Vec<u8>)> {
        let configured = Index::parse_from_bytes(&configured_bytes).context("Cannot decode configured dependency index")?;
        let production = Index::parse_from_bytes(&production_bytes).context("Cannot decode non-test dependency index")?;
        let mut repair_symbols = colliding_symbols(&configured, &production)?;
        repair_symbols.extend(unresolved_symbols(&configured)?);
        repair_symbols.extend(unresolved_symbols(&production)?);
        if repair_symbols.is_empty() {
            return Ok((configured_bytes, production_bytes));
        }
        let repair_pass = |index, configuration_path| -> Result<Vec<u8>> {
            let mut resolver = None;
            let repaired = repair_occurrences(index, &repair_symbols, |reference| {
                let resolver = if let Some(ref mut resolver) = resolver {
                    resolver
                } else {
                    if self.options.verbose() {
                        eprintln!("Resolving missing or ambiguous definitions with rust-analyzer...");
                    }
                    resolver.insert(DefinitionResolver::start(self.project_directory, configuration_path, self.options.verbose())?)
                };
                let locations = resolver
                    .definitions(&self.project_directory.join(reference.source.path()), reference.line, reference.column)?;
                let files = locations.iter().map(|location| {
                    dunce::canonicalize(location.file())
                        .with_context(|| format!("Cannot resolve definition at {}:{}:{}", location.file().display(), u64::from(*location.line()) + 1, u64::from(*location.column()) + 1))
                }).collect::<Result<BTreeSet<_>>>()?;
                definition_target(self.project_directory, &files, reference.candidates)
            })?;
            repaired.write_to_bytes().context("Cannot serialize repaired dependency index")
        };
        thread::scope(|scope| {
            let configured = scope.spawn(|| repair_pass(configured, self.configured_configuration_path));
            let production = scope.spawn(|| repair_pass(production, Some(self.production_configuration_path)));
            let configured = configured.join().map_err(|_| anyhow!("Configured symbol resolution panicked"))?;
            let production = production.join().map_err(|_| anyhow!("Non-test symbol resolution panicked"))?;
            Ok((configured?, production?))
        })
    }
}

fn definition_target(project: &Path, files: &BTreeSet<PathBuf>, candidates: &BTreeSet<FileNode>) -> Result<Option<FileNode>> {
    ensure!(!files.is_empty(), "The analyzer could not locate the referenced definition");
    let mut project_files = files.iter().filter_map(|file| file.strip_prefix(project).ok())
        .map(|file| FileNode::new(file.to_owned())).collect::<Result<BTreeSet<_>>>()?;
    ensure!(project_files.len() <= 1,
        "The analyzer returned multiple project definition files: {}",
        project_files.iter().map(|file| file.path().display().to_string()).collect::<Vec<_>>().join(", "));
    ensure!(!project_files.is_empty() || candidates.is_empty(), "A known project definition resolved outside the analyzed project");
    Ok(project_files.pop_first())
}
fn colliding_symbols(configured: &Index, production: &Index) -> Result<BTreeSet<String>> {
    let mut combined = definitions(configured)?;
    for (symbol, files) in definitions(production)? {
        combined.entry(symbol).or_default().extend(files);
    }
    Ok(combined.into_iter().filter(|entry| entry.1.len() > 1)
        .map(|(symbol, _)| symbol.to_owned()).collect())
}

struct UnresolvedReference<'a> {
    source: &'a FileNode,
    line: u32,
    column: u32,
    candidates: &'a BTreeSet<FileNode>,
}

fn repair_occurrences(
    mut index: Index,
    repair_symbols: &BTreeSet<String>,
    mut resolve: impl FnMut(&UnresolvedReference<'_>) -> Result<Option<FileNode>>,
) -> Result<Index> {
    let candidates: BTreeMap<_, _> = definitions(&index)?.into_iter()
        .filter(|&(symbol, _)| repair_symbols.contains(symbol))
        .map(|(symbol, files)| (symbol.to_owned(), files)).collect();
    let mut recovered: BTreeMap<FileNode, BTreeSet<String>> = BTreeMap::new();
    let no_candidates = BTreeSet::new();
    for document in &mut index.documents {
        let source = source_path(&document.relative_path)?;
        for occurrence in &mut document.occurrences {
            if !repair_symbols.contains(&occurrence.symbol) { continue; }
            let targets = candidates.get(&occurrence.symbol).unwrap_or(&no_candidates);
            let target = if occurrence.symbol_roles & SymbolRole::Definition as i32 != 0 {
                source.clone()
            } else if targets.len() == 1 {
                targets.first().context("Definition has no source file")?.clone()
            } else {
                let (line, column) = match occurrence.range.as_slice() {
                    &[line, column, ..] if occurrence.range.len() == 3 || occurrence.range.len() == 4 =>
                        (u32::try_from(line)?, u32::try_from(column)?),
                    _ => return Err(anyhow!("Invalid reference range in {}: {:?}", source.path().display(), occurrence.range)),
                };
                let resolved = resolve(&UnresolvedReference { source: &source, line, column, candidates: targets })
                    .with_context(|| format!("Cannot determine dependency at {}:{}:{}", source.path().display(), line + 1, column + 1))?;
                let Some(target) = resolved else {
                    ensure!(targets.is_empty(), "A known project definition cannot resolve to an external file");
                    continue;
                };
                target
            };
            ensure!(targets.is_empty() || targets.contains(&target), "Definition resolver returned a file outside the indexed candidates");
            occurrence.symbol = qualified_symbol(&occurrence.symbol, &target)?;
            recovered.entry(target).or_default().insert(occurrence.symbol.clone());
        }
        for information in &mut document.symbols {
            if candidates.get(&information.symbol).is_some_and(|files| files.contains(&source)) {
                information.symbol = qualified_symbol(&information.symbol, &source)?;
            }
        }
    }
    for document in &mut index.documents {
        if let Some(symbols) = recovered.remove(&source_path(&document.relative_path)?) {
            for symbol in symbols {
                if !document.symbols.iter().any(|information| information.symbol == symbol) {
                    document.symbols.push(SymbolInformation { symbol, ..SymbolInformation::default() });
                }
            }
        }
    }
    ensure!(recovered.is_empty(), "A dependency resolves to a source file absent from the analysis: {}",
        recovered.keys().map(|file| file.path().display().to_string()).collect::<Vec<_>>().join(", "));
    Ok(index)
}

fn qualified_symbol(symbol: &str, file: &FileNode) -> Result<String> {
    let mut parsed = decode_symbol(symbol)?;
    let path = file.path().components().map(|part| part.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
    parsed.descriptors.insert(0, Descriptor {
        name: format!("source-mesh:{path}"),
        suffix: Suffix::Namespace.into(),
        ..Descriptor::default()
    });
    Ok(format_symbol(parsed))
}
#[cfg(test)]
#[expect(clippy::panic_in_result_fn, reason = "Tests return Result for fallible setup and use assertions for expectations.")]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use protobuf::Message as _;
    use rstest::rstest;
    use scip::types::Document;
    use scip::types::Index;
    use scip::types::Occurrence;
    use scip::types::SymbolRole;

    use super::colliding_symbols;
    use super::definition_target;
    use super::repair_occurrences;
    use super::UnresolvedReference;
    use crate::model::DependencyKind;
    use crate::model::FileEdge;
    use crate::model::FileNode;
    use crate::scip_backend::parse_scip;
    use crate::test_dependencies::mark_test_references;

    #[must_use]
    fn index(symbol: &str, files: &[(&str, &[i32])]) -> Index {
        Index { documents: files.iter().map(|&(path, roles)| Document {
            relative_path: path.into(),
            occurrences: roles.iter().map(|&symbol_roles| Occurrence {
                symbol: symbol.into(), symbol_roles, range: vec![0, 0, 3], ..Occurrence::default()
            }).collect(),
            ..Document::default()
        }).collect(), ..Index::default() }
    }

    fn unexpected_definition_resolution(_: &UnresolvedReference<'_>) -> Result<Option<FileNode>> {
        unreachable!("Unexpected definition resolver call")
    }

    #[rstest]
    #[case::same_file("binary.rs", "binary.rs")]
    #[case::library_call("binary.rs", "library.rs")]
    fn test_semantic_resolution(#[case] caller: &str, #[case] target: &str) -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let fixture = index(symbol, &[("library.rs", &[1]), (caller, &[1, 0])]);
        let target = FileNode::new(PathBuf::from(target))?;
        let collisions = colliding_symbols(&fixture, &fixture)?;
        let source = FileNode::new(PathBuf::from(caller))?;
        let expected = if source == target { BTreeSet::new() } else {
            BTreeSet::from([FileEdge::new(source, target.clone(), DependencyKind::Production)])
        };

        let system_under_test = repair_occurrences(fixture, &collisions, |_| Ok(Some(target.clone())))?;
        let graph = parse_scip(&system_under_test.write_to_bytes()?)?;

        assert_eq!(graph.edges(), &expected);
        Ok(())
    }

    #[test]
    fn test_unreferenced_collisions() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 main().";
        let fixture = index(symbol, &[("first.rs", &[1]), ("second.rs", &[1])]);
        let collisions = colliding_symbols(&fixture, &fixture)?;

        let system_under_test = repair_occurrences(fixture, &collisions, unexpected_definition_resolution)?;
        let graph = parse_scip(&system_under_test.write_to_bytes()?)?;

        assert!(graph.edges().is_empty());
        assert_eq!(graph.files().len(), 2);
        Ok(())
    }

    #[test]
    fn test_shared_qualification_test_classification() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let configured = index(symbol, &[("first.rs", &[1]), ("second.rs", &[1]), ("caller.rs", &[0])]);
        let production = index(symbol, &[("first.rs", &[1]), ("caller.rs", &[0])]);
        let collisions = colliding_symbols(&configured, &production)?;
        let target = FileNode::new(PathBuf::from("first.rs"))?;
        let expected_dependencies = BTreeSet::from([FileEdge::new(
            FileNode::new(PathBuf::from("caller.rs"))?, target.clone(), DependencyKind::Production,
        )]);

        let configured = repair_occurrences(configured, &collisions, |_| Ok(Some(target.clone())))?;
        let production = repair_occurrences(production, &collisions, unexpected_definition_resolution)?;
        let classified = mark_test_references(&configured.write_to_bytes()?, &production.write_to_bytes()?)?;
        let system_under_test = parse_scip(&classified)?;

        assert_eq!(system_under_test.edges(), &expected_dependencies);
        Ok(())
    }

    #[test]
    fn test_configuration_specific_target_identity() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let configured = index(symbol, &[("test.rs", &[1]), ("caller.rs", &[0])]);
        let production = index(symbol, &[("production.rs", &[1]), ("caller.rs", &[0])]);
        let collisions = colliding_symbols(&configured, &production)?;

        let configured = repair_occurrences(configured, &collisions, unexpected_definition_resolution)?;
        let production = repair_occurrences(production, &collisions, unexpected_definition_resolution)?;
        let classified = mark_test_references(&configured.write_to_bytes()?, &production.write_to_bytes()?)?;
        let system_under_test = Index::parse_from_bytes(&classified)?;

        assert!(system_under_test.documents.iter().flat_map(|document| &document.occurrences)
            .filter(|occurrence| occurrence.symbol_roles & SymbolRole::Definition as i32 == 0)
            .all(|occurrence| occurrence.symbol_roles & SymbolRole::Test as i32 != 0));
        Ok(())
    }

    #[test]
    fn test_outside_candidate_rejection() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let fixture = index(symbol, &[("first.rs", &[1]), ("second.rs", &[1]), ("caller.rs", &[0])]);
        let collisions = colliding_symbols(&fixture, &fixture)?;

        let system_under_test = repair_occurrences(fixture, &collisions, |_| FileNode::new(PathBuf::from("outside.rs")).map(Some));

        assert!(system_under_test.is_err());
        Ok(())
    }

    #[test]
    fn test_generated_definition_recovery_and_offline_replay() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 model/impl#[Value]get().";
        let fixture = index(symbol, &[("model.rs", &[]), ("client.rs", &[0])]);
        let repair_symbols = BTreeSet::from([symbol.to_owned()]);
        let target = FileNode::new(PathBuf::from("model.rs"))?;
        let expected_dependencies = BTreeSet::from([FileEdge::new(
            FileNode::new(PathBuf::from("client.rs"))?, target.clone(), DependencyKind::Production,
        )]);

        let repaired = repair_occurrences(fixture, &repair_symbols, |_| Ok(Some(target.clone())))?;
        let system_under_test = parse_scip(&repaired.write_to_bytes()?)?;

        assert_eq!(system_under_test.edges(), &expected_dependencies);
        assert!(repaired.documents.iter().flat_map(|document| &document.occurrences)
            .all(|occurrence| occurrence.symbol_roles & SymbolRole::Definition as i32 == 0),
            "Recovering ownership must not invent declaration occurrences");
        Ok(())
    }

    #[test]
    fn test_generated_definition_classification_with_different_export_metadata() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 model/impl#[Value]get().";
        let configured = index(symbol, &[("model.rs", &[1]), ("client.rs", &[0]), ("validation.rs", &[0])]);
        let production = index(symbol, &[("model.rs", &[]), ("client.rs", &[0])]);
        let repair_symbols = BTreeSet::from([symbol.to_owned()]);
        let target = FileNode::new(PathBuf::from("model.rs"))?;
        let expected_dependencies = BTreeSet::from([
            FileEdge::new(FileNode::new(PathBuf::from("client.rs"))?, target.clone(), DependencyKind::Production),
            FileEdge::new(FileNode::new(PathBuf::from("validation.rs"))?, target.clone(), DependencyKind::Test),
        ]);

        let configured = repair_occurrences(configured, &repair_symbols, unexpected_definition_resolution)?;
        let production = repair_occurrences(production, &repair_symbols, |_| Ok(Some(target.clone())))?;
        let classified = mark_test_references(&configured.write_to_bytes()?, &production.write_to_bytes()?)?;
        let system_under_test = parse_scip(&classified)?;

        assert_eq!(system_under_test.edges(), &expected_dependencies);
        Ok(())
    }

    #[test]
    fn test_external_definition_resolution() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 external().";
        let fixture = index(symbol, &[("client.rs", &[0])]);
        let repair_symbols = BTreeSet::from([symbol.to_owned()]);

        let repaired = repair_occurrences(fixture, &repair_symbols, |_| Ok(None))?;
        let system_under_test = parse_scip(&repaired.write_to_bytes()?)?;

        assert!(system_under_test.edges().is_empty());
        Ok(())
    }

    #[test]
    fn test_missing_definition_document() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 get().";
        let fixture = index(symbol, &[("client.rs", &[0])]);
        let repair_symbols = BTreeSet::from([symbol.to_owned()]);

        let system_under_test = repair_occurrences(fixture, &repair_symbols, |_| FileNode::new(PathBuf::from("missing.rs")).map(Some));

        assert!(system_under_test.is_err());
        Ok(())
    }

    #[rstest]
    #[case::call_site_and_external_macro(true)]
    #[case::external_only(false)]
    fn test_definition_targets_with_external_macro_locations(#[case] include_call_site: bool) -> Result<()> {
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let target = FileNode::new(PathBuf::from("model.rs"))?;
        let mut files = BTreeSet::from([project.with_file_name("macro-dependency").join("macro.rs")]);
        if include_call_site {
            files.insert(project.join(target.path()));
        }

        let system_under_test = definition_target(&project, &files, &BTreeSet::new())?;

        assert_eq!(system_under_test, include_call_site.then_some(target));
        Ok(())
    }

    #[test]
    fn test_conflicting_project_definition_targets() {
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let files = BTreeSet::from([project.join("first.rs"), project.join("second.rs")]);

        let system_under_test = definition_target(&project, &files, &BTreeSet::new());

        assert!(system_under_test.is_err());
    }

    #[test]
    fn test_empty_definition_targets() {
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        let system_under_test = definition_target(&project, &BTreeSet::new(), &BTreeSet::new());

        assert!(system_under_test.is_err());
    }
}
