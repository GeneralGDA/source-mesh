use std::collections::BTreeSet;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use protobuf::Message as _;
use scip::types::Index;
use scip::types::SymbolRole;

use crate::model::FileEdge;
use crate::model::FileGraph;
use crate::model::DependencyKind;
use crate::scip_path::source_path;
use crate::scip_definitions::definitions;
use crate::test_dependencies::has_test_classification;

pub fn parse_scip(bytes: &[u8]) -> Result<FileGraph> {
    ensure!(!bytes.is_empty(), "SCIP index is empty");
    let index = Index::parse_from_bytes(bytes).context("Cannot parse SCIP protobuf index")?;
    ensure!(
        !index.documents.is_empty(),
        "SCIP index contains no source documents"
    );
    if !has_test_classification(&index) {
        eprintln!("warning: This SCIP index has no test classification. Raw rust-analyzer indices do not mark tests; --exclude-tests cannot identify them. Regenerate with source-mesh to classify dependencies.");
    }
    let files = index
        .documents
        .iter()
        .map(|document| source_path(&document.relative_path))
        .collect::<Result<Vec<_>>>()?;
    let definitions = definitions(&index)?;
    let mut edges = BTreeSet::new();
    for (document, source) in index.documents.iter().zip(&files) {
        for occurrence in &document.occurrences {
            if occurrence.symbol_roles & SymbolRole::Definition as i32 == 0
                && let Some(candidates) = definitions.get(occurrence.symbol.as_str())
            {
                ensure!(candidates.len() == 1,
                    "Cannot resolve reference to SCIP symbol {:?} in {} at {:?}: rust-analyzer assigned it to multiple files ({}). Run source-mesh on the original project to resolve Cargo target collisions; an offline index cannot recover the missing target identity.",
                    occurrence.symbol, source.path().display(), occurrence.range,
                    candidates.iter().map(|file| file.path().display().to_string()).collect::<Vec<_>>().join(", "));
                let target = candidates.first().context("SCIP definition has no source file")?;
                if source == target { continue; }
                let kind = if occurrence.symbol_roles & SymbolRole::Test as i32 != 0 {
                    DependencyKind::Test
                } else {
                    DependencyKind::Production
                };
                edges.insert(FileEdge::new(source.clone(), target.clone(), kind));
            }
        }
    }
    FileGraph::new(files.into_iter().collect(), edges)
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
    use rstest::rstest;
    use scip::types::Document;
    use scip::types::Index;
    use scip::types::Occurrence;
    use scip::types::SymbolInformation;
    use scip::types::SymbolRole;
    use scip::types::symbol_information::Kind;

    use super::parse_scip;
    use crate::model::DependencyKind;
    use crate::model::FileEdge;
    use crate::model::FileNode;

    #[must_use]
    fn occurrence(symbol: &str, roles: i32) -> Occurrence {
        Occurrence {
            symbol: symbol.into(),
            symbol_roles: roles,
            ..Occurrence::default()
        }
    }

    #[must_use]
    fn document(path: &str, occurrences: Vec<Occurrence>) -> Document {
        Document {
            relative_path: path.into(),
            language: "rust".into(),
            occurrences,
            ..Document::default()
        }
    }

    fn index_bytes(documents: Vec<Document>) -> Result<Vec<u8>> {
        Ok(Index {
            documents,
            ..Index::default()
        }
        .write_to_bytes()?)
    }

    #[test]
    fn test_reference_kind_bitmasks_and_mixed_file_pairs() -> Result<()> {
        let index = index_bytes(vec![
            document("source.rs", vec![
                occurrence("target", SymbolRole::ReadAccess.value()),
                occurrence("target", SymbolRole::Test.value() | SymbolRole::ReadAccess.value()),
                occurrence("target", SymbolRole::Test.value()),
                occurrence("defined_here", SymbolRole::Test.value() | SymbolRole::Definition.value()),
            ]),
            document("target.rs", vec![occurrence("target", SymbolRole::Test.value() | SymbolRole::Definition.value())]),
        ])?;
        let source = FileNode::new(PathBuf::from("source.rs"))?;
        let target = FileNode::new(PathBuf::from("target.rs"))?;
        let expected = BTreeSet::from([
            FileEdge::new(source.clone(), target.clone(), DependencyKind::Production),
            FileEdge::new(source, target, DependencyKind::Test),
        ]);

        let system_under_test = parse_scip(&index)?;

        assert_eq!(system_under_test.edges(), &expected, "SCIP lost source reference kinds or counted definitions as references");
        Ok(())
    }

    #[test]
    fn test_physical_definitions_and_relocated_methods() -> Result<()> {
        let index = index_bytes(vec![
            document(
                "src/api/entry.rs",
                vec![
                    occurrence("domain/Foo#method().", 0),
                    occurrence("domain/Foo#method().", SymbolRole::ReadAccess.value()),
                ],
            ),
            document(
                "src/service/impls.rs",
                vec![occurrence("domain/Foo#method().", SymbolRole::Definition.value())],
            ),
            document(
                "src/domain/types.rs",
                vec![occurrence("domain/Foo#", SymbolRole::Definition.value())],
            ),
            document("src/unused/isolated.rs", vec![]),
        ])?;
        let expected_dependencies = BTreeSet::from([FileEdge::new(
            FileNode::new(Path::new("src").join("api").join("entry.rs"))?,
            FileNode::new(Path::new("src").join("service").join("impls.rs"))?,
            DependencyKind::Production,
        )]);

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
                    occurrence(
                        "global",
                        SymbolRole::Definition.value() | SymbolRole::ReadAccess.value(),
                    ),
                    occurrence("global", 0),
                    occurrence("local 0", SymbolRole::Definition.value()),
                ],
            ),
            document("b.rs", vec![occurrence("global", SymbolRole::ReadAccess.value())]),
            document(
                "c.rs",
                vec![
                    occurrence("local 0", 0),
                    occurrence("external symbol", 0),
                    occurrence("", 0),
                ],
            ),
        ])?;
        let expected_dependencies = BTreeSet::from([FileEdge::new(
            FileNode::new(PathBuf::from("b.rs"))?,
            FileNode::new(PathBuf::from("a.rs"))?,
            DependencyKind::Production,
        )]);

        let system_under_test = parse_scip(&index)?;

        assert_eq!(system_under_test.edges(), &expected_dependencies, "Definition bitmasks or local symbols changed dependencies");
        Ok(())
    }

    #[test]
    fn test_namespace_qualifiers_and_module_declarations() -> Result<()> {
        let mut declarations = document(
            "src/lib.rs",
            vec![
                occurrence("module/", SymbolRole::Definition.value()),
                occurrence("namespace/", SymbolRole::Definition.value()),
            ],
        );
        declarations.symbols = vec![
            SymbolInformation {
                symbol: "module/".into(),
                kind: Kind::Module.into(),
                ..SymbolInformation::default()
            },
            SymbolInformation {
                symbol: "namespace/".into(),
                kind: Kind::Namespace.into(),
                ..SymbolInformation::default()
            },
        ];
        let caller = document(
            "src/caller/use.rs",
            vec![
                occurrence("module/", 0),
                occurrence("namespace/", 0),
                occurrence("module/run().", 0),
            ],
        );
        let target = document(
            "src/module/run.rs",
            vec![occurrence("module/run().", SymbolRole::Definition.value())],
        );
        let index = index_bytes(vec![declarations, caller, target])?;
        let expected_dependencies = BTreeSet::from([FileEdge::new(
            FileNode::new(Path::new("src").join("caller").join("use.rs"))?,
            FileNode::new(Path::new("src").join("module").join("run.rs"))?,
            DependencyKind::Production,
        )]);

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
            assert!(error.to_string().contains("multiple files"), "Unexpected diagnostic for ambiguous symbol definitions: {error}");
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
        let expected_dependencies = BTreeSet::from([FileEdge::new(
            FileNode::new(PathBuf::from("b.rs"))?,
            FileNode::new(PathBuf::from("a.rs"))?,
            DependencyKind::Production,
        )]);

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
    #[case("src/")]
    #[case("../outside.rs")]
    #[case("src/../../outside.rs")]
    #[case("/absolute.rs")]
    #[case("C:\\absolute.rs")]
    #[case("\\\\server\\share\\file.rs")]
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
    fn test_relative_document_path_normalization(#[case] path: &str) -> Result<()> {
        let index = index_bytes(vec![document(path, vec![])])?;
        let expected_files = BTreeSet::from([FileNode::new(Path::new("src").join("file.rs"))?]);

        let system_under_test = parse_scip(&index)?;

        assert_eq!(system_under_test.files(), &expected_files, "Unexpected normalized source paths");
        Ok(())
    }
}
