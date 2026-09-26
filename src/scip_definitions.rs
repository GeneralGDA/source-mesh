use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashSet;

use anyhow::Result;
use anyhow::anyhow;
use scip::symbol::format_symbol;
use scip::symbol::is_local_symbol;
use scip::symbol::parse_symbol;
use scip::types::Index;
use scip::types::Symbol;
use scip::types::SymbolRole;
use scip::types::descriptor::Suffix;
use scip::types::symbol_information::Kind;

use crate::model::FileNode;
use crate::scip_path::source_path;

pub(crate) fn definitions(index: &Index) -> Result<BTreeMap<&str, BTreeSet<FileNode>>> {
    let namespaces: HashSet<_> = index.documents.iter()
        .flat_map(|document| &document.symbols)
        .chain(&index.external_symbols)
        .filter(|symbol| matches!(symbol.kind.enum_value_or_default(), Kind::Module | Kind::Namespace | Kind::Package))
        .map(|symbol| symbol.symbol.as_str())
        .collect();
    let mut definitions: BTreeMap<&str, BTreeSet<FileNode>> = BTreeMap::new();
    for document in &index.documents {
        let file = source_path(&document.relative_path)?;
        let symbols = document.symbols.iter().map(|information| information.symbol.as_str())
            .chain(document.occurrences.iter()
                .filter(|occurrence| occurrence.symbol_roles & SymbolRole::Definition as i32 != 0)
                .map(|occurrence| occurrence.symbol.as_str()));
        for symbol in symbols {
            if !symbol.is_empty() && !is_local_symbol(symbol) && !namespaces.contains(symbol)
                && !is_inherent_implementation_symbol(symbol)
            {
                definitions.entry(symbol).or_default().insert(file.clone());
            }
        }
    }
    Ok(definitions)
}

pub(crate) fn unresolved_symbols(index: &Index) -> Result<BTreeSet<String>> {
    let definitions = definitions(index)?;
    let packages = definitions.keys().map(|symbol| package_identity(symbol))
        .collect::<Result<BTreeSet<_>>>()?;
    let known_symbols: HashSet<_> = index.documents.iter().flat_map(|document| &document.symbols)
        .chain(&index.external_symbols).map(|information| information.symbol.as_str())
        .chain(definitions.keys().copied()).collect();
    let mut unresolved = BTreeSet::new();
    for symbol in index.documents.iter().flat_map(|document| &document.occurrences)
        .map(|occurrence| occurrence.symbol.as_str())
        .filter(|symbol| !symbol.is_empty() && !is_local_symbol(symbol) && !known_symbols.contains(symbol)
            && !is_inherent_implementation_symbol(symbol))
        .collect::<BTreeSet<_>>()
    {
        let parsed = decode_symbol(symbol)?;
        if parsed.descriptors.last().is_some_and(|descriptor| descriptor.suffix.enum_value_or_default() != Suffix::Namespace)
            && packages.contains(&package_identity(symbol)?)
        {
            unresolved.insert(symbol.to_owned());
        }
    }
    Ok(unresolved)
}

#[must_use]
fn is_inherent_implementation_symbol(symbol: &str) -> bool {
    parse_symbol(symbol).is_ok_and(|parsed| {
        parsed.descriptors.last().is_some_and(|descriptor| descriptor.suffix.enum_value_or_default() == Suffix::TypeParameter)
            && parsed.descriptors.iter().nth_back(1).is_some_and(|descriptor| {
                descriptor.name == "impl" && descriptor.suffix.enum_value_or_default() == Suffix::Type
            })
    })
}

pub(crate) fn decode_symbol(symbol: &str) -> Result<Symbol> {
    parse_symbol(symbol).map_err(|error| anyhow!("Cannot decode symbol {symbol:?}: {error:?}"))
}

fn package_identity(symbol: &str) -> Result<String> {
    let mut parsed = decode_symbol(symbol)?;
    parsed.descriptors.clear();
    Ok(format_symbol(parsed))
}

