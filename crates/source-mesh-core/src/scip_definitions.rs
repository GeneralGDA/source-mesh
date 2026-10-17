use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashSet;

use anyhow::Result;
use scip::symbol::format_symbol;
use scip::symbol::is_local_symbol;
use scip::types::Symbol;
use scip::types::SymbolRole;
use scip::types::descriptor::Suffix;
use scip::types::symbol_information::Kind;

use crate::model::FileNode;
use crate::scip_index::ParsedIndex;

#[derive(Debug, PartialEq)]
pub(crate) enum DefinitionFiles {
    Unique(FileNode),
    Ambiguous(BTreeSet<FileNode>),
}

impl DefinitionFiles {
    fn insert(&mut self, file: &FileNode) {
        match *self {
            Self::Unique(ref current) => {
                if current != file {
                    *self = Self::Ambiguous(BTreeSet::from([current.clone(), file.clone()]));
                }
            }
            Self::Ambiguous(ref mut files) => {
                files.insert(file.clone());
            }
        }
    }

    pub(crate) fn extend(&mut self, definitions: Self) {
        match definitions {
            Self::Unique(file) => self.insert(&file),
            Self::Ambiguous(files) => {
                for file in files {
                    self.insert(&file);
                }
            }
        }
    }

    #[must_use]
    pub(crate) fn contains(&self, file: &FileNode) -> bool {
        match *self {
            Self::Unique(ref current) => current == file,
            Self::Ambiguous(ref files) => files.contains(file),
        }
    }
}

#[must_use]
pub(crate) fn definitions(index: &ParsedIndex) -> BTreeMap<&str, DefinitionFiles> {
    let namespace_symbols = index.symbols().filter(|symbol| {
        matches!(symbol.kind.enum_value_or_default(), Kind::Module | Kind::Namespace | Kind::Package)
    });
    let namespaces: HashSet<_> = namespace_symbols.map(|symbol| symbol.symbol.as_str()).collect();
    let mut definitions: BTreeMap<&str, DefinitionFiles> = BTreeMap::new();
    for document in index.documents() {
        let file = document.file();
        let information_symbols = document.document().symbols.iter().map(|information| information.symbol.as_str());
        let occurrences = document.document().occurrences.iter();
        let definition_occurrences = occurrences.filter(|occurrence| {
            occurrence.symbol_roles & SymbolRole::Definition as i32 != 0
        });
        let symbols = information_symbols.chain(definition_occurrences.map(|occurrence| occurrence.symbol.as_str()));
        for symbol in symbols {
            if symbol.is_empty() || is_local_symbol(symbol) || namespaces.contains(symbol) {
                continue;
            }
            if !index.symbol_cache().get(symbol).structure().is_some_and(is_inherent_implementation_symbol) {
                let definition = definitions.entry(symbol).and_modify(|files| files.insert(file));
                definition.or_insert_with(|| DefinitionFiles::Unique(file.clone()));
            }
        }
    }
    definitions
}

pub(crate) fn unresolved_symbols(index: &ParsedIndex) -> Result<BTreeSet<String>> {
    let definitions = definitions(index);
    let package_identities = definitions.keys().map(|symbol| {
        index.symbol_cache().get(symbol).require_structure().map(package_identity)
    });
    let packages = package_identities.collect::<Result<BTreeSet<_>>>()?;
    let information_symbols = index.symbols().map(|information| information.symbol.as_str());
    let known_symbols: HashSet<_> = information_symbols.chain(definitions.keys().copied()).collect();
    let mut unresolved = BTreeSet::new();
    let occurrences = index.documents().iter().flat_map(|document| &document.document().occurrences);
    let referenced_symbols = occurrences.map(|occurrence| occurrence.symbol.as_str());
    let referenced_symbols = referenced_symbols.filter(|symbol| {
        !symbol.is_empty() && !is_local_symbol(symbol) && !known_symbols.contains(symbol)
    });
    for symbol in referenced_symbols.collect::<BTreeSet<_>>() {
        let parsed = index.symbol_cache().get(symbol).require_structure()?;
        if is_inherent_implementation_symbol(parsed) {
            continue;
        }
        let is_definition = parsed.descriptors.last().is_some_and(|descriptor| {
            descriptor.suffix.enum_value_or_default() != Suffix::Namespace
        });
        if is_definition && packages.contains(&package_identity(parsed)) {
            unresolved.insert(symbol.to_owned());
        }
    }
    Ok(unresolved)
}

