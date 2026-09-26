use std::collections::HashSet;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use derive_new::new;
use protobuf::Message as _;
use scip::types::Document;
use scip::types::Index;
use scip::types::SymbolRole;
use scip::types::ToolInfo;

use crate::model::FileNode;
use crate::scip_path::source_path;

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
            [start_line, start_column, end_line, end_column] => {
                ((start_line, start_column), (end_line, end_column))
            }
            _ => bail!("SCIP range must contain three or four coordinates: {coordinates:?}"),
        };
        ensure!(coordinates.iter().all(|coordinate| *coordinate >= 0) && start <= end,
            "SCIP range must contain ordered nonnegative positions: {coordinates:?}");
        Ok(Self { start, end })
    }

    #[must_use]
    fn contains(&self, range: &Self) -> bool {
        self.start <= range.start && range.end <= self.end && range.start < range.end
    }
}

fn missing_definition_scopes(
    document: &Document,
    production_definitions: &HashSet<SymbolOccurrence>,
) -> Result<Vec<ScipRange>> {
    let file = source_path(&document.relative_path)?;
    let mut scopes = Vec::new();
    // rust-analyzer can attach a referenced target's body as enclosing_range.
    for occurrence in &document.occurrences {
        if occurrence.symbol_roles & SymbolRole::Definition as i32 == 0
            || occurrence.symbol.is_empty()
            || occurrence.symbol.starts_with("local ")
            || occurrence.enclosing_range.is_empty()
        {
            continue;
        }
        let definition = SymbolOccurrence::new(
            file.clone(), ScipRange::parse(&occurrence.range)?, occurrence.symbol.clone(),
        );
        if production_definitions.contains(&definition) {
            continue;
        }
        let scope = ScipRange::parse(&occurrence.enclosing_range)?;
        if scope != definition.range {
            ensure!(scope.contains(&definition.range),
                "SCIP definition scope does not contain its name in {}", file.path().display());
            scopes.push(scope);
        }
    }
    Ok(scopes)
}

pub(crate) fn mark_test_references(index_bytes: &[u8], production_bytes: &[u8]) -> Result<Vec<u8>> {
    ensure!(!index_bytes.is_empty(), "Full SCIP index is empty");
    ensure!(!production_bytes.is_empty(), "Production SCIP index is empty");
    let mut index = Index::parse_from_bytes(index_bytes).context("Cannot parse full SCIP index")?;
    let production = Index::parse_from_bytes(production_bytes)
        .context("Cannot parse production SCIP index")?;
    ensure!(!index.documents.is_empty(), "Full SCIP index contains no source documents");

    let mut production_references = HashSet::new();
    let mut production_definitions = HashSet::new();
    for document in production.documents {
        let file = source_path(&document.relative_path)?;
        for occurrence in document.occurrences {
            let identity = SymbolOccurrence::new(
                file.clone(), ScipRange::parse(&occurrence.range)?, occurrence.symbol,
            );
            if occurrence.symbol_roles & SymbolRole::Definition as i32 == 0 {
                production_references.insert(identity);
                continue;
            }
            if !identity.symbol.is_empty() && !identity.symbol.starts_with("local ") {
                production_definitions.insert(identity);
            }
        }
    }
    for document in &mut index.documents {
        let file = source_path(&document.relative_path)?;
        let test_scopes = missing_definition_scopes(document, &production_definitions)?;
        for occurrence in &mut document.occurrences {
            if occurrence.symbol_roles & SymbolRole::Definition as i32 != 0 {
                continue;
            }
            let identity = SymbolOccurrence::new(
                file.clone(), ScipRange::parse(&occurrence.range)?, occurrence.symbol.clone(),
            );
            if !production_references.contains(&identity)
                || test_scopes.iter().any(|scope| scope.contains(&identity.range)) {
                occurrence.symbol_roles |= SymbolRole::Test as i32;
            }
        }
    }

    let mut metadata = index.metadata.take().unwrap_or_default();
    metadata.tool_info = Some(ToolInfo {
        name: env!("CARGO_PKG_NAME").into(),
        version: env!("CARGO_PKG_VERSION").into(),
        ..ToolInfo::default()
    }).into();
    index.metadata = Some(metadata).into();
    index.write_to_bytes().context("Cannot serialize classified SCIP index")
}

