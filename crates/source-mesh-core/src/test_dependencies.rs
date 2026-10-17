use std::collections::HashSet;

use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use derive_new::new;
use scip::symbol::is_local_symbol;
use scip::types::SymbolRole;
use scip::types::ToolInfo;

use crate::model::FileNode;
use crate::scip_index::ParsedDocument;
use crate::scip_index::ParsedIndex;

const CLASSIFICATION_TOOL_NAME: &str = "source-mesh";

#[derive(PartialEq, Eq, Hash, new)]
struct SymbolOccurrence {
    file: FileNode,
    range: ScipRange,
    symbol: String,
}

#[derive(PartialEq, Eq, Hash)]
struct ScipRange {
    start: (i32, i32),
    end: (i32, i32),
}

impl ScipRange {
    fn parse(coordinates: &[i32]) -> Result<Self> {
        let (start, end) = match *coordinates {
            [line, start_column, end_column] => ((line, start_column), (line, end_column)),
            [start_line, start_column, end_line, end_column] => ((start_line, start_column), (end_line, end_column)),
            _ => bail!("SCIP range must contain three or four coordinates: {coordinates:?}"),
        };
        let ordered_nonnegative = coordinates.iter().all(|coordinate| *coordinate >= 0) && start <= end;
        ensure!(ordered_nonnegative, "SCIP range must contain ordered nonnegative positions: {coordinates:?}");
        Ok(Self { start, end })
    }

    #[must_use]
    fn contains(&self, range: &Self) -> bool {
        self.start <= range.start && range.end <= self.end && range.start < range.end
    }
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
            ensure!(
                scope.contains(&definition.range),
                "SCIP definition scope does not contain its name in {}",
                file.path().display()
            );
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
#[expect(
    clippy::panic_in_result_fn,
    reason = "Tests use assertions for expectations while returning Result for fallible setup."
)]
mod tests {
    use anyhow::Context as _;
    use anyhow::Result;
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
    use super::ScipRange;
    use super::has_test_classification;
    use super::mark_test_references;
    use crate::scip_index::ParsedIndex;

    const MISSING_REFERENCE: &str = "Reference missing";
    const MISSING_SOURCE_DOCUMENT: &str = "Source document missing";

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

    #[test]
    fn test_reference_identity_and_definition_roles() -> Result<()> {
        let shared_reference = reference("service/run().", [0, 0, 3]);
        let different_range_reference = reference("service/run().", [1, 0, 3]);
        let different_symbol_reference = reference("mock/run().", [0, 0, 3]);
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
        )?;
        let index_without_test = index_bytes("src/caller.rs", vec![shared_reference, already_classified])?;

        let system_under_test = classify(&index_original, &index_without_test)?;