#[must_use]
fn is_inherent_implementation_symbol(symbol: &Symbol) -> bool {
    let is_type_parameter = symbol.descriptors.last().is_some_and(|descriptor| {
        descriptor.suffix.enum_value_or_default() == Suffix::TypeParameter
    });
    is_type_parameter && {
        let descriptor = symbol.descriptors.iter().nth_back(1);
        descriptor.is_some_and(|descriptor| {
            descriptor.name == "impl" && descriptor.suffix.enum_value_or_default() == Suffix::Type
        })
    }
}

#[must_use]
fn package_identity(symbol: &Symbol) -> String {
    let mut package = symbol.clone();
    package.descriptors.clear();
    format_symbol(package)
}

#[cfg(test)]
#[expect(clippy::panic_in_result_fn, reason = "Tests return Result for fallible setup and use assertions for expectations.")]
mod tests {
    use std::path::PathBuf;

    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use protobuf::EnumOrUnknown;
    use protobuf::Message as _;
    use rstest::rstest;
    use scip::types::Document;
    use scip::types::Index;
    use scip::types::Occurrence;
    use scip::types::SymbolInformation;
    use scip::types::SymbolRole;
    use scip::types::symbol_information::Kind;

    use super::DefinitionFiles;
    use super::definitions;
    use super::unresolved_symbols;
    use crate::SourcePath;
    use crate::model::FileNode;
    use crate::scip_index::ParsedIndex;

    fn parsed_index(index: &Index) -> Result<ParsedIndex> {
        ParsedIndex::parse(&index.write_to_bytes()?)
    }

    #[rstest]
    #[case::generated_method(Kind::Method, true)]
    #[case::module(Kind::Module, false)]
    #[case::namespace(Kind::Namespace, false)]
    #[case::package(Kind::Package, false)]
    fn test_document_symbol_ownership(#[case] kind: Kind, #[case] dependency_target: bool) -> Result<()> {
        let symbol = "rust-analyzer cargo fixture 0.0.0 model/impl#[Value]get().";
        let fixture = Index {
            documents: vec![Document {
                relative_path: "model.rs".into(),
                symbols: vec![SymbolInformation { symbol: symbol.into(), kind: kind.into(), ..SymbolInformation::default() }],
                ..Document::default()
            }],
            ..Index::default()
        };
        let expected_owner = DefinitionFiles::Unique(FileNode::new(SourcePath::new(PathBuf::from("model.rs"))?));

        let fixture = parsed_index(&fixture)?;

        let system_under_test = definitions(&fixture);

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
        let fixture = Index {
            documents: vec![Document {
                relative_path: "model.rs".into(),
                symbols: vec![SymbolInformation {
                    symbol: "rust-analyzer cargo fixture 0.0.0 model/Value#".into(),
                    ..SymbolInformation::default()
                }],
                occurrences: vec![Occurrence { symbol: reference.into(), ..Occurrence::default() }],
                ..Document::default()
            }],
            ..Index::default()
        };

        let fixture = parsed_index(&fixture)?;

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
        let fixture = Index {
            documents: vec![
                Document {
                    relative_path: "first.rs".into(),
                    symbols: vec![SymbolInformation { symbol: symbol.into(), ..SymbolInformation::default() }],
                    occurrences: vec![Occurrence {
                        symbol: symbol.into(),
                        symbol_roles: SymbolRole::Definition as i32,
                        ..Occurrence::default()
                    }],
                    ..Document::default()
                },
                Document {
                    relative_path: "second.rs".into(),
                    occurrences: vec![Occurrence { symbol: symbol.into(), ..Occurrence::default() }],
                    ..Document::default()
                },
            ],
            ..Index::default()
        };

        let fixture = parsed_index(&fixture)?;

        let system_under_test = definitions(&fixture);

        assert_eq!(system_under_test.contains_key(symbol), dependency_target);
        Ok(())
    }

