use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use scip::types::SymbolRole;

use crate::diagnostics::emit;
use crate::model::DependencyKind;
use crate::model::FileEdge;
use crate::model::FileGraph;
use crate::scip_definitions::DefinitionFiles;
use crate::scip_definitions::definitions;
use crate::scip_index::ParsedIndex;
use crate::test_dependencies::has_test_classification;

pub fn parse_scip(bytes: &[u8]) -> Result<FileGraph> {
    file_graph(&ParsedIndex::parse(bytes)?)
}

pub(crate) fn file_graph(index: &ParsedIndex) -> Result<FileGraph> {
    ensure!(!index.documents().is_empty(), "SCIP index contains no source documents");
    if !has_test_classification(index) {
        emit(format_args!(
            "warning: This SCIP index has no test classification. Raw rust-analyzer indices do not mark tests; \
             --exclude-tests cannot identify them. Regenerate with source-mesh to classify dependencies."
        ));
    }
    let source_files = index.documents().iter().map(|document| document.file().clone()).collect();
    let mut graph = FileGraph::from_files(source_files);
    let definitions = definitions(index);
    for document in index.documents() {
        let source = document.file();
        for occurrence in &document.document().occurrences {
            if occurrence.symbol_roles & SymbolRole::Definition as i32 == 0
                && let Some(candidates) = definitions.get(occurrence.symbol.as_str())
            {
                let target = match *candidates {
                    DefinitionFiles::Unique(ref target) => target,
                    DefinitionFiles::Ambiguous(ref files) => {
                        let paths: Vec<_> = files.iter().map(|file| file.path().display().to_string()).collect();
                        bail!(
                            "Cannot resolve reference to SCIP symbol {:?} in {} at {:?}: \
                             rust-analyzer assigned it to multiple files ({}). Run source-mesh on the original project \
                             to resolve Cargo target collisions; an offline index cannot recover the missing target identity.",
                            occurrence.symbol,
                            source.path().display(),
                            occurrence.range,
                            paths.join(", ")
                        );
                    }
                };
                if source == target {
                    continue;
                }
                let kind = if occurrence.symbol_roles & SymbolRole::Test as i32 != 0 {
                    DependencyKind::Test
                } else {
                    DependencyKind::Production
                };
                graph.insert_dependency(FileEdge::new(source.clone(), target.clone(), kind));
            }
        }
    }
    Ok(graph)
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Tests use assertions for expectations while returning Result for fallible setup."
)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::path::PathBuf;

    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use protobuf::Enum as _;
    use protobuf::Message as _;
    use protobuf::SpecialFields;
    use rstest::rstest;
    use scip::types::Document;
    use scip::types::Index;
    use scip::types::Metadata;
    use scip::types::Occurrence;
    use scip::types::SymbolInformation;
    use scip::types::SymbolRole;
    use scip::types::symbol_information::Kind;

    use super::file_graph;
    use super::parse_scip;
    use crate::SourcePath;
    use crate::model::DependencyKind;
    use crate::model::FileEdge;
    use crate::model::FileNode;
    use crate::scip_definitions::DefinitionFiles;
    use crate::scip_definitions::definitions;
    use crate::scip_index::ParsedIndex;

    #[must_use]
    fn occurrence(symbol: &str, roles: i32) -> Occurrence {
        Occurrence { symbol: symbol.into(), symbol_roles: roles, ..Occurrence::default() }
    }

    #[must_use]
    fn document(path: &str, occurrences: Vec<Occurrence>) -> Document {
        Document { relative_path: path.into(), language: "rust".into(), occurrences, ..Document::default() }
    }

    fn index_bytes(documents: Vec<Document>) -> Result<Vec<u8>> {
        Ok(Index { documents, ..Index::default() }.write_to_bytes()?)
    }

    #[test]
    fn test_reference_kind_bitmasks_and_mixed_file_pairs() -> Result<()> {
        let index = index_bytes(vec![
            document(
                "source.rs",
                vec![
                    occurrence("target", SymbolRole::ReadAccess.value()),
                    occurrence("target", SymbolRole::Test.value() | SymbolRole::ReadAccess.value()),
                    occurrence("target", SymbolRole::Test.value()),
                    occurrence("defined_here", SymbolRole::Test.value() | SymbolRole::Definition.value()),
                ],
            ),
            document("target.rs", vec![occurrence("target", SymbolRole::Test.value() | SymbolRole::Definition.value())]),
        ])?;
        let source = FileNode::new(SourcePath::new(PathBuf::from("source.rs"))?);
        let target = FileNode::new(SourcePath::new(PathBuf::from("target.rs"))?);
        let expected = BTreeSet::from([
            FileEdge::new(source.clone(), target.clone(), DependencyKind::Production),
            FileEdge::new(source, target, DependencyKind::Test),
        ]);

        let system_under_test = parse_scip(&index)?;

        assert_eq!(
            system_under_test.edges(), &expected, "SCIP lost source reference kinds or counted definitions as references"
        );
        Ok(())
    }

    #[rstest]
    #[case(false)]
    #[case(true)]
    fn test_file_graph_document_order(#[case] reverse_documents: bool) -> Result<()> {
        let fixture = index_bytes(vec![
            document(
                "caller.rs",
                vec![
                    occurrence("target", 0),
                    occurrence("target", 0),
                    occurrence("target", SymbolRole::Test.value()),
                    occurrence("external", 0),
                ],
            ),
            document("target.rs", vec![occurrence("target", SymbolRole::Definition.value())]),
            document("isolated.rs", vec![]),
        ])?;
        let mut fixture = ParsedIndex::parse(&fixture)?;
        let expected_files: BTreeSet<_> = fixture.documents().iter().map(|document| document.file().clone()).collect();
        let source = FileNode::new(SourcePath::new(PathBuf::from("caller.rs"))?);
        let target = FileNode::new(SourcePath::new(PathBuf::from("target.rs"))?);
        let expected_edges = BTreeSet::from([
            FileEdge::new(source.clone(), target.clone(), DependencyKind::Production),
            FileEdge::new(source, target, DependencyKind::Test),
        ]);
        if reverse_documents {
            fixture.documents_mut().reverse();
        }

        let system_under_test = file_graph(&fixture)?;

        assert_eq!(
            &expected_files, system_under_test.files(), "Graph construction lost parsed files or introduced external files"
        );
        assert_eq!(
            &expected_edges,
            system_under_test.edges(),
            "Document order changed dependency endpoints, kinds, or deduplication"
        );
        Ok(())
    }

    #[test]
    fn test_physical_definitions_and_relocated_methods() -> Result<()> {
        let index = index_bytes(vec![
            document(
                "src/api/entry.rs",
                vec![occurrence("domain/Foo#method().", 0), occurrence("domain/Foo#method().", SymbolRole::ReadAccess.value())],
            ),
            document("src/service/impls.rs", vec![occurrence("domain/Foo#method().", SymbolRole::Definition.value())]),
            document("src/domain/types.rs", vec![occurrence("domain/Foo#", SymbolRole::Definition.value())]),
            document("src/unused/isolated.rs", vec![]),
        ])?;
        let source = FileNode::new(SourcePath::new(Path::new("src").join("api").join("entry.rs"))?);
        let target = FileNode::new(SourcePath::new(Path::new("src").join("service").join("impls.rs"))?);
        let expected_dependencies = BTreeSet::from([FileEdge::new(source, target, DependencyKind::Production)]);

        let system_under_test = parse_scip(&index)?;

        assert_eq!(system_under_test.edges(), &expected_dependencies, "Relocated method dependencies differ");
        assert_eq!(system_under_test.files().len(), 4, "SCIP did not preserve all four source documents");
        Ok(())
    }

    #[test]
    fn test_definition_bitmasks_and_file_local_symbols() -> Result<()> {
        let index = index_bytes(vec![
            document(
                "a.rs",
                vec![
                    occurrence("global", SymbolRole::Definition.value() | SymbolRole::ReadAccess.value()),
                    occurrence("global", 0),
                    occurrence("local 0", SymbolRole::Definition.value()),
                ],
            ),
            document("b.rs", vec![occurrence("global", SymbolRole::ReadAccess.value())]),
            document("c.rs", vec![occurrence("local 0", 0), occurrence("external symbol", 0), occurrence("", 0)]),
        ])?;
        let source = FileNode::new(SourcePath::new(PathBuf::from("b.rs"))?);
        let target = FileNode::new(SourcePath::new(PathBuf::from("a.rs"))?);
        let expected_dependencies = BTreeSet::from([FileEdge::new(source, target, DependencyKind::Production)]);

        let system_under_test = parse_scip(&index)?;

        assert_eq!(
            system_under_test.edges(), &expected_dependencies, "Definition bitmasks or local symbols changed dependencies"
        );
        Ok(())
    }

    #[test]
    fn test_namespace_qualifiers_and_module_declarations() -> Result<()> {
        let mut declarations = document(
            "src/lib.rs",
            vec![
                occurrence("module/", SymbolRole::Definition.value()), occurrence("namespace/", SymbolRole::Definition.value())
            ],
        );
        declarations.symbols = vec![
            SymbolInformation { symbol: "module/".into(), kind: Kind::Module.into(), ..SymbolInformation::default() },
            SymbolInformation { symbol: "namespace/".into(), kind: Kind::Namespace.into(), ..SymbolInformation::default() },
        ];
        let caller = document(
            "src/caller/use.rs",
            vec![occurrence("module/", 0), occurrence("namespace/", 0), occurrence("module/run().", 0)],
        );
        let target_document = document("src/module/run.rs", vec![occurrence("module/run().", SymbolRole::Definition.value())]);
        let index = index_bytes(vec![declarations, caller, target_document])?;
        let source = FileNode::new(SourcePath::new(Path::new("src").join("caller").join("use.rs"))?);
        let target = FileNode::new(SourcePath::new(Path::new("src").join("module").join("run.rs"))?);
        let expected_dependencies = BTreeSet::from([FileEdge::new(source, target, DependencyKind::Production)]);

        let system_under_test = parse_scip(&index)?;

        assert_eq!(system_under_test.edges(), &expected_dependencies, "Namespace declarations changed code dependencies");
        Ok(())
    }

    #[test]
    fn test_ambiguous_definitions_across_files() -> Result<()> {
        let index = index_bytes(vec![
            document("a.rs", vec![occurrence("ambiguous", SymbolRole::Definition.value())]),
            document("b.rs", vec![occurrence("ambiguous", SymbolRole::Definition.value())]),
            document("caller.rs", vec![occurrence("ambiguous", 0)]),
        ])?;

        let system_under_test = parse_scip(&index);

        assert!(system_under_test.is_err(), "Ambiguous definitions were accepted");
        if let Err(error) = system_under_test {
            assert!(
                error.to_string().contains("multiple files"), "Unexpected diagnostic for ambiguous symbol definitions: {error}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_unreferenced_ambiguous_definitions() -> Result<()> {
        let fixture = index_bytes(vec![
            document("first.rs", vec![occurrence("main", SymbolRole::Definition.value())]),
            document("second.rs", vec![occurrence("main", SymbolRole::Definition.value())]),
        ])?;

        let system_under_test = parse_scip(&fixture)?;

        assert_eq!(system_under_test.files().len(), 2);
        assert!(system_under_test.edges().is_empty(), "Unreferenced duplicate entry points cannot create file dependencies");
        Ok(())
    }

    #[test]
    fn test_duplicate_definitions_within_file() -> Result<()> {
        let definition = occurrence("same", SymbolRole::Definition.value());
        let index = index_bytes(vec![
            document("a.rs", vec![definition.clone(), definition]),
            document("b.rs", vec![occurrence("same", 0)]),
        ])?;
        let source = FileNode::new(SourcePath::new(PathBuf::from("b.rs"))?);
        let target = FileNode::new(SourcePath::new(PathBuf::from("a.rs"))?);
        let expected_dependencies = BTreeSet::from([FileEdge::new(source, target, DependencyKind::Production)]);

        let system_under_test = parse_scip(&index)?;

        assert_eq!(system_under_test.edges(), &expected_dependencies, "Duplicate definitions changed dependencies");
        Ok(())
    }

    #[rstest]
    #[case(&[0xff])]
    #[case(&[])]
    fn test_invalid_index_data(#[case] index: &[u8]) {
        let system_under_test = parse_scip(index);

        assert!(system_under_test.is_err());
    }

    #[rstest]
    #[case("")]
    #[case(".")]
    #[case("src/..")]
    #[case("../outside.rs")]
    #[case("src/../../outside.rs")]
    #[case("../src/../outside.rs")]
    #[case("..\\outside.rs")]
    #[case("src\\..\\..\\outside.rs")]
    #[case("src/../..\\outside.rs")]
    #[case("/absolute.rs")]
    #[case("/../absolute.rs")]
    #[case("\\absolute.rs")]
    #[case("C:\\absolute.rs")]
    #[case("C:/absolute.rs")]
    #[case("C:relative.rs")]
    #[case("\\\\server\\share\\file.rs")]
    #[case("//server/share/file.rs")]
    #[case("\\\\?\\C:\\absolute.rs")]
    #[case("\\\\?\\UNC\\server\\share\\file.rs")]
    #[case("\\\\.\\device")]
    fn test_unsafe_document_paths(#[case] path: &str) -> Result<()> {
        let index = index_bytes(vec![document(path, vec![])])?;

        let system_under_test = parse_scip(&index);

        assert!(system_under_test.is_err(), "Invalid document path was accepted: {path}");
        Ok(())
    }

    #[rstest]
    #[case(".\\src\\file.rs")]
    #[case("./src/file.rs")]
    #[case("src\\file.rs")]
    #[case("src/file.rs")]
    #[case("src//./file.rs/")]
    #[case("src\\file.rs\\")]
    #[case("src/internal/../file.rs")]
    #[case("src\\internal\\..\\file.rs")]
    #[case("src/internal\\../file.rs")]
    #[case("other/../src/file.rs")]
    #[case("src/internal/../../src/file.rs")]
    fn test_relative_document_path_normalization(#[case] path: &str) -> Result<()> {
        let index = index_bytes(vec![document(path, vec![])])?;
        let expected_files = BTreeSet::from([FileNode::new(SourcePath::new(Path::new("src").join("file.rs"))?)]);

        let system_under_test = parse_scip(&index)?;

        assert_eq!(system_under_test.files(), &expected_files, "Unexpected normalized source paths");
        Ok(())
    }

    #[rstest]
    #[case("src/")]
    #[case("file:name.rs")]
    #[case("file\0.rs")]
    #[case("CON")]
    #[case("file?.rs")]
    #[case("file*.rs")]
    #[case("file. ")]
    #[case("..hidden.rs")]
    fn test_document_paths_without_filesystem_validation(#[case] path: &str) -> Result<()> {
        let index = index_bytes(vec![document(path, vec![])])?;
        let expected_files = BTreeSet::from([FileNode::new(SourcePath::new(PathBuf::from(path))?)]);

        let system_under_test = parse_scip(&index)?;

        assert_eq!(system_under_test.files(), &expected_files, "Document path was validated against filesystem naming rules");
        Ok(())
    }

    #[test]
    fn test_normalized_document_identity() -> Result<()> {
        let index = index_bytes(vec![
            document("src/file.rs", vec![occurrence("target", SymbolRole::Definition as i32)]),
            document("src/internal/../file.rs", vec![occurrence("target", SymbolRole::Definition as i32)]),
            document("src/other/../caller.rs", vec![occurrence("target", 0)]),
        ])?;
        let target = FileNode::new(SourcePath::new(Path::new("src").join("file.rs"))?);
        let source = FileNode::new(SourcePath::new(Path::new("src").join("caller.rs"))?);
        let expected_files = BTreeSet::from([source.clone(), target.clone()]);
        let expected_dependencies = BTreeSet::from([FileEdge::new(source, target, DependencyKind::Production)]);

        let system_under_test = parse_scip(&index)?;

        assert_eq!(system_under_test.files(), &expected_files, "Equivalent paths produced distinct file nodes");
        assert_eq!(system_under_test.edges(), &expected_dependencies, "Normalized paths changed dependency identity");
        Ok(())
    }

    #[test]
    fn test_parsed_document_identity_and_round_trip() -> Result<()> {
        let mut special_fields = SpecialFields::new();
        special_fields.mut_unknown_fields().add_varint(100, 42);
        special_fields.mut_unknown_fields().add_length_delimited(101, b"unknown payload".to_vec());
        let metadata = Metadata {
            project_root: "file:///fixture".into(),
            special_fields: special_fields.clone(),
            ..Metadata::default()
        };
        let definition = occurrence("target", SymbolRole::Definition as i32);
        let mut target_document = document("src/internal/../target.rs", vec![definition]);
        target_document.special_fields = special_fields.clone();
        let external_symbol = SymbolInformation {
            symbol: "external".into(),
            special_fields: special_fields.clone(),
            ..SymbolInformation::default()
        };
        let fixture = Index {
            metadata: Some(metadata).into(),
            documents: vec![target_document, document("./src/caller.rs", vec![occurrence("target", 0)])],
            external_symbols: vec![external_symbol],
            special_fields,
        };
        let target = FileNode::new(SourcePath::new(Path::new("src").join("target.rs"))?);
        let mut system_under_test = ParsedIndex::parse(&fixture.write_to_bytes()?)?;

        system_under_test.documents_mut().reverse();
        let ownership = definitions(&system_under_test);

        assert_eq!(ownership.get("target"), Some(&DefinitionFiles::Unique(target)));
        assert_eq!(system_under_test.symbols().count(), 1, "External symbol metadata was lost");

        system_under_test.documents_mut().reverse();
        let restored = system_under_test.into_index();

        assert_eq!(restored, fixture, "Parsed identities must preserve paths, metadata, external symbols, and unknown fields");
        Ok(())
    }

    #[test]
    fn test_empty_index_without_test_graph_boundary() -> Result<()> {
        let fixture = Index { metadata: Some(Metadata::default()).into(), ..Index::default() }.write_to_bytes()?;
        let system_under_test = ParsedIndex::parse(&fixture)?;

        let graph = file_graph(&system_under_test);
        let public_graph = parse_scip(&fixture);

        assert!(system_under_test.documents().is_empty(), "An empty index without cfg(test) must remain representable");
        assert!(graph.is_err(), "An index without source documents cannot produce a graph");
        assert!(public_graph.is_err(), "The public byte adapter must preserve source-document validation");
        Ok(())
    }
}
