use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::btree_map::Entry;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use derive_new::new;
use scip::types::Descriptor;
use scip::types::Occurrence;
use scip::types::SymbolInformation;
use scip::types::SymbolRole;
use scip::types::descriptor::Suffix;

use crate::SourcePath;
use crate::analyzer::definitions::DefinitionResolver;
use crate::analyzer::definitions::canonical_definition_file;
use crate::analyzer::options::Options;
use crate::analyzer::parallel::try_pair;
use crate::diagnostics::emit;
use crate::model::FileNode;
use crate::scip::definitions::DefinitionFiles;
use crate::scip::definitions::definitions;
use crate::scip::definitions::unresolved_symbols;
use crate::scip::index::ParsedIndex;
use crate::scip::index::SymbolCache;
use crate::scip::range::ScipRange;

const MESSAGE_OUTSIDE_INDEXED_CANDIDATES: &str = "Definition resolver returned a file outside the indexed candidates";
const MESSAGE_INVALID_REFERENCE_RANGE: &str = "Invalid reference range";

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
                let files = locations.iter().map(canonical_definition_file).collect::<Result<BTreeSet<_>>>()?;
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
            assert!(!self.candidates.contains_key(&occurrence.symbol), "Known project definitions cannot resolve externally");
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
                let range = ScipRange::parse(&occurrence.range).with_context(|| {
                    format!("{MESSAGE_INVALID_REFERENCE_RANGE} in {}: {:?}", source.path().display(), occurrence.range)
                })?;
                let (line, column) = range.start();
                let reference = UnresolvedReference { source: &source, line, column, has_candidates: targets.is_some() };
                let resolved = resolve(&reference);
                resolved.with_context(|| {
                    format!("Cannot determine dependency at {}:{}:{}", source.path().display(), line + 1, column + 1)
                })?
            };
            let definition = target.map(|target| {
                ensure!(targets.is_none_or(|files| files.contains(&target)), MESSAGE_OUTSIDE_INDEXED_CANDIDATES);
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
    let namespace = Descriptor { name: format!("source-mesh:{path}"), suffix: Suffix::Namespace.into(), ..Descriptor::default() };
    parsed.descriptors.insert(0, namespace);
    Ok(symbols.intern(parsed))
}
#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::panic::catch_unwind;
    use std::path::Path;
    use std::path::PathBuf;

    use anyhow::Result;
    use claims::assert_ok;
    use pretty_assertions::assert_eq;
    use protobuf::Message as _;
    use rstest::rstest;
    use scip::types::Document;
    use scip::types::Index;
    use scip::types::Occurrence;
    use scip::types::SymbolInformation;
    use scip::types::SymbolRole;

    use super::IndexRepair;
    use super::MESSAGE_INVALID_REFERENCE_RANGE;
    use super::MESSAGE_OUTSIDE_INDEXED_CANDIDATES;
    use super::colliding_symbols;
    use super::definition_target;
    use super::repair_occurrences;
    use crate::SourcePath;
    use crate::analyzer::options::Options;
    use crate::model::DependencyKind;
    use crate::model::FileEdge;
    use crate::model::FileNode;
    use crate::scip::backend::file_graph;
    use crate::scip::backend::parse_scip;
    use crate::scip::definitions::definitions;
    use crate::scip::definitions::unresolved_symbols;
    use crate::scip::index::ParsedIndex;
    use crate::scip::test_dependencies::mark_test_references;

    #[must_use]
    fn index_fixture(symbol: &str, files: &[(&str, &[i32])], range: &[i32]) -> Index {
        let documents = files.iter().map(|&(path, roles)| {
            let relative_path = path.into();
            let occurrences = roles.iter().map(|&symbol_roles| {
                Occurrence { symbol: symbol.into(), symbol_roles, range: range.to_vec(), ..Occurrence::default() }
            });
            Document { relative_path, occurrences: occurrences.collect(), ..Document::default() }
        });
        Index { documents: documents.collect(), ..Index::default() }
    }

    fn index(symbol: &str, files: &[(&str, &[i32])]) -> Result<ParsedIndex> {
        ParsedIndex::parse(&index_fixture(symbol, files, &[0, 0, 3]).write_to_bytes()?)
    }

    #[rstest]
    #[case::single_line(&[7, 13, 17])]
    #[case::explicit_single_line(&[7, 13, 7, 17])]
    #[case::multiple_lines(&[7, 13, 8, 2])]
    #[case::empty_single_line(&[7, 13, 13])]
    #[case::empty_explicit_single_line(&[7, 13, 7, 13])]
    fn test_semantic_resolution(#[case] range: &[i32], #[values("binary.rs", "library.rs")] target: &str) {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let caller = "binary.rs";
        let fixture_index = index_fixture(symbol, &[("library.rs", &[1]), (caller, &[1, 0])], range);
        let fixture_bytes = fixture_index.write_to_bytes();
        let fixture_bytes = assert_ok!(fixture_bytes, "The ambiguous definition fixture must serialize");
        let fixture = ParsedIndex::parse(&fixture_bytes);
        let fixture = assert_ok!(fixture, "The ambiguous definition fixture must parse");
        let resolved_target = SourcePath::new(PathBuf::from(target));
        let resolved_target = assert_ok!(resolved_target, "The semantic resolution target must be a valid source path");
        let resolved_target = FileNode::new(resolved_target);
        let collisions = colliding_symbols(&fixture, &fixture);
        let source = SourcePath::new(PathBuf::from(caller));
        let source = assert_ok!(source, "The semantic resolution caller must be a valid source path");
        let source = FileNode::new(source);
        let expected = if source == resolved_target {
            BTreeSet::new()
        } else {
            BTreeSet::from([FileEdge::new(source, resolved_target.clone(), DependencyKind::Production)])
        };
        let mut resolved_positions = Vec::new();

        let system_under_test = repair_occurrences(fixture, &collisions, |reference| {
            resolved_positions.push((reference.line, reference.column));
            Ok(Some(resolved_target.clone()))
        });
        let system_under_test = assert_ok!(system_under_test, "Semantic resolution must repair the ambiguous definition");
        let graph = file_graph(&system_under_test);
        let graph = assert_ok!(graph, "The semantically repaired index must produce a file graph");

        assert_eq!(resolved_positions, vec![(7, 13)], "Semantic lookup must use the exact zero-based reference position");
        assert_eq!(graph.edges(), &expected);
    }

    #[rstest]
    #[case::missing_end(&[7, 13])]
    #[case::extra_coordinate(&[7, 13, 8, 2, 42])]
    #[case::negative_end_column(&[7, 13, 8, -1])]
    #[case::reversed_lines(&[7, 13, 6, 2])]
    #[case::reversed_columns(&[7, 13, 12])]
    fn test_invalid_reference_ranges(#[case] range: &[i32]) {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let caller = "caller.rs";
        let fixture_index = index_fixture(symbol, &[(caller, &[0])], range);
        let fixture_bytes = fixture_index.write_to_bytes();
        let fixture_bytes = assert_ok!(fixture_bytes, "The invalid reference range fixture must serialize");
        let fixture = ParsedIndex::parse(&fixture_bytes);
        let fixture = assert_ok!(fixture, "Index parsing must defer reference range validation to repair");
        let repair_symbols = BTreeSet::from([symbol.to_owned()]);

        let expected_range = format!("{range:?}");

        let system_under_test = repair_occurrences(fixture, &repair_symbols, |_| {
            unreachable!("Malformed reference ranges must not invoke definition resolution")
        });

        assert!(system_under_test.as_ref().is_err_and(|error| {
            let message = error.to_string();
            message.starts_with(MESSAGE_INVALID_REFERENCE_RANGE) && message.contains(caller) && message.contains(&expected_range)
        }), "Malformed reference ranges must be rejected with source context before semantic lookup");
    }

    #[test]
    fn test_unreferenced_collisions() {
        let symbol = "rust-analyzer cargo fixture 0.0.0 main().";
        let fixture = index(symbol, &[("first.rs", &[1]), ("second.rs", &[1])]);
        let fixture = assert_ok!(fixture, "The unreferenced duplicate definition fixture must parse");
        let collisions = colliding_symbols(&fixture, &fixture);

        let system_under_test = repair_occurrences(fixture, &collisions, |_| {
            unreachable!("Unreferenced definitions must not invoke semantic resolution")
        });
        let system_under_test = assert_ok!(system_under_test, "Repairing unreferenced definition collisions needs no lookup");
        let graph = file_graph(&system_under_test);
        let graph = assert_ok!(graph, "The repaired unreferenced definitions must produce a file graph");

        assert!(graph.edges().is_empty());
        assert_eq!(graph.files().len(), 2);
    }

    #[test]
    fn test_shared_qualification_test_classification() {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let index_original = index(symbol, &[("first.rs", &[1]), ("second.rs", &[1]), ("caller.rs", &[0])]);
        let index_original = assert_ok!(index_original, "The original index with colliding definitions must parse");
        let index_without_test = index(symbol, &[("first.rs", &[1]), ("caller.rs", &[0])]);
        let index_without_test = assert_ok!(index_without_test, "The index without cfg(test) must parse");
        let collisions = colliding_symbols(&index_original, &index_without_test);
        let target = SourcePath::new(PathBuf::from("first.rs"));
        let target = assert_ok!(target, "The shared definition target must be a valid source path");
        let target = FileNode::new(target);
        let source = SourcePath::new(PathBuf::from("caller.rs"));
        let source = assert_ok!(source, "The shared definition caller must be a valid source path");
        let expected_dependencies = BTreeSet::from([FileEdge::new(
            FileNode::new(source), target.clone(), DependencyKind::Production,
        )]);

        let repaired_original = repair_occurrences(index_original, &collisions, |_| Ok(Some(target.clone())));
        let repaired_original = assert_ok!(repaired_original, "The original index must resolve its colliding definitions");
        let repaired_without_test = repair_occurrences(index_without_test, &collisions, |_| {
            unreachable!("The unambiguous index without cfg(test) must not invoke definition resolution")
        });
        let repaired_without_test = assert_ok!(repaired_without_test, "Definition qualification must match without cfg(test)");
        let classified = mark_test_references(repaired_original, &repaired_without_test);
        let classified = assert_ok!(classified, "Shared qualified definitions must support test-reference classification");
        let system_under_test = file_graph(&classified);
        let system_under_test = assert_ok!(system_under_test, "The classified repaired index must produce a file graph");

        assert_eq!(system_under_test.edges(), &expected_dependencies);
    }

    #[test]
    fn test_configuration_specific_target_identity() {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let index_original = index(symbol, &[("test.rs", &[1]), ("caller.rs", &[0])]);
        let index_original = assert_ok!(index_original, "The original index with its test-specific target must parse");
        let index_without_test = index(symbol, &[("production.rs", &[1]), ("caller.rs", &[0])]);
        let index_without_test = assert_ok!(index_without_test, "The index with its production-specific target must parse");
        let collisions = colliding_symbols(&index_original, &index_without_test);

        let repaired_original = repair_occurrences(index_original, &collisions, |_| {
            unreachable!("The original configuration has an unambiguous definition target")
        });
        let repaired_original = assert_ok!(repaired_original, "Original index must qualify its configuration-specific target");
        let repaired_without_test = repair_occurrences(index_without_test, &collisions, |_| {
            unreachable!("The configuration without cfg(test) has an unambiguous definition target")
        });
        let repaired_without_test = assert_ok!(repaired_without_test, "The index without cfg(test) must qualify its own target");
        let system_under_test = mark_test_references(repaired_original, &repaired_without_test);
        let system_under_test = assert_ok!(system_under_test, "Configuration-specific references must be classifiable");

        let occurrences = system_under_test.documents().iter().flat_map(|document| &document.document().occurrences);
        let mut references = occurrences.filter(|occurrence| occurrence.symbol_roles & SymbolRole::Definition as i32 == 0);
        assert!(references.all(|occurrence| occurrence.symbol_roles & SymbolRole::Test as i32 != 0));
    }

    #[test]
    fn test_outside_candidate_rejection() {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let caller = "caller.rs";
        let fixture = index(symbol, &[("first.rs", &[1]), ("second.rs", &[1]), (caller, &[0])]);
        let fixture = assert_ok!(fixture, "The indexed definition candidates must parse");
        let collisions = colliding_symbols(&fixture, &fixture);
        let outside_candidate = SourcePath::new(PathBuf::from(caller));
        let outside_candidate = assert_ok!(outside_candidate, "The out-of-candidate target must still be a valid source path");
        let outside_candidate = FileNode::new(outside_candidate);

        let system_under_test = repair_occurrences(fixture, &collisions, |_| Ok(Some(outside_candidate.clone())));
        let error_message = system_under_test.err().map(|error| error.to_string());

        assert_eq!(error_message.as_deref(), Some(MESSAGE_OUTSIDE_INDEXED_CANDIDATES));
    }

    #[rstest]
    #[case("model.rs", "client.rs")]
    #[case("src/../model.rs", r".\nested\..\client.rs")]
    fn test_generated_definition_recovery_and_offline_replay(
        #[case] definition_path: &str,
        #[case] reference_path: &str,
        #[values(false, true)] include_existing_information: bool,
    ) {
        let symbol = "rust-analyzer cargo fixture 0.0.0 model/impl#[Value]get().";
        let existing_symbol = "rust-analyzer cargo fixture 0.0.0 model/Value#";
        let existing_information = include_existing_information.then(|| SymbolInformation {
            symbol: existing_symbol.into(),
            documentation: vec!["Existing model documentation".into()],
            ..SymbolInformation::default()
        });
        let expected_information: Vec<_> = existing_information.into_iter().collect();
        let mut fixture_index = index_fixture(symbol, &[(definition_path, &[]), (reference_path, &[0])], &[0, 0, 3]);
        for document in &mut fixture_index.documents {
            if document.relative_path == definition_path {
                document.symbols.clone_from(&expected_information);
            }
        }
        let fixture_bytes = fixture_index.write_to_bytes();
        let fixture_bytes = assert_ok!(fixture_bytes, "The generated definition recovery fixture must serialize");
        let fixture = ParsedIndex::parse(&fixture_bytes);
        let fixture = assert_ok!(fixture, "The generated definition recovery fixture must parse");
        let repair_symbols = BTreeSet::from([symbol.to_owned()]);
        let target = SourcePath::new(PathBuf::from("model.rs"));
        let target = assert_ok!(target, "The recovered definition target must be a valid source path");
        let target = FileNode::new(target);
        let source = SourcePath::new(PathBuf::from("client.rs"));
        let source = assert_ok!(source, "The recovered definition caller must be a valid source path");
        let expected_dependencies = BTreeSet::from([FileEdge::new(
            FileNode::new(source), target.clone(), DependencyKind::Production,
        )]);

        let repaired = repair_occurrences(fixture, &repair_symbols, |_| Ok(Some(target.clone())));
        let repaired = assert_ok!(repaired, "Semantic repair must recover the generated definition");
        let system_under_test = file_graph(&repaired);
        let system_under_test = assert_ok!(system_under_test, "The recovered definition must produce a file graph");
        let mut occurrences = repaired.documents().iter().flat_map(|document| &document.document().occurrences);
        let only_references = occurrences.all(|occurrence| occurrence.symbol_roles & SymbolRole::Definition as i32 == 0);
        let target_documents = repaired.documents().iter().filter(|document| document.file() == &target);
        let target_information = target_documents.flat_map(|document| &document.document().symbols);
        let retained_information = target_information.filter(|information| information.symbol == existing_symbol);
        let retained_information: Vec<_> = retained_information.cloned().collect();
        let replay_bytes = repaired.into_index().write_to_bytes();
        let replay_bytes = assert_ok!(replay_bytes, "The recovered definition index must serialize for offline replay");
        let replayed = parse_scip(&replay_bytes);
        let replayed = assert_ok!(replayed, "Offline replay must rebuild the recovered definition graph");

        assert_eq!(system_under_test.edges(), &expected_dependencies);
        assert_eq!(retained_information, expected_information, "Recovery must preserve unrelated symbol information");
        assert_eq!(replayed.files(), system_under_test.files());
        assert_eq!(replayed.edges(), system_under_test.edges());
        assert!(only_references, "Recovering ownership must not invent declaration occurrences");
    }

    #[rstest]
    #[case::single_reference(&[0])]
    #[case::repeated_references(&[0, 0])]
    fn test_existing_definition_information(#[case] reference_roles: &[i32]) {
        let symbol = "rust-analyzer cargo fixture 0.0.0 model/impl#[Value]get().";
        let definition_path = "model.rs";
        let reference_path = "client.rs";
        let documentation = vec!["Generated accessor documentation".to_owned()];
        let existing_information = SymbolInformation {
            symbol: symbol.into(), documentation: documentation.clone(), ..SymbolInformation::default()
        };
        let mut fixture_index = index_fixture(symbol, &[(definition_path, &[]), (reference_path, reference_roles)], &[0, 0, 3]);
        for document in &mut fixture_index.documents {
            if document.relative_path == definition_path {
                document.symbols.push(existing_information.clone());
            }
        }
        let fixture_bytes = fixture_index.write_to_bytes();
        let fixture_bytes = assert_ok!(fixture_bytes, "The existing definition information fixture must serialize");
        let fixture = ParsedIndex::parse(&fixture_bytes);
        let fixture = assert_ok!(fixture, "The existing definition information fixture must parse");
        let repair_symbols = BTreeSet::from([symbol.to_owned()]);
        let target = SourcePath::new(PathBuf::from(definition_path));
        let target = assert_ok!(target, "The existing definition target must be a valid source path");
        let target = FileNode::new(target);
        let source = SourcePath::new(PathBuf::from(reference_path));
        let source = assert_ok!(source, "The existing definition caller must be a valid source path");
        let source = FileNode::new(source);
        let expected_dependencies = BTreeSet::from([FileEdge::new(source, target, DependencyKind::Production)]);

        let repaired = repair_occurrences(fixture, &repair_symbols, |_| {
            unreachable!("Existing definition information must not invoke semantic resolution")
        });
        let repaired = assert_ok!(repaired, "Repair must preserve an existing unambiguous definition");
        let system_under_test = file_graph(&repaired);
        let system_under_test = assert_ok!(system_under_test, "Index with preserved definition information must yield a graph");
        let retained_documentation: Vec<_> = repaired.symbols().map(|information| information.documentation.clone()).collect();
        let replay_bytes = repaired.into_index().write_to_bytes();
        let replay_bytes = assert_ok!(replay_bytes, "The index with preserved definition information must serialize for replay");
        let replayed = parse_scip(&replay_bytes);
        let replayed = assert_ok!(replayed, "Offline replay must rebuild the graph with existing definition information");

        assert_eq!(retained_documentation, vec![documentation], "Existing definition metadata must not be duplicated or erased");
        assert_eq!(system_under_test.edges(), &expected_dependencies);
        assert_eq!(replayed.files(), system_under_test.files());
        assert_eq!(replayed.edges(), system_under_test.edges());
    }

    #[rstest]
    #[case::single_references(&[0])]
    #[case::repeated_references(&[0, 0])]
    fn test_multiple_generated_definition_recovery(#[case] reference_roles: &[i32]) {
        let first_symbol = "rust-analyzer cargo fixture 0.0.0 model/impl#[Value]get().";
        let second_symbol = "rust-analyzer cargo fixture 0.0.0 model/impl#[Value]set().";
        let definition_path = "model.rs";
        let first_caller = "first.rs";
        let second_caller = "second.rs";
        let mut fixture_index = index_fixture(
            first_symbol, &[(definition_path, &[]), (first_caller, reference_roles)], &[0, 0, 3],
        );
        fixture_index.documents.extend(index_fixture(second_symbol, &[(second_caller, reference_roles)], &[0, 0, 3]).documents);
        let fixture_bytes = fixture_index.write_to_bytes();
        let fixture_bytes = assert_ok!(fixture_bytes, "The multiple generated definition fixture must serialize");
        let fixture = ParsedIndex::parse(&fixture_bytes);
        let fixture = assert_ok!(fixture, "The multiple generated definition fixture must parse");
        let repair_symbols = BTreeSet::from([first_symbol.to_owned(), second_symbol.to_owned()]);
        let target = SourcePath::new(PathBuf::from(definition_path));
        let target = assert_ok!(target, "The shared recovered target must be a valid source path");
        let target = FileNode::new(target);
        let first_source = SourcePath::new(PathBuf::from(first_caller));
        let first_source = assert_ok!(first_source, "The first generated definition caller must be a valid source path");
        let first_source = FileNode::new(first_source);
        let second_source = SourcePath::new(PathBuf::from(second_caller));
        let second_source = assert_ok!(second_source, "The second generated definition caller must be a valid source path");
        let second_source = FileNode::new(second_source);
        let expected_dependencies = BTreeSet::from([
            FileEdge::new(first_source, target.clone(), DependencyKind::Production),
            FileEdge::new(second_source, target.clone(), DependencyKind::Production),
        ]);

        let repaired = repair_occurrences(fixture, &repair_symbols, |_| Ok(Some(target.clone())));
        let repaired = assert_ok!(repaired, "Repair must recover both generated definitions");
        let system_under_test = file_graph(&repaired);
        let system_under_test = assert_ok!(system_under_test, "The recovered definitions must produce a file graph");
        let unresolved = unresolved_symbols(&repaired);
        let unresolved = assert_ok!(unresolved, "Recovered symbols must support unresolved-symbol analysis");
        let definition_count = definitions(&repaired).len();
        let replay_bytes = repaired.into_index().write_to_bytes();
        let replay_bytes = assert_ok!(replay_bytes, "Both recovered definitions must serialize for offline replay");
        let replayed = parse_scip(&replay_bytes);
        let replayed = assert_ok!(replayed, "Offline replay must rebuild the graph with both recovered definitions");

        assert_eq!(definition_count, 2, "Every recovered symbol must retain its own definition");
        assert!(unresolved.is_empty(), "Recovered symbols must all have known owners");
        assert_eq!(system_under_test.edges(), &expected_dependencies);
        assert_eq!(replayed.files(), system_under_test.files());
        assert_eq!(replayed.edges(), system_under_test.edges());
    }

    #[test]
    fn test_generated_definition_classification_with_different_export_metadata() {
        let symbol = "rust-analyzer cargo fixture 0.0.0 model/impl#[Value]get().";
        let index_original = index(symbol, &[("model.rs", &[1]), ("client.rs", &[0]), ("validation.rs", &[0])]);
        let index_original = assert_ok!(index_original, "The original index with generated definition metadata must parse");
        let index_without_test = index(symbol, &[("model.rs", &[]), ("client.rs", &[0])]);
        let index_without_test = assert_ok!(index_without_test, "The index missing generated definition metadata must parse");
        let repair_symbols = BTreeSet::from([symbol.to_owned()]);
        let target = SourcePath::new(PathBuf::from("model.rs"));
        let target = assert_ok!(target, "The generated definition target must be a valid source path");
        let target = FileNode::new(target);
        let client = SourcePath::new(PathBuf::from("client.rs"));
        let client = assert_ok!(client, "The production caller must be a valid source path");
        let client = FileNode::new(client);
        let validation = SourcePath::new(PathBuf::from("validation.rs"));
        let validation = assert_ok!(validation, "The test-only caller must be a valid source path");
        let expected_dependencies = BTreeSet::from([
            FileEdge::new(client, target.clone(), DependencyKind::Production),
            FileEdge::new(FileNode::new(validation), target.clone(), DependencyKind::Test),
        ]);

        let repaired_original = repair_occurrences(index_original, &repair_symbols, |_| {
            unreachable!("The original generated definition metadata must avoid semantic lookup")
        });
        let repaired_original = assert_ok!(repaired_original, "The original index must retain its generated definition");
        let repaired_without_test = repair_occurrences(index_without_test, &repair_symbols, |_| Ok(Some(target.clone())));
        let repaired_without_test = assert_ok!(repaired_without_test, "Generated definitions must be restored without cfg(test)");
        let classified = mark_test_references(repaired_original, &repaired_without_test);
        let classified = assert_ok!(classified, "Different export metadata must still support test-reference classification");
        let system_under_test = file_graph(&classified);
        let system_under_test = assert_ok!(system_under_test, "The classified generated definition must produce a file graph");

        assert_eq!(system_under_test.edges(), &expected_dependencies);
    }

    #[test]
    fn test_external_definition_resolution() {
        let symbol = "rust-analyzer cargo fixture 0.0.0 external().";
        let fixture = index(symbol, &[("client.rs", &[0])]);
        let fixture = assert_ok!(fixture, "The external definition reference fixture must parse");
        let repair_symbols = BTreeSet::from([symbol.to_owned()]);

        let repaired = repair_occurrences(fixture, &repair_symbols, |_| Ok(None));
        let repaired = assert_ok!(repaired, "Repair must accept a definition resolved outside the project");
        let system_under_test = file_graph(&repaired);
        let system_under_test = assert_ok!(system_under_test, "Repaired external definition reference must yield a file graph");

        assert!(system_under_test.edges().is_empty());
    }

    #[test]
    fn test_missing_definition_document() {
        let symbol = "rust-analyzer cargo fixture 0.0.0 get().";
        let fixture = index(symbol, &[("client.rs", &[0])]);
        let fixture = assert_ok!(fixture, "The missing definition document fixture must parse");
        let repair_symbols = BTreeSet::from([symbol.to_owned()]);
        let target = SourcePath::new(PathBuf::from("missing.rs"));
        let target = assert_ok!(target, "The missing document target must still be a valid source path");
        let target = FileNode::new(target);

        let system_under_test = repair_occurrences(fixture, &repair_symbols, |_| Ok(Some(target.clone())));

        assert!(system_under_test.is_err(), "Repair must reject a definition target absent from the indexed documents");
    }

    #[rstest]
    #[case::call_site_and_external_macro(true)]
    #[case::external_only(false)]
    fn test_definition_targets_with_external_macro_locations(#[case] include_call_site: bool) {
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let target = SourcePath::new(PathBuf::from("model.rs"));
        let target = assert_ok!(target, "The macro call-site target must be a valid source path");
        let target = FileNode::new(target);
        let mut files = BTreeSet::from([project.with_file_name("macro-dependency").join("macro.rs")]);
        if include_call_site {
            files.insert(project.join(target.path()));
        }

        let system_under_test = definition_target(&project, &files, false);
        let system_under_test = assert_ok!(system_under_test, "External macro locations must allow project call-site selection");

        assert_eq!(system_under_test, include_call_site.then_some(target));
    }

    #[test]
    fn test_conflicting_project_definition_targets() {
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let files = BTreeSet::from([project.join("first.rs"), project.join("second.rs")]);

        let system_under_test = definition_target(&project, &files, false);

        assert!(system_under_test.is_err(), "Conflicting project definition targets must be rejected");
    }

    #[test]
    fn test_empty_definition_targets() {
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        let system_under_test = definition_target(&project, &BTreeSet::new(), false);

        assert!(system_under_test.is_err(), "Definition lookup must reject an empty target set");
    }

    #[test]
    fn test_normalized_definition_candidates() {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let fixture = index(
            symbol,
            &[
                ("src/../model.rs", &[SymbolRole::Definition as i32]),
                ("model.rs", &[SymbolRole::Definition as i32]),
                ("nested/../client.rs", &[0]),
            ],
        );
        let fixture = assert_ok!(fixture, "The equivalent definition path fixture must parse");
        let repair_symbols = BTreeSet::from([symbol.to_owned()]);
        let collisions = colliding_symbols(&fixture, &fixture);
        let target = SourcePath::new(PathBuf::from("model.rs"));
        let target = assert_ok!(target, "The normalized definition target must be a valid source path");
        let target = FileNode::new(target);
        let source = SourcePath::new(PathBuf::from("client.rs"));
        let source = assert_ok!(source, "The normalized caller must be a valid source path");
        let source = FileNode::new(source);
        let expected = BTreeSet::from([FileEdge::new(source, target, DependencyKind::Production)]);

        let repaired = repair_occurrences(fixture, &repair_symbols, |_| {
            unreachable!("Equivalent definition paths must not invoke semantic resolution")
        });
        let repaired = assert_ok!(repaired, "Repair must accept equivalent unambiguous definition paths");
        let system_under_test = file_graph(&repaired);
        let system_under_test = assert_ok!(system_under_test, "The normalized repaired index must produce a file graph");

        assert!(collisions.is_empty(), "Equivalent paths must not create ambiguous definitions");
        assert_eq!(system_under_test.edges(), &expected);
    }

    #[rstest]
    #[case("")]
    #[case("src/..")]
    #[case("../outside.rs")]
    #[case(r"src\..\..\outside.rs")]
    #[case("C:relative.rs")]
    fn test_invalid_repair_document_paths(#[case] path: &str, #[values(false, true)] invalid_index_without_test: bool) {
        let valid = index("", &[("caller.rs", &[])]);
        let valid = assert_ok!(valid, "The valid repair document fixture must parse");
        let valid_bytes = valid.into_index().write_to_bytes();
        let valid_bytes = assert_ok!(valid_bytes, "The valid repair document fixture must serialize");
        let document = Document { relative_path: path.into(), ..Document::default() };
        let invalid_bytes = Index { documents: vec![document], ..Index::default() }.write_to_bytes();
        let invalid_bytes = assert_ok!(invalid_bytes, "The invalid document path fixture must serialize before path validation");
        let (index_original, index_without_test) = if invalid_index_without_test {
            (valid_bytes, invalid_bytes)
        } else {
            (invalid_bytes, valid_bytes)
        };
        let options = Options::new(false);
        let system_under_test = IndexRepair::new(Path::new("."), None, Path::new("without_test.json"), &options);

        let original_parse = ParsedIndex::parse(&index_original);
        let repaired = match original_parse {
            Ok(parsed_original) => {
                let baseline_parse = ParsedIndex::parse(&index_without_test);
                match baseline_parse {
                    Ok(parsed_without_test) => system_under_test.perform(parsed_original, parsed_without_test),
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        };

        assert!(repaired.is_err(), "Invalid document path was accepted before repair: {path}");
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
    fn test_definition_resolution_callback_contract() {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let fixture = index(symbol, &[("first.rs", &[1]), ("second.rs", &[1]), ("caller.rs", &[0])]);
        let fixture = assert_ok!(fixture, "The resolver callback contract fixture must parse");
        let repair_symbols = colliding_symbols(&fixture, &fixture);

        let system_under_test = catch_unwind(|| repair_occurrences(fixture, &repair_symbols, |_| Ok(None)));

        assert!(system_under_test.is_err(), "A resolver contract violation must not become an operational error");
    }

    #[test]
    fn test_parsed_analysis_pipeline_document_identities() {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let index_original = index(
            symbol,
            &[("nested/../model.rs", &[SymbolRole::Definition as i32]), ("src/../client.rs", &[0]), ("tests.rs", &[0])],
        );
        let index_original = assert_ok!(index_original, "The original pipeline index with normalized document paths must parse");
        let index_without_test = index(symbol, &[("model.rs", &[SymbolRole::Definition as i32]), ("client.rs", &[0])]);
        let index_without_test = assert_ok!(index_without_test, "The pipeline index without cfg(test) must parse");
        let files_original = index_original.documents().iter().map(|document| std::ptr::from_ref(document.file()));
        let identities_original: Vec<_> = files_original.collect();
        let files_without_test = index_without_test.documents().iter().map(|document| std::ptr::from_ref(document.file()));
        let identities_without_test: Vec<_> = files_without_test.collect();
        let target = SourcePath::new(PathBuf::from("model.rs"));
        let target = assert_ok!(target, "The pipeline definition target must be a valid source path");
        let target = FileNode::new(target);
        let client = SourcePath::new(PathBuf::from("client.rs"));
        let client = assert_ok!(client, "The pipeline production caller must be a valid source path");
        let client = FileNode::new(client);
        let tests = SourcePath::new(PathBuf::from("tests.rs"));
        let tests = assert_ok!(tests, "The pipeline test-only caller must be a valid source path");
        let expected = BTreeSet::from([
            FileEdge::new(client, target.clone(), DependencyKind::Production),
            FileEdge::new(FileNode::new(tests), target, DependencyKind::Test),
        ]);
        let options = Options::new(false);
        let repair = IndexRepair::new(Path::new("."), None, Path::new("without_test.json"), &options);

        let repaired = repair.perform(index_original, index_without_test);
        let (repaired_original, repaired_without_test) = assert_ok!(repaired, "Repair must accept both parsed pipeline indexes");
        let system_under_test = mark_test_references(repaired_original, &repaired_without_test);
        let system_under_test = assert_ok!(system_under_test, "Repaired pipeline indexes must support reference classification");
        let graph = file_graph(&system_under_test);
        let graph = assert_ok!(graph, "The classified pipeline index must produce a file graph");
        let classified_files = system_under_test.documents().iter().map(|document| std::ptr::from_ref(document.file()));
        let classified_identities: Vec<_> = classified_files.collect();
        let documents_without_test = repaired_without_test.documents();
        let retained_files_without_test = documents_without_test.iter().map(|document| std::ptr::from_ref(document.file()));
        let retained_identities_without_test: Vec<_> = retained_files_without_test.collect();

        assert_eq!(classified_identities, identities_original, "Private stages must retain parsed document identities");
        assert_eq!(retained_identities_without_test, identities_without_test, "Identities without cfg(test) must not be rebuilt");
        assert_eq!(graph.edges(), &expected);
    }

    #[test]
    fn test_qualified_symbol_cache_and_replay() {
        let symbol = "rust-analyzer cargo fixture 0.0.0 run().";
        let fixture = index(symbol, &[("first.rs", &[1]), ("second.rs", &[1]), ("caller.rs", &[0, 0])]);
        let fixture = assert_ok!(fixture, "The qualified symbol cache fixture must parse");
        let original_structure = fixture.symbol_cache().get(symbol).require_structure();
        let original_structure = assert_ok!(original_structure, "Original fixture symbol must have parsed structure").clone();
        let repair_symbols = colliding_symbols(&fixture, &fixture);
        let target = SourcePath::new(PathBuf::from("first.rs"));
        let target = assert_ok!(target, "The qualified symbol target must be a valid source path");
        let target = FileNode::new(target);

        let system_under_test = repair_occurrences(fixture, &repair_symbols, |_| Ok(Some(target.clone())));
        let system_under_test = assert_ok!(system_under_test, "Repair must qualify the colliding cached symbols");
        let unresolved = unresolved_symbols(&system_under_test);
        let unresolved = assert_ok!(unresolved, "Qualified symbols must support unresolved-symbol analysis");
        let definition_count = definitions(&system_under_test).len();
        let retained_original = system_under_test.symbol_cache().get(symbol).require_structure();
        let retained_original = assert_ok!(retained_original, "Repair must retain the original cached symbol structure").clone();
        let cached_symbols = system_under_test.symbols().map(|information| {
            let structure = system_under_test.symbol_cache().get(&information.symbol).require_structure();
            let structure = assert_ok!(structure, "Qualified symbol {} must have a cached structure", information.symbol);
            (information.symbol.clone(), structure.clone())
        });
        let cached_symbols: Vec<_> = cached_symbols.collect();
        let replay_bytes = system_under_test.into_index().write_to_bytes();
        let replay_bytes = assert_ok!(replay_bytes, "The qualified symbol index must serialize for replay");
        let replayed = ParsedIndex::parse(&replay_bytes);
        let replayed = assert_ok!(replayed, "Offline replay must parse the qualified symbol index");

        assert!(unresolved.is_empty(), "Qualified symbols must be immediately available to semantic consumers");
        assert_eq!(definition_count, 2, "Each physical definition must retain its qualified identity");
        assert_eq!(retained_original, original_structure, "Qualification must not mutate the original cached symbol");
        for (qualified_symbol, structure) in cached_symbols {
            let replayed_structure = replayed.symbol_cache().get(&qualified_symbol).require_structure();
            let replayed_structure = assert_ok!(replayed_structure, "Replayed qualified symbol {qualified_symbol} must parse");
            assert_eq!(replayed_structure, &structure, "Cached generated symbols must match their serialized identity");
        }
    }

    #[rstest]
    #[case::qualification_requires_structure(true)]
    #[case::external_target_keeps_opaque_identifier(false)]
    fn test_opaque_repair_resolution(#[case] project_target: bool) {
        let symbol = "synthetic-method";
        let fixture = index(symbol, &[("model.rs", &[]), ("client.rs", &[0, 0])]);
        let fixture = assert_ok!(fixture, "The opaque symbol repair fixture must parse without requiring structure");
        let repair_symbols = BTreeSet::from([symbol.to_owned()]);
        let target = SourcePath::new(PathBuf::from("model.rs"));
        let target = assert_ok!(target, "The opaque symbol target must be a valid source path");
        let target = FileNode::new(target);

        let system_under_test = repair_occurrences(fixture, &repair_symbols, |_| Ok(project_target.then_some(target.clone())));

        assert_eq!(system_under_test.is_err(), project_target, "Only qualification requires a structured symbol");
        if let Ok(index) = system_under_test {
            assert!(index.symbol_cache().get(symbol).structure().is_none(), "External opaque symbols must remain opaque");
        }
    }
}