#[must_use]
pub(crate) fn has_test_classification(index: &Index) -> bool {
    index.metadata.as_ref()
        .and_then(|metadata| metadata.tool_info.as_ref())
        .is_some_and(|tool| tool.name == env!("CARGO_PKG_NAME"))
        || index.documents.iter().flat_map(|document| &document.occurrences)
            .any(|occurrence| occurrence.symbol_roles & SymbolRole::Test as i32 != 0)
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

    use super::has_test_classification;
    use super::mark_test_references;
    use super::ScipRange;

    #[must_use]
    fn reference(symbol: &str, range: [i32; 3]) -> Occurrence {
        Occurrence { symbol: symbol.into(), range: range.into(), ..Occurrence::default() }
    }

    fn index_bytes(path: &str, occurrences: Vec<Occurrence>) -> Result<Vec<u8>> {
        Ok(Index {
            documents: vec![Document {
                relative_path: path.into(), occurrences, ..Document::default()
            }],
            ..Index::default()
        }.write_to_bytes()?)
    }

    #[test]
    fn test_reference_identity_and_definition_roles() -> Result<()> {
        let shared_reference = reference("service/run().", [0, 0, 3]);
        let another_range = reference("service/run().", [1, 0, 3]);
        let another_symbol = reference("mock/run().", [0, 0, 3]);
        let mut definition = reference("service/definition().", [2, 0, 3]);
        definition.symbol_roles = SymbolRole::Definition as i32 | SymbolRole::ReadAccess as i32;
        let mut already_classified = reference("service/retained().", [3, 0, 3]);
        already_classified.symbol_roles = SymbolRole::Test as i32 | SymbolRole::ReadAccess as i32;
        let full_index = index_bytes("src/caller.rs", vec![
            shared_reference.clone(), another_range, another_symbol,
            definition.clone(), already_classified.clone(),
        ])?;
        let production_index = index_bytes("src/caller.rs", vec![
            shared_reference, already_classified,
        ])?;

        let classified_bytes = mark_test_references(&full_index, &production_index)?;
        let system_under_test = Index::parse_from_bytes(&classified_bytes)?;

        let occurrences = &system_under_test.documents.first().context("Source document missing")?.occurrences;
        let roles: Vec<_> = occurrences.iter().map(|occurrence| occurrence.symbol_roles).collect();
        assert_eq!(roles, vec![
            0,
            SymbolRole::Test as i32,
            SymbolRole::Test as i32,
            definition.symbol_roles,
            SymbolRole::Test as i32 | SymbolRole::ReadAccess as i32,
        ], "Reference identity, existing flags, or definitions were classified incorrectly");
        assert!(has_test_classification(&system_under_test), "Classification provenance missing");
        Ok(())
    }

    #[rstest]
    #[case(".\\src\\caller.rs", "src/caller.rs", false)]
    #[case("src/caller.rs", "src/other.rs", true)]
    fn test_document_path_identity(
        #[case] full_path: &str,
        #[case] production_path: &str,
        #[case] expected_test_reference: bool,
    ) -> Result<()> {
        let occurrence = reference("service/run().", [0, 0, 3]);
        let full_index = index_bytes(full_path, vec![occurrence.clone()])?;
        let production_index = index_bytes(production_path, vec![occurrence])?;

        let classified_bytes = mark_test_references(&full_index, &production_index)?;
        let system_under_test = Index::parse_from_bytes(&classified_bytes)?;

        let classified_occurrence = system_under_test.documents.first()
            .and_then(|document| document.occurrences.first()).context("Reference missing")?;
        assert_eq!(classified_occurrence.symbol_roles & SymbolRole::Test as i32 != 0, expected_test_reference,
            "Document paths were matched incorrectly");
        Ok(())
    }

    #[test]
    fn test_production_definitions_do_not_match_references() -> Result<()> {
        let occurrence = reference("service/run().", [0, 0, 3]);
        let full_index = index_bytes("caller.rs", vec![occurrence.clone()])?;
        let mut definition = occurrence;
        definition.symbol_roles = SymbolRole::Definition as i32;
        let production_index = index_bytes("caller.rs", vec![definition])?;

        let classified_bytes = mark_test_references(&full_index, &production_index)?;
        let system_under_test = Index::parse_from_bytes(&classified_bytes)?;

        let classified_occurrence = system_under_test.documents.first()
            .and_then(|document| document.occurrences.first()).context("Reference missing")?;
        assert!(classified_occurrence.symbol_roles & SymbolRole::Test as i32 != 0,
            "Production definition was treated as a matching reference");
        Ok(())
    }

    #[test]
    fn test_empty_production_documents() -> Result<()> {
        let full_index = index_bytes("caller.rs", vec![reference("service/run().", [0, 0, 3])])?;
        let production_index = Index {
            metadata: Some(Metadata::default()).into(), ..Index::default()
        }.write_to_bytes()?;

        let classified_bytes = mark_test_references(&full_index, &production_index)?;
        let system_under_test = Index::parse_from_bytes(&classified_bytes)?;

        let occurrence = system_under_test.documents.first()
            .and_then(|document| document.occurrences.first()).context("Reference missing")?;
        assert!(occurrence.symbol_roles & SymbolRole::Test as i32 != 0,
            "Reference absent from an empty production index was not classified");
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

        let classified_bytes = mark_test_references(&index, &index)?;
        let system_under_test = Index::parse_from_bytes(&classified_bytes)?;

        let metadata = system_under_test.metadata.as_ref().context("Metadata missing")?;
        let original_metadata = original.metadata.as_ref().context("Fixture metadata missing")?;
        assert!(metadata.project_root == original_metadata.project_root
            && metadata.text_document_encoding == original_metadata.text_document_encoding,
            "Source root or encoding was changed");
        assert!(has_test_classification(&system_under_test), "Empty classification provenance missing");
        assert!(!has_test_classification(&original), "Raw index was mistaken for a classified index");
        Ok(())
    }

    #[rstest]
    #[case(0, false)]
    #[case(SymbolRole::Test as i32, true)]
    fn test_existing_test_role_detection(#[case] roles: i32, #[case] expected: bool) -> Result<()> {
        let mut occurrence = reference("service/run().", [0, 0, 3]);
        occurrence.symbol_roles = roles;
        let index = Index::parse_from_bytes(&index_bytes("caller.rs", vec![occurrence])?)?;

        let system_under_test = has_test_classification(&index);

        assert_eq!(system_under_test, expected, "SCIP Test role detection differs");
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
        let full_index = index_bytes("caller.rs", vec![
            scope_definition, before_scope.clone(), inside_scope.clone(), after_scope.clone(),
        ])?;
        let production_index = index_bytes("caller.rs", vec![before_scope, inside_scope, after_scope])?;

        let classified_bytes = mark_test_references(&full_index, &production_index)?;
        let system_under_test = Index::parse_from_bytes(&classified_bytes)?;

        let roles: Vec<_> = system_under_test.documents.first().context("Source document missing")?
            .occurrences.iter().map(|occurrence| occurrence.symbol_roles).collect();
        assert_eq!(roles, vec![SymbolRole::Definition as i32, 0, SymbolRole::Test as i32, 0],
            "Inactive scope references or adjacent production references were classified incorrectly");
        Ok(())
    }

    #[rstest]
    #[case("local 0")]
    #[case("")]
    fn test_unnamed_definitions_do_not_create_scopes(#[case] symbol: &str) -> Result<()> {
        let shared_reference = reference("shared/run().", [7, 0, 3]);
        let mut definition = reference(symbol, [4, 4, 9]);
        definition.symbol_roles = SymbolRole::Definition as i32;
        definition.enclosing_range = vec![3, 0, 12, 1];
        let full_index = index_bytes("caller.rs", vec![definition, shared_reference.clone()])?;
        let production_index = index_bytes("caller.rs", vec![shared_reference])?;

        let classified_bytes = mark_test_references(&full_index, &production_index)?;
        let system_under_test = Index::parse_from_bytes(&classified_bytes)?;

        let classified_reference = system_under_test.documents.first()
            .and_then(|document| document.occurrences.last()).context("Reference missing")?;
        assert_eq!(classified_reference.symbol_roles, 0,
            "An unnamed definition created a test scope");
        Ok(())
    }

    #[test]
    fn test_retained_definitions_do_not_create_scopes() -> Result<()> {
        let shared_reference = reference("shared/run().", [7, 0, 3]);
        let mut definition = reference("client/production/", [4, 4, 9]);
        definition.symbol_roles = SymbolRole::Definition as i32;
        definition.enclosing_range = vec![3, 0, 12, 1];
        let index = index_bytes("caller.rs", vec![definition, shared_reference])?;

        let classified_bytes = mark_test_references(&index, &index)?;
        let system_under_test = Index::parse_from_bytes(&classified_bytes)?;

        let classified_reference = system_under_test.documents.first()
            .and_then(|document| document.occurrences.last()).context("Reference missing")?;
        assert_eq!(classified_reference.symbol_roles, 0,
            "A retained definition created a test scope");
        Ok(())
    }

    #[test]
    fn test_file_module_definitions_do_not_create_scopes() -> Result<()> {
        let shared_reference = reference("shared/run().", [7, 0, 3]);
        let module = Occurrence {
            symbol: "client/".into(), range: vec![0, 0, 13, 0],
            enclosing_range: vec![0, 0, 13, 0], symbol_roles: SymbolRole::Definition as i32,
            ..Occurrence::default()
        };
        let full_index = index_bytes("caller.rs", vec![module, shared_reference.clone()])?;
        let production_index = index_bytes("caller.rs", vec![shared_reference])?;

        let classified_bytes = mark_test_references(&full_index, &production_index)?;
        let system_under_test = Index::parse_from_bytes(&classified_bytes)?;

        let classified_reference = system_under_test.documents.first()
            .and_then(|document| document.occurrences.last()).context("Reference missing")?;
        assert_eq!(classified_reference.symbol_roles, 0,
            "A file-module definition incorrectly classified the entire source file");
        Ok(())
    }

    #[test]
    fn test_reference_target_ranges_do_not_create_scopes() -> Result<()> {
        let mut shared_reference = reference("shared/run().", [7, 0, 3]);
        shared_reference.enclosing_range = vec![3, 0, 12, 1];
        let index = index_bytes("caller.rs", vec![shared_reference])?;

        let classified_bytes = mark_test_references(&index, &index)?;
        let system_under_test = Index::parse_from_bytes(&classified_bytes)?;

        let classified_reference = system_under_test.documents.first()
            .and_then(|document| document.occurrences.first()).context("Reference missing")?;
        assert_eq!(classified_reference.symbol_roles, 0,
            "A reference target's definition range was used as the reference's own scope");
        Ok(())
    }

    #[test]
    fn test_equivalent_reference_range_encodings() -> Result<()> {
        let short_reference = reference("shared/run().", [7, 0, 3]);
        let mut long_reference = short_reference.clone();
        long_reference.range = vec![7, 0, 7, 3];
        let full_index = index_bytes("caller.rs", vec![short_reference])?;
        let production_index = index_bytes("caller.rs", vec![long_reference])?;

        let classified_bytes = mark_test_references(&full_index, &production_index)?;
        let system_under_test = Index::parse_from_bytes(&classified_bytes)?;

        let classified_reference = system_under_test.documents.first()
            .and_then(|document| document.occurrences.first()).context("Reference missing")?;
        assert_eq!(classified_reference.symbol_roles, 0,
            "Equivalent SCIP range encodings did not match");
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

        assert_eq!(system_under_test.contains(&reference_range), expected,
            "Scope containment was incorrect for {coordinates:?}");
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
    fn test_invalid_index_inputs(#[case] full: &[u8], #[case] production: &[u8]) {
        let system_under_test = mark_test_references(full, production);

        assert!(system_under_test.is_err());
    }
}
