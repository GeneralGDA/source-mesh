use std::collections::HashSet;

use anyhow::Result;
use anyhow::ensure;
use derive_new::new;
use scip::symbol::is_local_symbol;
use scip::types::SymbolRole;
use scip::types::ToolInfo;

use crate::model::FileNode;
use crate::scip::index::ParsedDocument;
use crate::scip::index::ParsedIndex;
use crate::scip::range::ScipRange;

const CLASSIFICATION_TOOL_NAME: &str = "source-mesh";

#[derive(PartialEq, Eq, Hash, new)]
struct SymbolOccurrence {
    file: FileNode,
    range: ScipRange,
    symbol: String,
}

fn missing_definition_scopes(
    document: &ParsedDocument,
    definitions_without_test: &HashSet<SymbolOccurrence>,
) -> Result<Vec<ScipRange>> {
    let file = document.file();
    let mut scopes = Vec::new();
    // rust-analyzer can attach a referenced target's body as enclosing_range.
    for occurrence in &document.document().occurrences {
        if occurrence.symbol_roles & SymbolRole::Definition as i32 == 0 {
            continue;
        }
        if occurrence.symbol.is_empty() || is_local_symbol(&occurrence.symbol) || occurrence.enclosing_range.is_empty() {
            continue;
        }
        let definition = SymbolOccurrence::new(file.clone(), ScipRange::parse(&occurrence.range)?, occurrence.symbol.clone());
        if definitions_without_test.contains(&definition) {
            continue;
        }
        let scope = ScipRange::parse(&occurrence.enclosing_range)?;
        if scope != definition.range {
            ensure!(scope.contains(&definition.range), "SCIP definition scope omits its name in {}", file.path().display());
            scopes.push(scope);
        }
    }
    Ok(scopes)
}

pub(crate) fn mark_test_references(mut index_original: ParsedIndex, index_without_test: &ParsedIndex) -> Result<ParsedIndex> {
    ensure!(!index_original.documents().is_empty(), "Original SCIP index contains no source documents");

    let mut references_without_test = HashSet::new();
    let mut definitions_without_test = HashSet::new();
    for document in index_without_test.documents() {
        let file = document.file();
        for occurrence in &document.document().occurrences {
            let identity = SymbolOccurrence::new(file.clone(), ScipRange::parse(&occurrence.range)?, occurrence.symbol.clone());
            if occurrence.symbol_roles & SymbolRole::Definition as i32 == 0 {
                references_without_test.insert(identity);
                continue;
            }
            if !identity.symbol.is_empty() && !is_local_symbol(&identity.symbol) {
                definitions_without_test.insert(identity);
            }
        }
    }
    for document in index_original.documents_mut() {
        let file = document.file().clone();
        let test_scopes = missing_definition_scopes(document, &definitions_without_test)?;
        for occurrence in document.occurrences_mut() {
            if occurrence.symbol_roles & SymbolRole::Definition as i32 != 0 {
                continue;
            }
            let identity = SymbolOccurrence::new(file.clone(), ScipRange::parse(&occurrence.range)?, occurrence.symbol.clone());
            let exists_without_test = references_without_test.contains(&identity);
            if !exists_without_test || test_scopes.iter().any(|scope| scope.contains(&identity.range)) {
                occurrence.symbol_roles |= SymbolRole::Test as i32;
            }
        }
    }

    let mut metadata = index_original.metadata_mut().take().unwrap_or_default();
    metadata.tool_info = Some(ToolInfo {
        name: CLASSIFICATION_TOOL_NAME.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        ..ToolInfo::default()
    }).into();
    *index_original.metadata_mut() = Some(metadata).into();
    Ok(index_original)
}