    #[rstest]
    #[case::structured("rust-analyzer cargo dependency 0.0.0 model/Value#")]
    #[case::opaque("synthetic-external")]
    fn test_external_symbol_ownership(#[case] symbol: &str) -> Result<()> {
        let fixture = Index {
            documents: vec![Document {
                relative_path: "caller.rs".into(),
                occurrences: vec![Occurrence { symbol: symbol.into(), ..Occurrence::default() }],
                ..Document::default()
            }],
            external_symbols: vec![SymbolInformation { symbol: symbol.into(), ..SymbolInformation::default() }],
            ..Index::default()
        };

        let fixture = parsed_index(&fixture)?;

        let system_under_test = definitions(&fixture);
        let unresolved = unresolved_symbols(&fixture)?;

        assert!(system_under_test.is_empty(), "External metadata must not assign ownership to the caller");
        assert!(unresolved.is_empty(), "External metadata must not require local definition recovery");
        Ok(())
    }

    #[rstest]
    #[case(Kind::Method as i32)]
    #[case(i32::MAX)]
    fn test_symbol_kind_fallback(#[case] kind: i32) -> Result<()> {
        let symbol = "synthetic-method";
        let fixture = parsed_index(&Index {
            documents: vec![Document {
                relative_path: "model.rs".into(),
                symbols: vec![SymbolInformation {
                    symbol: symbol.into(),
                    kind: EnumOrUnknown::from_i32(kind),
                    ..SymbolInformation::default()
                }],
                ..Document::default()
            }],
            ..Index::default()
        })?;
        let expected = DefinitionFiles::Unique(FileNode::new(SourcePath::new(PathBuf::from("model.rs"))?));

        let system_under_test = definitions(&fixture);

        assert_eq!(system_under_test.get(symbol), Some(&expected));
        Ok(())
    }

    #[test]
    fn test_unresolved_symbol_decoding() -> Result<()> {
        let fixture = parsed_index(&Index {
            documents: vec![Document {
                relative_path: "model.rs".into(),
                symbols: vec![SymbolInformation {
                    symbol: "rust-analyzer cargo fixture 0.0.0 Value#".into(),
                    ..SymbolInformation::default()
                }],
                occurrences: vec![Occurrence { symbol: "malformed-symbol".into(), ..Occurrence::default() }],
                ..Document::default()
            }],
            ..Index::default()
        })?;

        let system_under_test = unresolved_symbols(&fixture);

        assert!(system_under_test.is_err(), "Resolving an unknown symbol still requires valid SCIP encoding");
        Ok(())
    }

    #[test]
    fn test_opaque_symbol_structure_requirements() -> Result<()> {
        let symbol = "synthetic-method";
        let fixture = parsed_index(&Index {
            documents: vec![Document {
                relative_path: "model.rs".into(),
                symbols: vec![SymbolInformation { symbol: symbol.into(), ..SymbolInformation::default() }],
                ..Document::default()
            }],
            ..Index::default()
        })?;
        let retained_error = fixture.symbol_cache().get(symbol).require_structure().err().map(|error| error.to_string());

        let ownership = definitions(&fixture);
        let system_under_test = unresolved_symbols(&fixture);

        assert!(ownership.contains_key(symbol), "Opaque identifiers must retain definition ownership");
        assert!(system_under_test.is_err(), "Package analysis requires structured SCIP symbols");
        assert_eq!(system_under_test.err().map(|error| error.to_string()), retained_error);
        Ok(())
    }
}
