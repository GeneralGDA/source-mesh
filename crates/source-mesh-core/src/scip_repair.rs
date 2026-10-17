use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::btree_map::Entry;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::ensure;
use derive_new::new;
use scip::types::Descriptor;
use scip::types::Occurrence;
use scip::types::SymbolInformation;
use scip::types::SymbolRole;
use scip::types::descriptor::Suffix;

use crate::SourcePath;
use crate::analyzer_definitions::DefinitionResolver;
use crate::analyzer_options::Options;
use crate::diagnostics::emit;
use crate::model::FileNode;
use crate::parallel::try_pair;
use crate::scip_definitions::DefinitionFiles;
use crate::scip_definitions::definitions;
use crate::scip_definitions::unresolved_symbols;
use crate::scip_index::ParsedIndex;
use crate::scip_index::SymbolCache;

#[derive(new)]
pub(crate) struct IndexRepair<'a> {
    project_directory: &'a Path,
    configuration_path_original: Option<&'a Path>,
    configuration_path_without_test: &'a Path,
    options: &'a Options,
}

impl IndexRepair<'_> {
    pub(crate) fn perform(
        &self, index_original: ParsedIndex, index_without_test: ParsedIndex,
    ) -> Result<(ParsedIndex, ParsedIndex)> {
        let mut repair_symbols = colliding_symbols(&index_original, &index_without_test);
        repair_symbols.extend(unresolved_symbols(&index_original)?);
        repair_symbols.extend(unresolved_symbols(&index_without_test)?);
        if repair_symbols.is_empty() {
            return Ok((index_original, index_without_test));
        }
        let repair_pass = |index, configuration_path| -> Result<ParsedIndex> {
            let mut resolver = None;
            repair_occurrences(index, &repair_symbols, |reference| {
                let resolver = if let Some(ref mut resolver) = resolver {
                    resolver
                } else {
                    if self.options.verbose() {
                        emit(format_args!("Resolving missing or ambiguous definitions with rust-analyzer..."));
                    }
                    let verbose = self.options.verbose();
                    resolver.insert(DefinitionResolver::start(self.project_directory, configuration_path, verbose)?)
                };
                let source_path = self.project_directory.join(reference.source.path());
                let locations = resolver.definitions(&source_path, reference.line, reference.column)?;
                let files = locations.iter().map(|location| {
                    dunce::canonicalize(location.file()).with_context(|| {
                        let line = u64::from(*location.line()) + 1;
                        let column = u64::from(*location.column()) + 1;
                        format!("Cannot resolve definition at {}:{}:{}", location.file().display(), line, column)
                    })
                });
                let files = files.collect::<Result<BTreeSet<_>>>()?;
                definition_target(self.project_directory, &files, reference.has_candidates)
            })
        };
        try_pair(
            || repair_pass(index_original, self.configuration_path_original),
            || repair_pass(index_without_test, Some(self.configuration_path_without_test)),
        )
    }
}

fn definition_target(project: &Path, files: &BTreeSet<PathBuf>, has_candidates: bool) -> Result<Option<FileNode>> {
    ensure!(!files.is_empty(), "The analyzer could not locate the referenced definition");
    let project_paths = files.iter().filter_map(|file| file.strip_prefix(project).ok());
    let project_files = project_paths.map(|file| SourcePath::new(file.to_owned()).map(FileNode::new));
    let mut project_files = project_files.collect::<Result<BTreeSet<_>>>()?;
    ensure!(project_files.len() <= 1, "The analyzer returned multiple project definition files: {}", {
        let paths: Vec<_> = project_files.iter().map(|file| file.path().display().to_string()).collect();
        paths.join(", ")
    });
    ensure!(!project_files.is_empty() || !has_candidates, "A known project definition resolved outside the analyzed project");
    Ok(project_files.pop_first())
}
#[must_use]
fn colliding_symbols(index_original: &ParsedIndex, index_without_test: &ParsedIndex) -> BTreeSet<String> {
    let mut combined = definitions(index_original);
    for (symbol, files) in definitions(index_without_test) {
        match combined.entry(symbol) {
            Entry::Vacant(entry) => {
                entry.insert(files);
            }
            Entry::Occupied(mut entry) => entry.get_mut().extend(files),
        }
    }
    let ambiguous = combined.into_iter().filter(|entry| matches!(entry.1, DefinitionFiles::Ambiguous(_)));
    ambiguous.map(|(symbol, _)| symbol.to_owned()).collect()
}