        let document = system_under_test.documents().first().context(MISSING_SOURCE_DOCUMENT)?;
        let roles: Vec<_> = document.document().occurrences.iter().map(|occurrence| occurrence.symbol_roles).collect();
        assert_eq!(
            roles,
            vec![
                0,
                SymbolRole::Test as i32,
                SymbolRole::Test as i32,
                definition.symbol_roles,
                SymbolRole::Test as i32 | SymbolRole::ReadAccess as i32,
            ],
            "Reference identity, existing flags, or definitions were classified incorrectly"
        );
        assert!(has_test_classification(&system_under_test), "Classification provenance missing");
        let tool = system_under_test.metadata().as_ref().and_then(|metadata| metadata.tool_info.as_ref());
        let tool_name = tool.map(|tool| tool.name.as_str());
        assert_eq!(tool_name, Some(CLASSIFICATION_TOOL_NAME), "Classification must install metadata and tool information");
        Ok(())
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
    ) -> Result<()> {
        let occurrence = reference("service/run().", [0, 0, 3]);
        let index_original = index_bytes(path_original, vec![occurrence.clone()])?;
        let index_without_test = index_bytes(path_without_test, vec![occurrence])?;

        let system_under_test = classify(&index_original, &index_without_test)?;

        let reference = system_under_test.documents().first().and_then(|document| document.document().occurrences.first());
        let classified_occurrence = reference.context(MISSING_REFERENCE)?;
        let is_test_reference = classified_occurrence.symbol_roles & SymbolRole::Test as i32 != 0;
        assert_eq!(is_test_reference, expected_test_reference, "Document paths were matched incorrectly");
        Ok(())
    }

    #[test]
    fn test_definition_reference_identity_without_test() -> Result<()> {
        let occurrence = reference("service/run().", [0, 0, 3]);
        let index_original = index_bytes("caller.rs", vec![occurrence.clone()])?;
        let mut definition = occurrence;
        definition.symbol_roles = SymbolRole::Definition as i32;
        let index_without_test = index_bytes("caller.rs", vec![definition])?;

        let system_under_test = classify(&index_original, &index_without_test)?;

        let reference = system_under_test.documents().first().and_then(|document| document.document().occurrences.first());
        let classified_occurrence = reference.context(MISSING_REFERENCE)?;
        let is_test_reference = classified_occurrence.symbol_roles & SymbolRole::Test as i32 != 0;
        assert!(is_test_reference, "Definition from the index without cfg(test) was treated as a matching reference");
        Ok(())
    }

    #[test]
    fn test_empty_documents_without_test() -> Result<()> {
        let index_original = index_bytes("caller.rs", vec![reference("service/run().", [0, 0, 3])])?;
        let index_without_test = Index { metadata: Some(Metadata::default()).into(), ..Index::default() }.write_to_bytes()?;

        let system_under_test = classify(&index_original, &index_without_test)?;

        let occurrence = system_under_test.documents().first().and_then(|document| document.document().occurrences.first());
        let occurrence = occurrence.context(MISSING_REFERENCE)?;
        let is_test_reference = occurrence.symbol_roles & SymbolRole::Test as i32 != 0;
        assert!(is_test_reference, "Reference absent from an empty index without cfg(test) was not classified");
        Ok(())
    }

    #[test]
    fn test_metadata_and_classification_without_test_references() -> Result<()> {
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
        let index = original.write_to_bytes()?;
        let parsed = ParsedIndex::parse(&index)?;
        let original_metadata = parsed.metadata().as_ref().context("Fixture metadata missing")?;

        let system_under_test = classify(&index, &index)?;

        let metadata = system_under_test.metadata().as_ref().context("Metadata missing")?;
        assert_eq!(metadata.project_root, original_metadata.project_root, "Source root was changed");
        assert_eq!(metadata.text_document_encoding, original_metadata.text_document_encoding, "Source encoding was changed");
        assert!(has_test_classification(&system_under_test), "Empty classification provenance missing");
        assert!(!has_test_classification(&parsed), "Raw index was mistaken for a classified index");
        Ok(())
    }

    #[rstest]
    #[case(0, false)]
    #[case(SymbolRole::Test as i32, true)]
    fn test_existing_test_role_detection(#[case] roles: i32, #[case] expected: bool) -> Result<()> {
        let mut occurrence = reference("service/run().", [0, 0, 3]);
        occurrence.symbol_roles = roles;
        let index = ParsedIndex::parse(&index_bytes("caller.rs", vec![occurrence])?)?;

        let system_under_test = has_test_classification(&index);

        assert_eq!(system_under_test, expected, "SCIP Test role detection differs");
        Ok(())
    }

    #[test]
    fn test_saved_classification_provenance() -> Result<()> {
        let mut fixture = ParsedIndex::parse(include_bytes!("fixtures/dependency-graph.scip"))?;
        fixture.documents_mut().clear();

        let system_under_test = has_test_classification(&fixture);

        assert!(system_under_test, "Saved classification without test occurrences was not recognized");
        Ok(())
    }

    #[test]
    fn test_inactive_inline_scope_with_retained_baseline_references() -> Result<()> {
        let before_scope = reference("shared/run().", [2, 0, 3]);
        let inside_scope = reference("shared/run().", [7, 0, 3]);
        let after_scope = reference("shared/run().", [12, 1, 4]);
        let mut scope_definition = reference("client/tests/", [4, 4, 9]);
        scope_definition.symbol_roles = SymbolRole::Definition as i32;
        scope_definition.enclosing_range = vec![3, 0, 12, 1];
        let occurrences = vec![scope_definition, before_scope.clone(), inside_scope.clone(), after_scope.clone()];
        let index_original = index_bytes("caller.rs", occurrences)?;
        let index_without_test = index_bytes("caller.rs", vec![before_scope, inside_scope, after_scope])?;

        let system_under_test = classify(&index_original, &index_without_test)?;

        let document = system_under_test.documents().first().context(MISSING_SOURCE_DOCUMENT)?;
        let roles: Vec<_> = document.document().occurrences.iter().map(|occurrence| occurrence.symbol_roles).collect();
        let expected = vec![SymbolRole::Definition as i32, 0, SymbolRole::Test as i32, 0];
        assert_eq!(roles, expected, "Inactive scope references or adjacent production references were classified incorrectly");
        Ok(())
    }

    #[rstest]
    #[case("local 0")]
    #[case("")]
    fn test_unnamed_definition_scopes(#[case] symbol: &str) -> Result<()> {
        let shared_reference = reference("shared/run().", [7, 0, 3]);
        let mut definition = reference(symbol, [4, 4, 9]);
        definition.symbol_roles = SymbolRole::Definition as i32;
        definition.enclosing_range = vec![3, 0, 12, 1];
        let index_original = index_bytes("caller.rs", vec![definition, shared_reference.clone()])?;
        let index_without_test = index_bytes("caller.rs", vec![shared_reference])?;

        let system_under_test = classify(&index_original, &index_without_test)?;

        let reference = system_under_test.documents().first().and_then(|document| document.document().occurrences.last());
        let classified_reference = reference.context(MISSING_REFERENCE)?;
        assert_eq!(classified_reference.symbol_roles, 0, "An unnamed definition created a test scope");
        Ok(())
    }

    #[rstest]
    #[case("caller.rs", "caller.rs")]
    #[case("src/../caller.rs", r".\nested\..\caller.rs")]
    fn test_retained_definition_scopes(#[case] path_original: &str, #[case] path_without_test: &str) -> Result<()> {
        let shared_reference = reference("shared/run().", [7, 0, 3]);
        let mut definition = reference("client/production/", [4, 4, 9]);
        definition.symbol_roles = SymbolRole::Definition as i32;
        definition.enclosing_range = vec![3, 0, 12, 1];
        let index_original = index_bytes(path_original, vec![definition.clone(), shared_reference.clone()])?;
        let index_without_test = index_bytes(path_without_test, vec![definition, shared_reference])?;

        let system_under_test = classify(&index_original, &index_without_test)?;

        let reference = system_under_test.documents().first().and_then(|document| document.document().occurrences.last());
        let classified_reference = reference.context(MISSING_REFERENCE)?;
        assert_eq!(classified_reference.symbol_roles, 0, "A retained definition created a test scope");
        Ok(())
    }

    #[test]
    fn test_file_module_definition_scopes() -> Result<()> {
        let shared_reference = reference("shared/run().", [7, 0, 3]);
        let module = Occurrence {
            symbol: "client/".into(),
            range: vec![0, 0, 13, 0],
            enclosing_range: vec![0, 0, 13, 0],
            symbol_roles: SymbolRole::Definition as i32,
            ..Occurrence::default()
        };
        let index_original = index_bytes("caller.rs", vec![module, shared_reference.clone()])?;
        let index_without_test = index_bytes("caller.rs", vec![shared_reference])?;

        let system_under_test = classify(&index_original, &index_without_test)?;

        let reference = system_under_test.documents().first().and_then(|document| document.document().occurrences.last());
        let classified_reference = reference.context(MISSING_REFERENCE)?;
        assert_eq!(
            classified_reference.symbol_roles, 0,
            "A file-module definition incorrectly classified the entire source file"
        );
        Ok(())
    }

    #[test]
    fn test_reference_target_scopes() -> Result<()> {
        let mut shared_reference = reference("shared/run().", [7, 0, 3]);
        shared_reference.enclosing_range = vec![3, 0, 12, 1];
        let index = index_bytes("caller.rs", vec![shared_reference])?;

        let system_under_test = classify(&index, &index)?;

        let reference = system_under_test.documents().first().and_then(|document| document.document().occurrences.first());
        let classified_reference = reference.context(MISSING_REFERENCE)?;
        assert_eq!(
            classified_reference.symbol_roles, 0,
            "A reference target's definition range was used as the reference's own scope"
        );
        Ok(())
    }

    #[test]
    fn test_equivalent_reference_range_encodings() -> Result<()> {
        let short_reference = reference("shared/run().", [7, 0, 3]);
        let mut long_reference = short_reference.clone();
        long_reference.range = vec![7, 0, 7, 3];
        let index_original = index_bytes("caller.rs", vec![short_reference])?;
        let index_without_test = index_bytes("caller.rs", vec![long_reference])?;

        let system_under_test = classify(&index_original, &index_without_test)?;

        let reference = system_under_test.documents().first().and_then(|document| document.document().occurrences.first());
        let classified_reference = reference.context(MISSING_REFERENCE)?;
        assert_eq!(classified_reference.symbol_roles, 0, "Equivalent SCIP range encodings did not match");
        Ok(())
    }

    #[rstest]
    #[case(&[3, 0, 1], true)]
    #[case(&[12, 0, 1], true)]
    #[case(&[4, 0, 8, 1], true)]
    #[case(&[3, 0, 12, 1], true)]
    #[case(&[12, 1, 2], false)]
    #[case(&[2, 0, 3], false)]
    #[case(&[2, 0, 4, 0], false)]
    #[case(&[11, 0, 12, 2], false)]
    #[case(&[7, 0, 0], false)]
    fn test_scope_range_containment(#[case] coordinates: &[i32], #[case] expected: bool) -> Result<()> {
        let reference_range = ScipRange::parse(coordinates)?;
        let system_under_test = ScipRange::parse(&[3, 0, 12, 1])?;

        let contains_reference = system_under_test.contains(&reference_range);

        assert_eq!(contains_reference, expected, "Scope containment was incorrect for {coordinates:?}");
        Ok(())
    }

    #[rstest]
    #[case(&[])]
    #[case(&[0, 0])]
    #[case(&[0, 0, 1, 0, 1])]
    #[case(&[-1, 0, 3])]
    #[case(&[0, 4, 3])]
    #[case(&[4, 0, 3, 0])]
    fn test_invalid_scope_ranges(#[case] coordinates: &[i32]) {
        let system_under_test = ScipRange::parse(coordinates);

        assert!(system_under_test.is_err());
    }

    #[rstest]
    #[case(&[], &[0x0a, 0x00])]
    #[case(&[0xff], &[0x0a, 0x00])]
    #[case(&[0x0a, 0x00], &[])]
    #[case(&[0x0a, 0x00], &[0xff])]
    #[case(&[0x0a, 0x00], &[0x0a, 0x00])]
    fn test_invalid_index_inputs(#[case] index_original: &[u8], #[case] index_without_test: &[u8]) {
        let system_under_test = classify(index_original, index_without_test);

        assert!(system_under_test.is_err());
    }

    #[rstest]
    #[case("")]
    #[case("src/..")]
    #[case("../outside.rs")]
    #[case(r"src\..\..\outside.rs")]
    #[case("C:relative.rs")]
    fn test_invalid_document_paths(#[case] path: &str, #[values(false, true)] invalid_index_without_test: bool) -> Result<()> {
        let valid = index_bytes("caller.rs", vec![])?;
        let invalid = index_bytes(path, vec![])?;
        let (index_original, index_without_test) = if invalid_index_without_test {
            (&valid, &invalid)
        } else {
            (&invalid, &valid)
        };

        let system_under_test = classify(index_original, index_without_test);

        assert!(system_under_test.is_err(), "Invalid document path was accepted: {path}");
        Ok(())
    }
}