#[cfg(test)]
#[expect(clippy::panic_in_result_fn, reason = "Tests return Result for fallible setup and use assertions for expectations.")]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use scip::types::Document;
    use scip::types::Index;
    use scip::types::Occurrence;
    use scip::types::SymbolInformation;
    use scip::types::SymbolRole;
    use scip::types::symbol_information::Kind;

    use super::definitions;
    use super::unresolved_symbols;
    use crate::model::FileNode;

    #[rstest]
    #[case::generated_method(Kind::Method, true)]
    #[case::module(Kind::Module, false)]
    #[case::namespace(Kind::Namespace, false)]
    #[case::package(Kind::Package, false)]
    fn test_document_symbol_ownership(#[case] kind: Kind, #[case] dependency_target: bool) -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 model/impl#[Value]get().";
        let fixture = Index { documents: vec![Document {
            relative_path: "model.rs".into(),
            symbols: vec![SymbolInformation { symbol: symbol.into(), kind: kind.into(), ..SymbolInformation::default() }],
            ..Document::default()
        }], ..Index::default() };
        let expected_owner = BTreeSet::from([FileNode::new(PathBuf::from("model.rs"))?]);

        let system_under_test = definitions(&fixture)?;

        assert_eq!(system_under_test.get(symbol), dependency_target.then_some(&expected_owner));
        Ok(())
    }

    #[rstest]
    #[case::missing_method("rust-analyzer cargo fixture 0.0.0 model/impl#[Value]get().", true)]
    #[case::inherent_implementation("rust-analyzer cargo fixture 0.0.0 model/impl#[Value]", false)]
    #[case::known_type("rust-analyzer cargo fixture 0.0.0 model/Value#", false)]
    #[case::external_package("rust-analyzer cargo dependency 0.0.0 Value#", false)]
    #[case::external_version("rust-analyzer cargo fixture 1.0.0 Value#", false)]
    #[case::module("rust-analyzer cargo fixture 0.0.0 model/", false)]
    #[case::local("local 0", false)]
    fn test_missing_project_symbol_selection(#[case] reference: &str, #[case] needs_resolution: bool) -> Result<()> {
        let fixture = Index { documents: vec![Document {
            relative_path: "model.rs".into(),
            symbols: vec![SymbolInformation { symbol: "rust-analyzer cargo fixture 0.0.0 model/Value#".into(), ..SymbolInformation::default() }],
            occurrences: vec![Occurrence { symbol: reference.into(), ..Occurrence::default() }],
            ..Document::default()
        }], ..Index::default() };

        let system_under_test = unresolved_symbols(&fixture)?;

        assert_eq!(system_under_test.contains(reference), needs_resolution);
        Ok(())
    }
    #[rstest]
    #[case::inherent_implementation("rust-analyzer cargo fixture 0.0.0 model/impl#[Value]", false)]
    #[case::generated_method("rust-analyzer cargo fixture 0.0.0 model/impl#[Value]get().", true)]
    #[case::raw_identifier_method("rust-analyzer cargo fixture 0.0.0 model/impl#get().", true)]
    #[case::trait_implementation("rust-analyzer cargo fixture 0.0.0 model/impl#[Value][Trait]", true)]
    #[case::synthetic_symbol("synthetic-method", true)]
    fn test_inherent_implementation_ownership(#[case] symbol: &str, #[case] dependency_target: bool) -> Result<()> {
        let fixture = Index { documents: vec![
            Document {
                relative_path: "first.rs".into(),
                symbols: vec![SymbolInformation { symbol: symbol.into(), ..SymbolInformation::default() }],
                occurrences: vec![Occurrence { symbol: symbol.into(), symbol_roles: SymbolRole::Definition as i32, ..Occurrence::default() }],
                ..Document::default()
            },
            Document {
                relative_path: "second.rs".into(),
                occurrences: vec![Occurrence { symbol: symbol.into(), ..Occurrence::default() }],
                ..Document::default()
            },
        ], ..Index::default() };

        let system_under_test = definitions(&fixture)?;

        assert_eq!(system_under_test.contains_key(symbol), dependency_target);
        Ok(())
    }

    #[test]
    fn test_external_symbol_ownership() -> Result<()> {
        let symbol = "rust-analyzer cargo dependency 0.0.0 model/Value#";
        let fixture = Index {
            documents: vec![Document {
                relative_path: "caller.rs".into(),
                occurrences: vec![Occurrence { symbol: symbol.into(), ..Occurrence::default() }],
                ..Document::default()
            }],
            external_symbols: vec![SymbolInformation { symbol: symbol.into(), ..SymbolInformation::default() }],
            ..Index::default()
        };

        let system_under_test = definitions(&fixture)?;
        let unresolved = unresolved_symbols(&fixture)?;

        assert!(system_under_test.is_empty(), "External metadata must not assign ownership to the caller");
        assert!(unresolved.is_empty(), "External metadata must not require local definition recovery");
        Ok(())
    }
}