struct UnresolvedReference<'a> {
    source: &'a FileNode,
    line: u32,
    column: u32,
    has_candidates: bool,
}

#[derive(new)]
struct DefinitionRecovery {
    candidates: BTreeMap<String, DefinitionFiles>,
    #[new(default)]
    recovered: BTreeMap<FileNode, BTreeSet<String>>,
}

impl DefinitionRecovery {
    fn apply(&mut self, occurrence: &mut Occurrence, definition: Option<(FileNode, String)>) {
        let Some((target, symbol)) = definition else {
            assert!(
                !self.candidates.contains_key(&occurrence.symbol),
                "A known project definition cannot resolve to an external file"
            );
            return;
        };
        occurrence.symbol.clone_from(&symbol);
        self.recovered.entry(target).or_default().insert(symbol);
    }

    fn install(mut self, index: &mut ParsedIndex) -> Result<()> {
        for document in index.documents_mut() {
            if let Some(symbols) = self.recovered.remove(document.file()) {
                for symbol in symbols {
                    if !document.document().symbols.iter().any(|information| information.symbol == symbol) {
                        document.symbols_mut().push(SymbolInformation { symbol, ..SymbolInformation::default() });
                    }
                }
            }
        }
        ensure!(self.recovered.is_empty(), "A dependency resolves to a source file absent from the analysis: {}", {
            let paths: Vec<_> = self.recovered.keys().map(|file| file.path().display().to_string()).collect();
            paths.join(", ")
        });
        Ok(())
    }
}

