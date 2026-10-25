use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use scip::types::SymbolRole;

use crate::diagnostics::emit;
use crate::model::DependencyKind;
use crate::model::FileEdge;
use crate::model::FileGraph;
use crate::scip::definitions::DefinitionFiles;
use crate::scip::definitions::definitions;
use crate::scip::index::ParsedIndex;
use crate::scip::test_dependencies::has_test_classification;

const MISSING_CLASSIFICATION_WARNING: &str =
    "warning: This SCIP index has no test classification. Raw rust-analyzer indices do not mark tests; \
     --exclude-tests cannot identify them. Regenerate with source-mesh to classify dependencies.";

pub fn parse_scip(bytes: &[u8]) -> Result<FileGraph> {
    file_graph(&ParsedIndex::parse(bytes)?)
}

pub(crate) fn file_graph(index: &ParsedIndex) -> Result<FileGraph> {
    ensure!(!index.documents().is_empty(), "SCIP index contains no source documents");
    if !has_test_classification(index) {
        emit(format_args!("{MISSING_CLASSIFICATION_WARNING}"));
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
mod tests {
    use std::collections::BTreeSet;
    use std::env;
    use std::fs;
    use std::io::Write as _;
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::Command;

    use anyhow::Result;
    use claims::assert_ok;
    use claims::assert_some;
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
    use tempfile::NamedTempFile;

    use super::MISSING_CLASSIFICATION_WARNING;
    use super::file_graph;
    use super::parse_scip;
    use crate::SourcePath;
    use crate::model::DependencyKind;
    use crate::model::FileEdge;
    use crate::model::FileNode;
    use crate::scip::definitions::DefinitionFiles;
    use crate::scip::definitions::definitions;
    use crate::scip::index::ParsedIndex;
    use crate::scip::test_dependencies::mark_test_references;

    const ENV_CLASSIFICATION_FIXTURE: &str = "SOURCE_MESH_CLASSIFICATION_FIXTURE";

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

    #[rstest]
    #[case::production(0, DependencyKind::Production)]
    #[case::read_access(SymbolRole::ReadAccess.value(), DependencyKind::Production)]
    #[case::test_only(SymbolRole::Test.value(), DependencyKind::Test)]
    #[case::test_read_access(SymbolRole::Test.value() | SymbolRole::ReadAccess.value(), DependencyKind::Test)]
    fn test_single_reference_kind_bitmasks(#[case] reference_roles: i32, #[case] expected_kind: DependencyKind) {
        let source_path = "source.rs";
        let target_path = "target.rs";
        let symbol = "target";
        let fixture = index_bytes(vec![
            document(source_path, vec![occurrence(symbol, reference_roles)]),
            document(target_path, vec![occurrence(symbol, SymbolRole::Definition.value())]),
        ]);
        let fixture = assert_ok!(fixture, "The reference role fixture must serialize");
        let source = SourcePath::new(PathBuf::from(source_path)).map(FileNode::new);
        let source = assert_ok!(source, "The reference source path must be valid");
        let target = SourcePath::new(PathBuf::from(target_path)).map(FileNode::new);
        let target = assert_ok!(target, "The reference target path must be valid");
        let expected = BTreeSet::from([FileEdge::new(source, target, expected_kind)]);

        let system_under_test = parse_scip(&fixture);
        let system_under_test = assert_ok!(system_under_test, "The reference role fixture must produce a graph");

        assert_eq!(system_under_test.edges(), &expected, "Reference role flags changed its dependency kind");
    }

    #[test]
    fn test_reference_kind_bitmasks_and_mixed_file_pairs() {
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
        ]);
        let index = assert_ok!(index, "The mixed reference role fixture must serialize");
        let source = SourcePath::new(PathBuf::from("source.rs")).map(FileNode::new);
        let source = assert_ok!(source, "The mixed reference source path must be valid");
        let target = SourcePath::new(PathBuf::from("target.rs")).map(FileNode::new);
        let target = assert_ok!(target, "The mixed reference target path must be valid");
        let expected = BTreeSet::from([
            FileEdge::new(source.clone(), target.clone(), DependencyKind::Production),
            FileEdge::new(source, target, DependencyKind::Test),
        ]);

        let system_under_test = parse_scip(&index);
        let system_under_test = assert_ok!(system_under_test, "Mixed reference roles must produce a graph");

        assert_eq!(system_under_test.edges(), &expected, "SCIP lost source reference kinds or counted definitions as references");
    }

    #[rstest]
    #[case(false)]
    #[case(true)]
    fn test_file_graph_document_order(#[case] reverse_documents: bool) {
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
        ]);
        let fixture = assert_ok!(fixture, "The document ordering fixture must serialize");
        let fixture = ParsedIndex::parse(&fixture);
        let mut fixture = assert_ok!(fixture, "The document ordering fixture must parse before reordering");
        let expected_files: BTreeSet<_> = fixture.documents().iter().map(|document| document.file().clone()).collect();
        let source = SourcePath::new(PathBuf::from("caller.rs")).map(FileNode::new);
        let source = assert_ok!(source, "The caller path must be valid");
        let target = SourcePath::new(PathBuf::from("target.rs")).map(FileNode::new);
        let target = assert_ok!(target, "The target path must be valid");
        let expected_edges = BTreeSet::from([
            FileEdge::new(source.clone(), target.clone(), DependencyKind::Production),
            FileEdge::new(source, target, DependencyKind::Test),
        ]);
        if reverse_documents {
            fixture.documents_mut().reverse();
        }

        let system_under_test = file_graph(&fixture);
        let system_under_test = assert_ok!(system_under_test, "Either document ordering must produce a graph");

        assert_eq!(&expected_files, system_under_test.files(), "Graph lost parsed files or added external files");
        assert_eq!(&expected_edges, system_under_test.edges(), "Document order changed endpoints, kinds, or deduplication");
    }

    #[test]
    fn test_physical_definitions_and_relocated_methods() {
        let index = index_bytes(vec![
            document(
                "src/api/entry.rs",
                vec![occurrence("domain/Foo#method().", 0), occurrence("domain/Foo#method().", SymbolRole::ReadAccess.value())],
            ),
            document("src/service/impls.rs", vec![occurrence("domain/Foo#method().", SymbolRole::Definition.value())]),
            document("src/domain/types.rs", vec![occurrence("domain/Foo#", SymbolRole::Definition.value())]),
            document("src/unused/isolated.rs", vec![]),
        ]);
        let index = assert_ok!(index, "The relocated method fixture must serialize");
        let source = SourcePath::new(Path::new("src").join("api").join("entry.rs")).map(FileNode::new);
        let source = assert_ok!(source, "The API entry path must be valid");
        let target = SourcePath::new(Path::new("src").join("service").join("impls.rs")).map(FileNode::new);
        let target = assert_ok!(target, "The relocated implementation path must be valid");
        let expected_dependencies = BTreeSet::from([FileEdge::new(source, target, DependencyKind::Production)]);

        let system_under_test = parse_scip(&index);
        let system_under_test = assert_ok!(system_under_test, "Relocated method definitions must produce a graph");

        assert_eq!(system_under_test.edges(), &expected_dependencies, "Relocated method dependencies differ");
        assert_eq!(system_under_test.files().len(), 4, "SCIP did not preserve all four source documents");
    }

    #[test]
    fn test_definition_bitmasks_and_file_local_symbols() {
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
        ]);
        let index = assert_ok!(index, "The definition role and local symbol fixture must serialize");
        let source = SourcePath::new(PathBuf::from("b.rs")).map(FileNode::new);
        let source = assert_ok!(source, "The global reference source path must be valid");
        let target = SourcePath::new(PathBuf::from("a.rs")).map(FileNode::new);
        let target = assert_ok!(target, "The global definition target path must be valid");
        let expected_dependencies = BTreeSet::from([FileEdge::new(source, target, DependencyKind::Production)]);

        let system_under_test = parse_scip(&index);
        let system_under_test = assert_ok!(system_under_test, "Definition bitmasks and local symbols must produce a graph");

        assert_eq!(system_under_test.edges(), &expected_dependencies, "Definition bitmasks or local symbols changed edges");
    }

    #[test]
    fn test_namespace_qualifiers_and_module_declarations() {
        let mut declarations = document(
            "src/lib.rs",
            vec![occurrence("module/", SymbolRole::Definition.value()), occurrence("namespace/", SymbolRole::Definition.value())],
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
        let index = index_bytes(vec![declarations, caller, target_document]);
        let index = assert_ok!(index, "The namespace declaration fixture must serialize");
        let source = SourcePath::new(Path::new("src").join("caller").join("use.rs")).map(FileNode::new);
        let source = assert_ok!(source, "The namespace caller path must be valid");
        let target = SourcePath::new(Path::new("src").join("module").join("run.rs")).map(FileNode::new);
        let target = assert_ok!(target, "The module implementation path must be valid");
        let expected_dependencies = BTreeSet::from([FileEdge::new(source, target, DependencyKind::Production)]);

        let system_under_test = parse_scip(&index);
        let system_under_test = assert_ok!(system_under_test, "Namespace declarations must not prevent graph construction");

        assert_eq!(system_under_test.edges(), &expected_dependencies, "Namespace declarations changed code dependencies");
    }

    #[test]
    fn test_ambiguous_definitions_across_files() {
        let index = index_bytes(vec![
            document("a.rs", vec![occurrence("ambiguous", SymbolRole::Definition.value())]),
            document("b.rs", vec![occurrence("ambiguous", SymbolRole::Definition.value())]),
            document("caller.rs", vec![occurrence("ambiguous", 0)]),
        ]);
        let index = assert_ok!(index, "The ambiguous definition fixture must serialize before graph validation");

        let system_under_test = parse_scip(&index);

        assert!(system_under_test.is_err(), "Ambiguous definitions were accepted");
        if let Err(error) = system_under_test {
            assert!(error.to_string().contains("multiple files"), "Unexpected ambiguous-symbol diagnostic: {error}");
        }
    }

    #[test]
    fn test_unreferenced_ambiguous_definitions() {
        let fixture = index_bytes(vec![
            document("first.rs", vec![occurrence("main", SymbolRole::Definition.value())]),
            document("second.rs", vec![occurrence("main", SymbolRole::Definition.value())]),
        ]);
        let fixture = assert_ok!(fixture, "The unreferenced duplicate definition fixture must serialize");

        let system_under_test = parse_scip(&fixture);
        let system_under_test = assert_ok!(system_under_test, "Unreferenced ambiguous definitions must permit a graph");

        assert_eq!(system_under_test.files().len(), 2, "Both files with unreferenced definitions must remain in the graph");
        assert!(system_under_test.edges().is_empty(), "Unreferenced duplicate entry points cannot create file dependencies");
    }

    #[test]
    fn test_duplicate_definitions_within_file() {
        let definition = occurrence("same", SymbolRole::Definition.value());
        let index = index_bytes(vec![
            document("a.rs", vec![definition.clone(), definition]),
            document("b.rs", vec![occurrence("same", 0)]),
        ]);
        let index = assert_ok!(index, "The duplicate definition fixture must serialize");
        let source = SourcePath::new(PathBuf::from("b.rs")).map(FileNode::new);
        let source = assert_ok!(source, "The duplicate definition caller path must be valid");
        let target = SourcePath::new(PathBuf::from("a.rs")).map(FileNode::new);
        let target = assert_ok!(target, "The duplicate definition owner path must be valid");
        let expected_dependencies = BTreeSet::from([FileEdge::new(source, target, DependencyKind::Production)]);

        let system_under_test = parse_scip(&index);
        let system_under_test = assert_ok!(system_under_test, "Repeated definitions within one file must produce a graph");

        assert_eq!(system_under_test.edges(), &expected_dependencies, "Duplicate definitions changed dependencies");
    }

    #[rstest]
    #[case(&[0xff])]
    #[case(&[])]
    fn test_invalid_index_data(#[case] index: &[u8]) {
        let system_under_test = parse_scip(index);

        assert!(system_under_test.is_err(), "Empty or malformed SCIP bytes must not produce a graph");
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
    fn test_unsafe_document_paths(#[case] path: &str) {
        let index = index_bytes(vec![document(path, vec![])]);
        let index = assert_ok!(index, "The unsafe document path fixture must serialize before path validation");

        let system_under_test = parse_scip(&index);

        assert!(system_under_test.is_err(), "Invalid document path was accepted: {path}");
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
    fn test_relative_document_path_normalization(#[case] path: &str) {
        let index = index_bytes(vec![document(path, vec![])]);
        let index = assert_ok!(index, "The relative document path fixture must serialize");
        let expected_file = SourcePath::new(Path::new("src").join("file.rs")).map(FileNode::new);
        let expected_file = assert_ok!(expected_file, "The normalized document path must be valid");
        let expected_files = BTreeSet::from([expected_file]);

        let system_under_test = parse_scip(&index);
        let system_under_test = assert_ok!(system_under_test, "The relative document path must produce a graph: {path:?}");

        assert_eq!(system_under_test.files(), &expected_files, "Unexpected normalized source paths");
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
    fn test_document_paths_without_filesystem_validation(#[case] path: &str) {
        let index = index_bytes(vec![document(path, vec![])]);
        let index = assert_ok!(index, "The filesystem-independent document path fixture must serialize");
        let expected_file = SourcePath::new(PathBuf::from(path)).map(FileNode::new);
        let expected_file = assert_ok!(expected_file, "The document path must be lexically valid: {path:?}");
        let expected_files = BTreeSet::from([expected_file]);

        let system_under_test = parse_scip(&index);
        let system_under_test = assert_ok!(system_under_test, "Filesystem naming restrictions must not reject {path:?}");

        assert_eq!(system_under_test.files(), &expected_files, "Document path was validated against filesystem naming rules");
    }

    #[test]
    fn test_normalized_document_identity() {
        let index = index_bytes(vec![
            document("src/file.rs", vec![occurrence("target", SymbolRole::Definition as i32)]),
            document("src/internal/../file.rs", vec![occurrence("target", SymbolRole::Definition as i32)]),
            document("src/other/../caller.rs", vec![occurrence("target", 0)]),
        ]);
        let index = assert_ok!(index, "The normalized document identity fixture must serialize");
        let target = SourcePath::new(Path::new("src").join("file.rs")).map(FileNode::new);
        let target = assert_ok!(target, "The normalized target path must be valid");
        let source = SourcePath::new(Path::new("src").join("caller.rs")).map(FileNode::new);
        let source = assert_ok!(source, "The normalized caller path must be valid");
        let expected_files = BTreeSet::from([source.clone(), target.clone()]);
        let expected_dependencies = BTreeSet::from([FileEdge::new(source, target, DependencyKind::Production)]);

        let system_under_test = parse_scip(&index);
        let system_under_test = assert_ok!(system_under_test, "Equivalent document paths must produce an unambiguous graph");

        assert_eq!(system_under_test.files(), &expected_files, "Equivalent paths produced distinct file nodes");
        assert_eq!(system_under_test.edges(), &expected_dependencies, "Normalized paths changed dependency identity");
    }

    #[test]
    fn test_parsed_document_identity_and_round_trip() {
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
        let target = SourcePath::new(Path::new("src").join("target.rs")).map(FileNode::new);
        let target = assert_ok!(target, "The round-trip definition target path must be valid");
        let bytes = fixture.write_to_bytes();
        let bytes = assert_ok!(bytes, "The round-trip fixture must serialize with unknown fields intact");
        let system_under_test = ParsedIndex::parse(&bytes);
        let mut system_under_test = assert_ok!(system_under_test, "The round-trip fixture must parse");

        system_under_test.documents_mut().reverse();
        let ownership = definitions(&system_under_test);

        assert_eq!(ownership.get("target"), Some(&DefinitionFiles::Unique(target)), "Reordering changed normalized ownership");
        assert_eq!(system_under_test.symbols().count(), 1, "External symbol metadata was lost");

        system_under_test.documents_mut().reverse();
        let restored = system_under_test.into_index();

        assert_eq!(restored, fixture, "Parsed identities must preserve paths, metadata, external symbols, and unknown fields");
    }

    #[rstest]
    #[case::unclassified(false, false, true)]
    #[case::metadata(true, false, false)]
    #[case::test_role(false, true, false)]
    #[case::both(true, true, false)]
    fn test_classification_warning(#[case] metadata: bool, #[case] test_role: bool, #[case] warn: bool) {
        let reference_roles = if test_role { SymbolRole::Test as i32 } else { 0 };
        let reference = Occurrence { range: vec![0, 0, 3], ..occurrence("external/run().", reference_roles) };
        let mut index = Index { documents: vec![document("isolated.rs", vec![reference])], ..Index::default() };
        if metadata {
            let bytes = index.write_to_bytes();
            let bytes = assert_ok!(bytes, "The classification warning fixture must serialize before classification");
            let original = ParsedIndex::parse(&bytes);
            let original = assert_ok!(original, "The original classification warning index must parse");
            let baseline = ParsedIndex::parse(&bytes);
            let baseline = assert_ok!(baseline, "The baseline classification warning index must parse");
            let classified = mark_test_references(original, &baseline);
            let classified = assert_ok!(classified, "The warning fixture must accept classification metadata");
            index = classified.into_index();
        }
        let fixture = NamedTempFile::new();
        let mut fixture = assert_ok!(fixture, "The classification subprocess fixture file must be created");
        let bytes = index.write_to_bytes();
        let bytes = assert_ok!(bytes, "The classification subprocess index must serialize");
        let written = fixture.write_all(&bytes);
        assert_ok!(written, "The classification subprocess fixture bytes must be written");
        let module = module_path!().split_once("::");
        let (_, module) = assert_some!(module, "The classification fixture must be nested in the library test module");
        let filter = format!("{module}::{}", stringify!(test_classification_warning_peer));
        let executable = env::current_exe();
        let executable = assert_ok!(executable, "The library test executable must be located for the warning subprocess");
        let mut command = Command::new(executable);
        command.args(["--exact", &filter, "--ignored", "--nocapture"]);
        command.env(ENV_CLASSIFICATION_FIXTURE, fixture.path());
        let expected = if warn { format!("{MISSING_CLASSIFICATION_WARNING}\n") } else { String::new() };

        let output = command.output();
        let output = assert_ok!(output, "The classification warning subprocess must launch and finish");

        assert!(output.status.success(), "The classification fixture must produce a graph successfully");
        assert_eq!(output.stderr, expected.as_bytes(), "Only unclassified indices require a classification warning");
        let warning_on_stdout = String::from_utf8_lossy(&output.stdout).contains(MISSING_CLASSIFICATION_WARNING);
        assert!(!warning_on_stdout, "Classification warnings must not be emitted on stdout");
    }

    #[test]
    #[ignore = "Executed by classification warning tests in a subprocess"]
    fn test_classification_warning_peer() {
        let Some(path) = env::var_os(ENV_CLASSIFICATION_FIXTURE) else {
            return;
        };
        let fixture = fs::read(path);
        let fixture = assert_ok!(fixture, "The classification warning subprocess must read its supplied fixture");

        let system_under_test = parse_scip(&fixture);
        let system_under_test = assert_ok!(system_under_test, "The classification warning fixture must produce a graph");

        assert_eq!(system_under_test.files().len(), 1, "Warning emission must not discard the source graph");
    }

    #[rstest]
    #[case::no_documents(false)]
    #[case::document_without_occurrences(true)]
    fn test_source_document_graph_boundary(#[case] has_document: bool) {
        let documents = if has_document { vec![document("isolated.rs", vec![])] } else { vec![] };
        let fixture = Index { metadata: Some(Metadata::default()).into(), documents, ..Index::default() }.write_to_bytes();
        let fixture = assert_ok!(fixture, "The source document boundary fixture must serialize");
        let parsed = ParsedIndex::parse(&fixture);
        let parsed = assert_ok!(parsed, "Parsing must accept an index regardless of source document presence");
        let expected_file = SourcePath::new(PathBuf::from("isolated.rs")).map(FileNode::new);
        let expected_file = assert_ok!(expected_file, "The isolated document path must be valid");
        let expected_files = BTreeSet::from([expected_file]);

        let system_under_test = file_graph(&parsed);
        let public_graph = parse_scip(&fixture);

        assert_eq!(parsed.documents().len(), usize::from(has_document), "Parsing must preserve document presence");
        if has_document {
            let graph = assert_ok!(system_under_test, "Graph construction requires documents, not occurrences");
            assert_ok!(public_graph, "The public byte adapter must accept an occurrence-free document");
            assert_eq!(graph.files(), &expected_files, "An occurrence-free source file must remain in the graph");
            assert!(graph.edges().is_empty(), "An occurrence-free document cannot introduce dependencies");
        } else {
            assert!(system_under_test.is_err(), "Graph construction must reject an index without source documents");
            assert!(public_graph.is_err(), "The public byte adapter must reject an index without source documents");
        }
    }
}