#[must_use]
pub(crate) fn has_test_classification(index: &ParsedIndex) -> bool {
    let tool = index.metadata().as_ref().and_then(|metadata| metadata.tool_info.as_ref());
    if tool.is_some_and(|tool| tool.name == CLASSIFICATION_TOOL_NAME) {
        return true;
    }
    let mut occurrences = index.documents().iter().flat_map(|document| &document.document().occurrences);
    occurrences.any(|occurrence| occurrence.symbol_roles & SymbolRole::Test as i32 != 0)
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use claims::assert_ok;
    use claims::assert_some;
    use pretty_assertions::assert_eq;
    use protobuf::Message as _;
    use rstest::rstest;
    use scip::types::Document;
    use scip::types::Index;
    use scip::types::Metadata;
    use scip::types::Occurrence;
    use scip::types::SymbolRole;
    use scip::types::TextEncoding;
    use scip::types::ToolInfo;

    use super::CLASSIFICATION_TOOL_NAME;
    use super::has_test_classification;
    use super::mark_test_references;
    use crate::scip::index::ParsedIndex;

    const MESSAGE_MISSING_REFERENCE: &str = "The classified index must retain its reference";
    const MESSAGE_MISSING_SOURCE_DOCUMENT: &str = "The classified index must retain its source document";

    #[must_use]
    fn reference(symbol: &str, range: [i32; 3]) -> Occurrence {
        Occurrence { symbol: symbol.into(), range: range.into(), ..Occurrence::default() }
    }

    fn index_bytes(path: &str, occurrences: Vec<Occurrence>) -> Result<Vec<u8>> {
        let document = Document { relative_path: path.into(), occurrences, ..Document::default() };
        Ok(Index { documents: vec![document], ..Index::default() }.write_to_bytes()?)
    }

    fn classify(index_bytes_original: &[u8], index_bytes_without_test: &[u8]) -> Result<ParsedIndex> {
        let index_original = ParsedIndex::parse(index_bytes_original)?;
        let index_without_test = ParsedIndex::parse(index_bytes_without_test)?;
        mark_test_references(index_original, &index_without_test)
    }

    #[rstest]
    #[case(0)]
    #[case(SymbolRole::ReadAccess as i32 | SymbolRole::WriteAccess as i32)]
    fn test_reference_identity_and_definition_roles(#[case] reference_roles: i32) {
        let shared_reference = reference("service/run().", [0, 0, 3]);
        let mut different_range_reference = reference("service/run().", [1, 0, 3]);
        different_range_reference.symbol_roles = reference_roles;
        let mut different_symbol_reference = reference("mock/run().", [0, 0, 3]);
        different_symbol_reference.symbol_roles = reference_roles;
        let mut definition = reference("service/definition().", [2, 0, 3]);
        definition.symbol_roles = SymbolRole::Definition as i32 | SymbolRole::ReadAccess as i32;
        let mut already_classified = reference("service/retained().", [3, 0, 3]);
        already_classified.symbol_roles = SymbolRole::Test as i32 | SymbolRole::ReadAccess as i32;
        let index_original = index_bytes(
            "src/caller.rs",
            vec![
                shared_reference.clone(),
                different_range_reference,
                different_symbol_reference,
                definition.clone(),
                already_classified.clone(),
            ],
        );
        let index_original = assert_ok!(index_original, "The original reference identity fixture must serialize");
        let index_without_test = index_bytes("src/caller.rs", vec![shared_reference, already_classified]);
        let index_without_test = assert_ok!(index_without_test, "The reference identity baseline must serialize");

        let system_under_test = classify(&index_original, &index_without_test);
        let system_under_test = assert_ok!(system_under_test, "Reference identity and definition roles must be classifiable");

        let document = system_under_test.documents().first();
        let document = assert_some!(document, "{MESSAGE_MISSING_SOURCE_DOCUMENT}");
        let roles: Vec<_> = document.document().occurrences.iter().map(|occurrence| occurrence.symbol_roles).collect();
        assert_eq!(
            roles,
            vec![
                0,
                reference_roles | SymbolRole::Test as i32,
                reference_roles | SymbolRole::Test as i32,
                definition.symbol_roles,
                SymbolRole::Test as i32 | SymbolRole::ReadAccess as i32,
            ],
            "Reference identity, existing flags, or definitions were classified incorrectly"
        );
        assert!(has_test_classification(&system_under_test), "Classification provenance missing");
        let tool = system_under_test.metadata().as_ref().and_then(|metadata| metadata.tool_info.as_ref());
        let tool_name = tool.map(|tool| tool.name.as_str());
        assert_eq!(tool_name, Some(CLASSIFICATION_TOOL_NAME), "Classification must install metadata and tool information");
    }

    #[rstest]
    #[case(".\\src\\caller.rs", "src/caller.rs", false)]
    #[case("src/caller.rs", "src/other.rs", true)]
    #[case("src/nested/../caller.rs", "src/caller.rs", false)]
    #[case("src/caller.rs", "./src/internal/../caller.rs", false)]
    fn test_document_path_identity(
        #[case] path_original: &str,
        #[case] path_without_test: &str,
        #[case] expected_test_reference: bool,
    ) {
        let occurrence = reference("service/run().", [0, 0, 3]);
        let index_original = index_bytes(path_original, vec![occurrence.clone()]);
        let index_original = assert_ok!(index_original, "The original document path fixture must serialize");
        let index_without_test = index_bytes(path_without_test, vec![occurrence]);
        let index_without_test = assert_ok!(index_without_test, "The document path baseline must serialize");

        let system_under_test = classify(&index_original, &index_without_test);
        let system_under_test = assert_ok!(system_under_test, "Valid document paths must support reference classification");

        let reference = system_under_test.documents().first().and_then(|document| document.document().occurrences.first());
        let classified_occurrence = assert_some!(reference, "{MESSAGE_MISSING_REFERENCE}");
        let is_test_reference = classified_occurrence.symbol_roles & SymbolRole::Test as i32 != 0;
        assert_eq!(is_test_reference, expected_test_reference, "Document paths were matched incorrectly");
    }

    #[test]
    fn test_definition_reference_identity_without_test() {
        let occurrence = reference("service/run().", [0, 0, 3]);
        let index_original = index_bytes("caller.rs", vec![occurrence.clone()]);
        let index_original = assert_ok!(index_original, "The original reference fixture must serialize");
        let mut definition = occurrence;
        definition.symbol_roles = SymbolRole::Definition as i32;
        let index_without_test = index_bytes("caller.rs", vec![definition]);
        let index_without_test = assert_ok!(index_without_test, "The definition-only baseline must serialize");

        let system_under_test = classify(&index_original, &index_without_test);
        let system_under_test = assert_ok!(system_under_test, "A definition-only baseline must allow reference classification");

        let reference = system_under_test.documents().first().and_then(|document| document.document().occurrences.first());
        let classified_occurrence = assert_some!(reference, "{MESSAGE_MISSING_REFERENCE}");
        let is_test_reference = classified_occurrence.symbol_roles & SymbolRole::Test as i32 != 0;
        assert!(is_test_reference, "Definition from the index without cfg(test) was treated as a matching reference");
    }

    #[test]
    fn test_empty_documents_without_test() {
        let index_original = index_bytes("caller.rs", vec![reference("service/run().", [0, 0, 3])]);
        let index_original = assert_ok!(index_original, "The original reference fixture must serialize before classification");
        let index_without_test = Index { metadata: Some(Metadata::default()).into(), ..Index::default() }.write_to_bytes();
        let index_without_test = assert_ok!(index_without_test, "The baseline without documents must serialize");

        let system_under_test = classify(&index_original, &index_without_test);
        let system_under_test = assert_ok!(system_under_test, "A baseline without documents must allow classification");

        let occurrence = system_under_test.documents().first().and_then(|document| document.document().occurrences.first());
        let occurrence = assert_some!(occurrence, "{MESSAGE_MISSING_REFERENCE}");
        let is_test_reference = occurrence.symbol_roles & SymbolRole::Test as i32 != 0;
        assert!(is_test_reference, "Reference absent from an empty index without cfg(test) was not classified");
    }

    #[rstest]
    #[case::in_memory(false)]
    #[case::saved_analysis(true)]
    fn test_metadata_and_classification_without_test_references(#[case] serialize: bool) {
        let original = Index {
            metadata: Some(Metadata {
                project_root: "file:///workspace".into(),
                text_document_encoding: TextEncoding::UTF8.into(),
                tool_info: Some(ToolInfo { name: "rust-analyzer".into(), ..ToolInfo::default() }).into(),
                ..Metadata::default()
            }).into(),
            documents: vec![Document { relative_path: "caller.rs".into(), ..Document::default() }],
            ..Index::default()
        };
        let index = original.write_to_bytes();
        let index = assert_ok!(index, "The metadata preservation fixture must serialize");
        let parsed = ParsedIndex::parse(&index);
        let parsed = assert_ok!(parsed, "The metadata preservation fixture must parse");
        let original_metadata = parsed.metadata().as_ref();
        let original_metadata = assert_some!(original_metadata, "The fixture must retain its original metadata");

        let system_under_test = classify(&index, &index);
        let system_under_test = assert_ok!(system_under_test, "An index without references must still be classifiable");
        let system_under_test = if serialize {
            let bytes = system_under_test.into_index().write_to_bytes();
            let bytes = assert_ok!(bytes, "The classified index must serialize its metadata");
            let replayed = ParsedIndex::parse(&bytes);
            assert_ok!(replayed, "The serialized classification metadata must parse")
        } else {
            system_under_test
        };

        let metadata = system_under_test.metadata().as_ref();
        let metadata = assert_some!(metadata, "Classification must retain index metadata");
        let tool = metadata.tool_info.as_ref();
        let tool = assert_some!(tool, "Classification must record its tool information");
        assert_eq!(metadata.project_root, original_metadata.project_root, "Source root was changed");
        assert_eq!(metadata.text_document_encoding, original_metadata.text_document_encoding, "Source encoding was changed");
        assert_eq!(tool.name, CLASSIFICATION_TOOL_NAME, "Classification tool identity was not recorded");
        assert_eq!(tool.version, env!("CARGO_PKG_VERSION"), "Classification tool version was not recorded");
        assert!(has_test_classification(&system_under_test), "Empty classification provenance missing");
        assert!(!has_test_classification(&parsed), "Raw index was mistaken for a classified index");
    }

    #[rstest]
    #[case(0, false)]
    #[case(SymbolRole::Test as i32, true)]
    fn test_existing_test_role_detection(#[case] roles: i32, #[case] expected: bool) {
        let mut occurrence = reference("service/run().", [0, 0, 3]);
        occurrence.symbol_roles = roles;
        let bytes = index_bytes("caller.rs", vec![occurrence]);
        let bytes = assert_ok!(bytes, "The existing test role fixture must serialize");
        let index = ParsedIndex::parse(&bytes);
        let index = assert_ok!(index, "The existing test role fixture must parse");

        let system_under_test = has_test_classification(&index);

        assert_eq!(system_under_test, expected, "SCIP Test role detection differs");
    }

    #[test]
    fn test_saved_classification_provenance() {
        let fixture = ParsedIndex::parse(include_bytes!("../fixtures/dependency-graph.scip"));
        let mut fixture = assert_ok!(fixture, "The saved classification fixture must parse before removing its documents");
        fixture.documents_mut().clear();

        let system_under_test = has_test_classification(&fixture);

        assert!(system_under_test, "Saved classification without test occurrences was not recognized");
    }

    #[rstest]
    #[case(SymbolRole::Definition as i32)]
    #[case(SymbolRole::Definition as i32 | SymbolRole::ReadAccess as i32)]
    fn test_inactive_inline_scopes_with_retained_baseline_references(#[case] definition_roles: i32) {
        let before_scopes = reference("shared/run().", [2, 0, 3]);
        let inside_first_scope = reference("shared/run().", [7, 0, 3]);
        let between_scopes = reference("shared/run().", [12, 1, 4]);
        let inside_second_scope = reference("shared/run().", [17, 0, 3]);
        let after_scopes = reference("shared/run().", [22, 1, 4]);
        let mut first_definition = reference("client/first_tests/", [4, 4, 9]);
        first_definition.symbol_roles = definition_roles;
        first_definition.enclosing_range = vec![3, 0, 12, 1];
        let mut second_definition = reference("client/second_tests/", [14, 4, 9]);
        second_definition.symbol_roles = definition_roles;
        second_definition.enclosing_range = vec![13, 0, 22, 1];
        let occurrences = vec![
            first_definition,
            second_definition,
            before_scopes.clone(),
            inside_first_scope.clone(),
            between_scopes.clone(),
            inside_second_scope.clone(),
            after_scopes.clone(),
        ];
        let index_original = index_bytes("caller.rs", occurrences);
        let index_original = assert_ok!(index_original, "The original inactive scope fixture must serialize");
        let retained_references = vec![before_scopes, inside_first_scope, between_scopes, inside_second_scope, after_scopes];
        let index_without_test = index_bytes("caller.rs", retained_references);
        let index_without_test = assert_ok!(index_without_test, "The retained inactive scope reference baseline must serialize");

        let system_under_test = classify(&index_original, &index_without_test);
        let system_under_test = assert_ok!(system_under_test, "Inactive scopes with retained references must be classifiable");

        let document = system_under_test.documents().first();
        let document = assert_some!(document, "{MESSAGE_MISSING_SOURCE_DOCUMENT}");
        let roles: Vec<_> = document.document().occurrences.iter().map(|occurrence| occurrence.symbol_roles).collect();
        let expected = vec![definition_roles, definition_roles, 0, SymbolRole::Test as i32, 0, SymbolRole::Test as i32, 0];
        assert_eq!(roles, expected, "Inactive scope references or adjacent production references were classified incorrectly");
    }

    #[rstest]
    #[case::local("local 0")]
    #[case::empty("")]
    fn test_unnamed_definition_scopes(#[case] symbol: &str, #[values(false, true)] retain_definition: bool) {
        let shared_reference = reference("shared/run().", [7, 0, 3]);
        let mut definition = reference(symbol, [4, 4, 9]);
        definition.symbol_roles = SymbolRole::Definition as i32;
        definition.enclosing_range = vec![3, 0, 12, 1];
        let mut retained_occurrences = vec![shared_reference.clone()];
        if retain_definition {
            retained_occurrences.push(definition.clone());
        }
        let index_original = index_bytes("caller.rs", vec![definition, shared_reference]);
        let index_original = assert_ok!(index_original, "The original unnamed definition fixture must serialize");
        let index_without_test = index_bytes("caller.rs", retained_occurrences);
        let index_without_test = assert_ok!(index_without_test, "The unnamed definition baseline must serialize");

        let system_under_test = classify(&index_original, &index_without_test);
        let system_under_test = assert_ok!(system_under_test, "Unnamed definitions must not prevent classification");

        let document = system_under_test.documents().first();
        let document = assert_some!(document, "{MESSAGE_MISSING_SOURCE_DOCUMENT}");
        let roles: Vec<_> = document.document().occurrences.iter().map(|occurrence| occurrence.symbol_roles).collect();
        assert_eq!(roles, vec![SymbolRole::Definition as i32, 0], "Unnamed definitions must not affect test classification");
    }


    #[rstest]
    #[case("caller.rs", "caller.rs")]
    #[case("src/../caller.rs", r".\nested\..\caller.rs")]
    fn test_retained_definition_scopes(#[case] path_original: &str, #[case] path_without_test: &str) {
        let shared_reference = reference("shared/run().", [7, 0, 3]);
        let mut definition = reference("client/production/", [4, 4, 9]);
        definition.symbol_roles = SymbolRole::Definition as i32;
        definition.enclosing_range = vec![3, 0, 12, 1];
        let index_original = index_bytes(path_original, vec![definition.clone(), shared_reference.clone()]);
        let index_original = assert_ok!(index_original, "The original retained definition fixture must serialize");
        let index_without_test = index_bytes(path_without_test, vec![definition, shared_reference]);
        let index_without_test = assert_ok!(index_without_test, "The retained definition baseline must serialize");

        let system_under_test = classify(&index_original, &index_without_test);
        let system_under_test = assert_ok!(system_under_test, "Retained definition scopes must support classification");

        let reference = system_under_test.documents().first().and_then(|document| document.document().occurrences.last());
        let classified_reference = assert_some!(reference, "{MESSAGE_MISSING_REFERENCE}");
        assert_eq!(classified_reference.symbol_roles, 0, "A retained definition created a test scope");
    }

    #[test]
    fn test_file_module_definition_scopes() {
        let shared_reference = reference("shared/run().", [7, 0, 3]);
        let module = Occurrence {
            symbol: "client/".into(),
            range: vec![0, 0, 13, 0],
            enclosing_range: vec![0, 0, 13, 0],
            symbol_roles: SymbolRole::Definition as i32,
            ..Occurrence::default()
        };
        let index_original = index_bytes("caller.rs", vec![module, shared_reference.clone()]);
        let index_original = assert_ok!(index_original, "The original file-module scope fixture must serialize");
        let index_without_test = index_bytes("caller.rs", vec![shared_reference]);
        let index_without_test = assert_ok!(index_without_test, "The file-module scope baseline must serialize");

        let system_under_test = classify(&index_original, &index_without_test);
        let system_under_test = assert_ok!(system_under_test, "File-module definition ranges must support classification");

        let reference = system_under_test.documents().first().and_then(|document| document.document().occurrences.last());
        let classified_reference = assert_some!(reference, "{MESSAGE_MISSING_REFERENCE}");
        assert_eq!(classified_reference.symbol_roles, 0, "File-module definition classified the whole source file");
    }

    #[test]
    fn test_reference_target_scopes() {
        let mut shared_reference = reference("shared/run().", [7, 0, 3]);
        shared_reference.enclosing_range = vec![3, 0, 12, 1];
        let index = index_bytes("caller.rs", vec![shared_reference]);
        let index = assert_ok!(index, "The reference target scope fixture must serialize");

        let system_under_test = classify(&index, &index);
        let system_under_test = assert_ok!(system_under_test, "Reference target scopes must not prevent classification");

        let reference = system_under_test.documents().first().and_then(|document| document.document().occurrences.first());
        let classified_reference = assert_some!(reference, "{MESSAGE_MISSING_REFERENCE}");
        assert_eq!(classified_reference.symbol_roles, 0, "Reference used its target's definition range as its own scope");
    }

    #[test]
    fn test_equivalent_reference_range_encodings() {
        let short_reference = reference("shared/run().", [7, 0, 3]);
        let mut long_reference = short_reference.clone();
        long_reference.range = vec![7, 0, 7, 3];
        let index_original = index_bytes("caller.rs", vec![short_reference]);
        let index_original = assert_ok!(index_original, "The compact reference range fixture must serialize");
        let index_without_test = index_bytes("caller.rs", vec![long_reference]);
        let index_without_test = assert_ok!(index_without_test, "The explicit reference range baseline must serialize");

        let system_under_test = classify(&index_original, &index_without_test);
        let system_under_test = assert_ok!(system_under_test, "Equivalent reference range encodings must be classifiable");

        let reference = system_under_test.documents().first().and_then(|document| document.document().occurrences.first());
        let classified_reference = assert_some!(reference, "{MESSAGE_MISSING_REFERENCE}");
        assert_eq!(classified_reference.symbol_roles, 0, "Equivalent SCIP range encodings did not match");
    }

    #[rstest]
    #[case::original(false)]
    #[case::without_test(true)]
    fn test_invalid_reference_ranges(#[case] invalid_index_without_test: bool) {
        let path = "caller.rs";
        let occurrence = reference("shared/run().", [7, 0, 3]);
        let valid_bytes = index_bytes(path, vec![occurrence.clone()]);
        let valid_bytes = assert_ok!(valid_bytes, "The valid reference range fixture must serialize");
        let valid = ParsedIndex::parse(&valid_bytes);
        let valid = assert_ok!(valid, "The valid reference range fixture must parse");
        let invalid_occurrence = Occurrence { range: vec![7, 0], ..occurrence };
        let invalid_bytes = index_bytes(path, vec![invalid_occurrence]);
        let invalid_bytes = assert_ok!(invalid_bytes, "The invalid reference range fixture must serialize");
        let invalid = ParsedIndex::parse(&invalid_bytes);
        let invalid = assert_ok!(invalid, "Index parsing must leave reference range validation to classification");
        let (index_original, index_without_test) = if invalid_index_without_test { (valid, invalid) } else { (invalid, valid) };

        let system_under_test = mark_test_references(index_original, &index_without_test);

        assert!(system_under_test.is_err(), "Classification must reject malformed reference ranges in either index");
    }

    #[rstest]
    #[case::absent(&[], false)]
    #[case::malformed(&[3, 0], true)]
    fn test_optional_definition_scopes(#[case] enclosing_range: &[i32], #[case] invalid_scope: bool) {
        let shared_reference = reference("shared/run().", [7, 0, 3]);
        let mut definition = reference("client/tests/", [4, 4, 9]);
        definition.symbol_roles = SymbolRole::Definition as i32;
        definition.enclosing_range = enclosing_range.to_vec();
        let index_original = index_bytes("caller.rs", vec![definition, shared_reference.clone()]);
        let index_original = assert_ok!(index_original, "The original optional definition scope fixture must serialize");
        let index_without_test = index_bytes("caller.rs", vec![shared_reference]);
        let index_without_test = assert_ok!(index_without_test, "The optional definition scope baseline must serialize");

        let system_under_test = classify(&index_original, &index_without_test);

        if invalid_scope {
            assert!(system_under_test.is_err(), "Malformed definition scopes must fail classification");
        } else {
            let index = assert_ok!(system_under_test, "An absent definition scope must allow classification");
            let reference = index.documents().first().and_then(|document| document.document().occurrences.last());
            let reference_roles = reference.map(|occurrence| occurrence.symbol_roles);
            assert_eq!(reference_roles, Some(0), "An absent definition scope must not classify a retained reference as a test");
        }
    }

    #[rstest]
    #[case(&[], &[0x0a, 0x00])]
    #[case(&[0xff], &[0x0a, 0x00])]
    #[case(&[0x0a, 0x00], &[])]
    #[case(&[0x0a, 0x00], &[0xff])]
    #[case(&[0x0a, 0x00], &[0x0a, 0x00])]
    fn test_invalid_index_inputs(#[case] index_original: &[u8], #[case] index_without_test: &[u8]) {
        let system_under_test = classify(index_original, index_without_test);

        assert!(system_under_test.is_err(), "Classification must reject empty, malformed, or document-free original indices");
    }

    #[rstest]
    #[case("")]
    #[case("src/..")]
    #[case("../outside.rs")]
    #[case(r"src\..\..\outside.rs")]
    #[case("C:relative.rs")]
    fn test_invalid_document_paths(#[case] path: &str, #[values(false, true)] invalid_index_without_test: bool) {
        let valid = index_bytes("caller.rs", vec![]);
        let valid = assert_ok!(valid, "The valid document path fixture must serialize");
        let invalid = index_bytes(path, vec![]);
        let invalid = assert_ok!(invalid, "The invalid document path fixture must serialize before path validation");
        let (index_original, index_without_test) = if invalid_index_without_test {
            (&valid, &invalid)
        } else {
            (&invalid, &valid)
        };

        let system_under_test = classify(index_original, index_without_test);

        assert!(system_under_test.is_err(), "Invalid document path was accepted: {path}");
    }
}