fn repair_occurrences(
    mut index: ParsedIndex,
    repair_symbols: &BTreeSet<String>,
    mut resolve: impl FnMut(&UnresolvedReference<'_>) -> Result<Option<FileNode>>,
) -> Result<ParsedIndex> {
    let candidates = definitions(&index).into_iter().filter(|&(symbol, _)| repair_symbols.contains(symbol));
    let candidates = candidates.map(|(symbol, files)| (symbol.to_owned(), files)).collect();
    let mut recovery = DefinitionRecovery::new(candidates);
    let (documents, symbols) = index.documents_with_symbols_mut();
    for document in documents {
        let source = document.file().clone();
        for occurrence in document.occurrences_mut() {
            if !repair_symbols.contains(&occurrence.symbol) {
                continue;
            }
            let targets = recovery.candidates.get(&occurrence.symbol);
            let target = if occurrence.symbol_roles & SymbolRole::Definition as i32 != 0 {
                Some(source.clone())
            } else if let Some(target) = targets.and_then(|files| match *files {
                DefinitionFiles::Unique(ref target) => Some(target),
                DefinitionFiles::Ambiguous(_) => None,
            }) {
                Some(target.clone())
            } else {
                let (line, column) = match occurrence.range.as_slice() {
                    &[line, column, ..] if occurrence.range.len() == 3 || occurrence.range.len() == 4 => {
                        (u32::try_from(line)?, u32::try_from(column)?)
                    }
                    _ => {
                        let path = source.path().display();
                        return Err(anyhow!("Invalid reference range in {}: {:?}", path, occurrence.range));
                    }
                };
                let reference = UnresolvedReference { source: &source, line, column, has_candidates: targets.is_some() };
                let resolved = resolve(&reference);
                resolved.with_context(|| {
                    format!("Cannot determine dependency at {}:{}:{}", source.path().display(), line + 1, column + 1)
                })?
            };
            let definition = target.map(|target| {
                ensure!(
                    targets.is_none_or(|files| files.contains(&target)),
                    "Definition resolver returned a file outside the indexed candidates"
                );
                let symbol = qualified_symbol(symbols, &occurrence.symbol, &target)?;
                Ok((target, symbol))
            });
            recovery.apply(occurrence, definition.transpose()?);
        }
        for information in document.symbols_mut() {
            if recovery.candidates.get(&information.symbol).is_some_and(|files| files.contains(&source)) {
                information.symbol = qualified_symbol(symbols, &information.symbol, &source)?;
            }
        }
    }
    recovery.install(&mut index)?;
    Ok(index)
}

fn qualified_symbol(symbols: &mut SymbolCache, symbol: &str, file: &FileNode) -> Result<String> {
    let mut parsed = symbols.get(symbol).require_structure()?.clone();
    let components: Vec<_> = file.path().components().map(|part| part.as_os_str().to_string_lossy()).collect();
    let path = components.join("/");
    let namespace = Descriptor {
        name: format!("source-mesh:{path}"),
        suffix: Suffix::Namespace.into(),
        ..Descriptor::default()
    };
    parsed.descriptors.insert(0, namespace);
    Ok(symbols.intern(parsed))
}
#[cfg(test)]
#[expect(clippy::panic_in_result_fn, reason = "Tests return Result for fallible setup and use assertions for expectations.")]
mod tests {
    use std::collections::BTreeSet;
    use std::panic::catch_unwind;
    use std::path::Path;
    use std::path::PathBuf;

    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use protobuf::Message as _;
    use rstest::rstest;
    use scip::types::Document;
    use scip::types::Index;
    use scip::types::Occurrence;
    use scip::types::SymbolRole;

    use super::IndexRepair;
    use super::UnresolvedReference;
    use super::colliding_symbols;
    use super::definition_target;
    use super::repair_occurrences;
    use crate::SourcePath;
    use crate::analyzer_options::Options;
    use crate::model::DependencyKind;
    use crate::model::FileEdge;
    use crate::model::FileNode;
    use crate::scip_backend::file_graph;
    use crate::scip_backend::parse_scip;
    use crate::scip_definitions::definitions;
    use crate::scip_definitions::unresolved_symbols;
    use crate::scip_index::ParsedIndex;
    use crate::test_dependencies::mark_test_references;

    fn index(symbol: &str, files: &[(&str, &[i32])]) -> Result<ParsedIndex> {
        let documents = files.iter().map(|&(path, roles)| {
            let relative_path = path.into();
            let occurrences = roles.iter().map(|&symbol_roles| {
                Occurrence { symbol: symbol.into(), symbol_roles, range: vec![0, 0, 3], ..Occurrence::default() }
            });
            Document { relative_path, occurrences: occurrences.collect(), ..Document::default() }
        });
        let fixture = Index { documents: documents.collect(), ..Index::default() };
        ParsedIndex::parse(&fixture.write_to_bytes()?)
    }

    fn unexpected_definition_resolution(_: &UnresolvedReference<'_>) -> Result<Option<FileNode>> {
        unreachable!("Unexpected definition resolver call")
    }

    #[rstest]
    #[case::same_file("binary.rs", "binary.rs")]
    #[case::library_call("binary.rs", "library.rs")]
    fn test_semantic_resolution(#[case] caller: &str, #[case] target: &str) -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let fixture = index(symbol, &[("library.rs", &[1]), (caller, &[1, 0])])?;
        let target = FileNode::new(SourcePath::new(PathBuf::from(target))?);
        let collisions = colliding_symbols(&fixture, &fixture);
        let source = FileNode::new(SourcePath::new(PathBuf::from(caller))?);
        let expected = if source == target {
            BTreeSet::new()
        } else {
            BTreeSet::from([FileEdge::new(source, target.clone(), DependencyKind::Production)])
        };

        let system_under_test = repair_occurrences(fixture, &collisions, |_| Ok(Some(target.clone())))?;
        let graph = file_graph(&system_under_test)?;

        assert_eq!(graph.edges(), &expected);
        Ok(())
    }

    #[test]
    fn test_unreferenced_collisions() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 main().";
        let fixture = index(symbol, &[("first.rs", &[1]), ("second.rs", &[1])])?;
        let collisions = colliding_symbols(&fixture, &fixture);

        let system_under_test = repair_occurrences(fixture, &collisions, unexpected_definition_resolution)?;
        let graph = file_graph(&system_under_test)?;

        assert!(graph.edges().is_empty());
        assert_eq!(graph.files().len(), 2);
        Ok(())
    }

    #[test]
    fn test_shared_qualification_test_classification() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let index_original = index(symbol, &[("first.rs", &[1]), ("second.rs", &[1]), ("caller.rs", &[0])])?;
        let index_without_test = index(symbol, &[("first.rs", &[1]), ("caller.rs", &[0])])?;
        let collisions = colliding_symbols(&index_original, &index_without_test);
        let target = FileNode::new(SourcePath::new(PathBuf::from("first.rs"))?);
        let expected_dependencies = BTreeSet::from([FileEdge::new(
            FileNode::new(SourcePath::new(PathBuf::from("caller.rs"))?),
            target.clone(),
            DependencyKind::Production,
        )]);

        let index_original = repair_occurrences(index_original, &collisions, |_| Ok(Some(target.clone())))?;
        let index_without_test = repair_occurrences(index_without_test, &collisions, unexpected_definition_resolution)?;
        let classified = mark_test_references(index_original, &index_without_test)?;
        let system_under_test = file_graph(&classified)?;

        assert_eq!(system_under_test.edges(), &expected_dependencies);
        Ok(())
    }

    #[test]
    fn test_configuration_specific_target_identity() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let index_original = index(symbol, &[("test.rs", &[1]), ("caller.rs", &[0])])?;
        let index_without_test = index(symbol, &[("production.rs", &[1]), ("caller.rs", &[0])])?;
        let collisions = colliding_symbols(&index_original, &index_without_test);

        let index_original = repair_occurrences(index_original, &collisions, unexpected_definition_resolution)?;
        let index_without_test = repair_occurrences(index_without_test, &collisions, unexpected_definition_resolution)?;
        let system_under_test = mark_test_references(index_original, &index_without_test)?;

        let occurrences = system_under_test.documents().iter().flat_map(|document| &document.document().occurrences);
        let mut references = occurrences.filter(|occurrence| occurrence.symbol_roles & SymbolRole::Definition as i32 == 0);
        assert!(references.all(|occurrence| occurrence.symbol_roles & SymbolRole::Test as i32 != 0));
        Ok(())
    }

    #[test]
    fn test_outside_candidate_rejection() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let fixture = index(symbol, &[("first.rs", &[1]), ("second.rs", &[1]), ("caller.rs", &[0])])?;
        let collisions = colliding_symbols(&fixture, &fixture);

        let system_under_test = repair_occurrences(fixture, &collisions, |_| {
            Ok(Some(FileNode::new(SourcePath::new(PathBuf::from("outside.rs"))?)))
        });

        assert!(system_under_test.is_err());
        Ok(())
    }

    #[rstest]
    #[case("model.rs", "client.rs")]
    #[case("src/../model.rs", r".\nested\..\client.rs")]
    fn test_generated_definition_recovery_and_offline_replay(
        #[case] definition_path: &str,
        #[case] reference_path: &str,
    ) -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 model/impl#[Value]get().";
        let fixture = index(symbol, &[(definition_path, &[]), (reference_path, &[0])])?;
        let repair_symbols = BTreeSet::from([symbol.to_owned()]);
        let target = FileNode::new(SourcePath::new(PathBuf::from("model.rs"))?);
        let expected_dependencies = BTreeSet::from([FileEdge::new(
            FileNode::new(SourcePath::new(PathBuf::from("client.rs"))?),
            target.clone(),
            DependencyKind::Production,
        )]);

        let repaired = repair_occurrences(fixture, &repair_symbols, |_| Ok(Some(target.clone())))?;
        let system_under_test = file_graph(&repaired)?;
        let mut occurrences = repaired.documents().iter().flat_map(|document| &document.document().occurrences);
        let only_references = occurrences.all(|occurrence| occurrence.symbol_roles & SymbolRole::Definition as i32 == 0);
        let replayed = parse_scip(&repaired.into_index().write_to_bytes()?)?;

        assert_eq!(system_under_test.edges(), &expected_dependencies);
        assert_eq!(replayed.files(), system_under_test.files());
        assert_eq!(replayed.edges(), system_under_test.edges());
        assert!(only_references, "Recovering ownership must not invent declaration occurrences");
        Ok(())
    }

    #[test]
    fn test_generated_definition_classification_with_different_export_metadata() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 model/impl#[Value]get().";
        let index_original = index(symbol, &[("model.rs", &[1]), ("client.rs", &[0]), ("validation.rs", &[0])])?;
        let index_without_test = index(symbol, &[("model.rs", &[]), ("client.rs", &[0])])?;
        let repair_symbols = BTreeSet::from([symbol.to_owned()]);
        let target = FileNode::new(SourcePath::new(PathBuf::from("model.rs"))?);
        let client = FileNode::new(SourcePath::new(PathBuf::from("client.rs"))?);
        let expected_dependencies = BTreeSet::from([
            FileEdge::new(client, target.clone(), DependencyKind::Production),
            FileEdge::new(FileNode::new(SourcePath::new(PathBuf::from("validation.rs"))?), target.clone(), DependencyKind::Test),
        ]);

        let index_original = repair_occurrences(index_original, &repair_symbols, unexpected_definition_resolution)?;
        let index_without_test = repair_occurrences(index_without_test, &repair_symbols, |_| Ok(Some(target.clone())))?;
        let classified = mark_test_references(index_original, &index_without_test)?;
        let system_under_test = file_graph(&classified)?;

        assert_eq!(system_under_test.edges(), &expected_dependencies);
        Ok(())
    }

    #[test]
    fn test_external_definition_resolution() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 external().";
        let fixture = index(symbol, &[("client.rs", &[0])])?;
        let repair_symbols = BTreeSet::from([symbol.to_owned()]);

        let repaired = repair_occurrences(fixture, &repair_symbols, |_| Ok(None))?;
        let system_under_test = file_graph(&repaired)?;

        assert!(system_under_test.edges().is_empty());
        Ok(())
    }

    #[test]
    fn test_missing_definition_document() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 get().";
        let fixture = index(symbol, &[("client.rs", &[0])])?;
        let repair_symbols = BTreeSet::from([symbol.to_owned()]);

        let system_under_test = repair_occurrences(fixture, &repair_symbols, |_| {
            Ok(Some(FileNode::new(SourcePath::new(PathBuf::from("missing.rs"))?)))
        });

        assert!(system_under_test.is_err());
        Ok(())
    }

    #[rstest]
    #[case::call_site_and_external_macro(true)]
    #[case::external_only(false)]
    fn test_definition_targets_with_external_macro_locations(#[case] include_call_site: bool) -> Result<()> {
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let target = FileNode::new(SourcePath::new(PathBuf::from("model.rs"))?);
        let mut files = BTreeSet::from([project.with_file_name("macro-dependency").join("macro.rs")]);
        if include_call_site {
            files.insert(project.join(target.path()));
        }

        let system_under_test = definition_target(&project, &files, false)?;

        assert_eq!(system_under_test, include_call_site.then_some(target));
        Ok(())
    }

    #[test]
    fn test_conflicting_project_definition_targets() {
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let files = BTreeSet::from([project.join("first.rs"), project.join("second.rs")]);

        let system_under_test = definition_target(&project, &files, false);

        assert!(system_under_test.is_err());
    }

    #[test]
    fn test_empty_definition_targets() {
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        let system_under_test = definition_target(&project, &BTreeSet::new(), false);

        assert!(system_under_test.is_err());
    }

    #[test]
    fn test_normalized_definition_candidates() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let fixture = index(
            symbol,
            &[
                ("src/../model.rs", &[SymbolRole::Definition as i32]),
                ("model.rs", &[SymbolRole::Definition as i32]),
                ("nested/../client.rs", &[0]),
            ],
        )?;
        let repair_symbols = BTreeSet::from([symbol.to_owned()]);
        let collisions = colliding_symbols(&fixture, &fixture);
        let target = FileNode::new(SourcePath::new(PathBuf::from("model.rs"))?);
        let source = FileNode::new(SourcePath::new(PathBuf::from("client.rs"))?);
        let expected = BTreeSet::from([FileEdge::new(source, target, DependencyKind::Production)]);

        let repaired = repair_occurrences(fixture, &repair_symbols, unexpected_definition_resolution)?;
        let system_under_test = file_graph(&repaired)?;

        assert!(collisions.is_empty(), "Equivalent paths must not create ambiguous definitions");
        assert_eq!(system_under_test.edges(), &expected);
        Ok(())
    }

    #[rstest]
    #[case("")]
    #[case("src/..")]
    #[case("../outside.rs")]
    #[case(r"src\..\..\outside.rs")]
    #[case("C:relative.rs")]
    fn test_invalid_repair_document_paths(
        #[case] path: &str, #[values(false, true)] invalid_index_without_test: bool,
    ) -> Result<()> {
        let valid = index("", &[("caller.rs", &[])])?.into_index().write_to_bytes()?;
        let document = Document { relative_path: path.into(), ..Document::default() };
        let invalid = Index { documents: vec![document], ..Index::default() }.write_to_bytes()?;
        let (index_original, index_without_test) = if invalid_index_without_test { (valid, invalid) } else { (invalid, valid) };
        let options = Options::new(false);
        let system_under_test = IndexRepair::new(Path::new("."), None, Path::new("without_test.json"), &options);

        let index_original = ParsedIndex::parse(&index_original);
        let repaired = index_original.and_then(|index_original| {
            system_under_test.perform(index_original, ParsedIndex::parse(&index_without_test)?)
        });

        assert!(repaired.is_err(), "Invalid document path was accepted before repair: {path}");
        Ok(())
    }

    #[rstest]
    #[case::project_root(PathBuf::new())]
    #[case::parent_escape(Path::new("..").join("outside.rs"))]
    #[case::nested_parent(Path::new("src").join("..").join("model.rs"))]
    fn test_invalid_project_definition_paths(#[case] path: PathBuf) {
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let files = BTreeSet::from([project.join(path)]);

        let system_under_test = definition_target(&project, &files, false);

        assert!(system_under_test.is_err(), "Malformed analyzer paths must still pass SourcePath validation");
    }

    #[test]
    fn test_known_definition_outside_project() {
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let files = BTreeSet::from([project.with_file_name("dependency").join("model.rs")]);

        let system_under_test = definition_target(&project, &files, true);

        assert!(system_under_test.is_err(), "Known project definitions must not resolve externally");
    }

    #[test]
    fn test_definition_resolution_callback_contract() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let fixture = index(symbol, &[("first.rs", &[1]), ("second.rs", &[1]), ("caller.rs", &[0])])?;
        let repair_symbols = colliding_symbols(&fixture, &fixture);

        let system_under_test = catch_unwind(|| repair_occurrences(fixture, &repair_symbols, |_| Ok(None)));

        assert!(system_under_test.is_err(), "A resolver contract violation must not become an operational error");
        Ok(())
    }

    #[test]
    fn test_parsed_analysis_pipeline_document_identities() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let index_original = index(
            symbol,
            &[("nested/../model.rs", &[SymbolRole::Definition as i32]), ("src/../client.rs", &[0]), ("tests.rs", &[0])],
        )?;
        let index_without_test = index(symbol, &[("model.rs", &[SymbolRole::Definition as i32]), ("client.rs", &[0])])?;
        let files_original = index_original.documents().iter().map(|document| std::ptr::from_ref(document.file()));
        let identities_original: Vec<_> = files_original.collect();
        let files_without_test = index_without_test.documents().iter().map(|document| std::ptr::from_ref(document.file()));
        let identities_without_test: Vec<_> = files_without_test.collect();
        let target = FileNode::new(SourcePath::new(PathBuf::from("model.rs"))?);
        let client = FileNode::new(SourcePath::new(PathBuf::from("client.rs"))?);
        let expected = BTreeSet::from([
            FileEdge::new(client, target.clone(), DependencyKind::Production),
            FileEdge::new(FileNode::new(SourcePath::new(PathBuf::from("tests.rs"))?), target, DependencyKind::Test),
        ]);
        let options = Options::new(false);
        let repair = IndexRepair::new(Path::new("."), None, Path::new("without_test.json"), &options);

        let (index_original, index_without_test) = repair.perform(index_original, index_without_test)?;
        let system_under_test = mark_test_references(index_original, &index_without_test)?;
        let graph = file_graph(&system_under_test)?;
        let classified_files = system_under_test.documents().iter().map(|document| std::ptr::from_ref(document.file()));
        let classified_identities: Vec<_> = classified_files.collect();
        let documents_without_test = index_without_test.documents();
        let retained_files_without_test = documents_without_test.iter().map(|document| std::ptr::from_ref(document.file()));
        let retained_identities_without_test: Vec<_> = retained_files_without_test.collect();

        assert_eq!(classified_identities, identities_original, "Private stages must retain parsed document identities");
        assert_eq!(
            retained_identities_without_test, identities_without_test, "Identities without cfg(test) must not be rebuilt",
        );
        assert_eq!(graph.edges(), &expected);
        Ok(())
    }

    #[test]
    fn test_qualified_symbol_cache_and_replay() -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let fixture = index(symbol, &[("first.rs", &[1]), ("second.rs", &[1]), ("caller.rs", &[0, 0])])?;
        let original_structure = fixture.symbol_cache().get(symbol).require_structure()?.clone();
        let repair_symbols = colliding_symbols(&fixture, &fixture);
        let target = FileNode::new(SourcePath::new(PathBuf::from("first.rs"))?);

        let system_under_test = repair_occurrences(fixture, &repair_symbols, |_| Ok(Some(target.clone())))?;
        let unresolved = unresolved_symbols(&system_under_test)?;
        let definition_count = definitions(&system_under_test).len();
        let retained_original = system_under_test.symbol_cache().get(symbol).require_structure()?.clone();
        let cached_symbols = system_under_test.symbols().map(|information| {
            let structure = system_under_test.symbol_cache().get(&information.symbol).require_structure()?;
            Ok((information.symbol.clone(), structure.clone()))
        });
        let cached_symbols = cached_symbols.collect::<Result<Vec<_>>>()?;
        let replayed = ParsedIndex::parse(&system_under_test.into_index().write_to_bytes()?)?;

        assert!(unresolved.is_empty(), "Qualified symbols must be immediately available to semantic consumers");
        assert_eq!(definition_count, 2, "Each physical definition must retain its qualified identity");
        assert_eq!(retained_original, original_structure, "Qualification must not mutate the original cached symbol");
        for (qualified_symbol, structure) in cached_symbols {
            let replayed_structure = replayed.symbol_cache().get(&qualified_symbol).require_structure()?;
            assert_eq!(replayed_structure, &structure, "Cached generated symbols must match their serialized identity");
        }
        Ok(())
    }

    #[rstest]
    #[case::qualification_requires_structure(true)]
    #[case::external_target_keeps_opaque_identifier(false)]
    fn test_opaque_repair_resolution(#[case] project_target: bool) -> Result<()> {
        let symbol = "synthetic-method";
        let fixture = index(symbol, &[("model.rs", &[]), ("client.rs", &[0, 0])])?;
        let repair_symbols = BTreeSet::from([symbol.to_owned()]);
        let target = FileNode::new(SourcePath::new(PathBuf::from("model.rs"))?);

        let system_under_test = repair_occurrences(fixture, &repair_symbols, |_| Ok(project_target.then_some(target.clone())));

        assert_eq!(system_under_test.is_err(), project_target, "Only qualification requires a structured symbol");
        if let Ok(index) = system_under_test {
            assert!(index.symbol_cache().get(symbol).structure().is_none(), "External opaque symbols must remain opaque");
        }
        Ok(())
    }
}
