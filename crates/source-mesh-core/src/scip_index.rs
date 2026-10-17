use std::collections::BTreeMap;
use std::collections::BTreeSet;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::ensure;
use getset::Getters;
use getset::MutGetters;
use protobuf::Message as _;
use protobuf::MessageField;
use protobuf::SpecialFields;
use scip::symbol::format_symbol;
use scip::symbol::parse_symbol;
use scip::types::Document;
use scip::types::Index;
use scip::types::Metadata;
use scip::types::Occurrence;
use scip::types::Symbol;
use scip::types::SymbolInformation;

use crate::model::FileNode;
use crate::scip_path::source_path;

#[derive(Getters, MutGetters)]
pub(crate) struct ParsedIndex {
    #[getset(get = "pub(crate)", get_mut = "pub(crate)")]
    metadata: MessageField<Metadata>,
    #[getset(get = "pub(crate)", get_mut = "pub(crate)")]
    documents: Vec<ParsedDocument>,
    external_symbols: Vec<SymbolInformation>,
    special_fields: SpecialFields,
    #[getset(get = "pub(crate)")]
    symbol_cache: SymbolCache,
}

impl ParsedIndex {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(!bytes.is_empty(), "SCIP index is empty");
        let index = Index::parse_from_bytes(bytes).context("Cannot parse SCIP protobuf index")?;
        let Index { metadata, documents, external_symbols, special_fields } = index;
        let documents = documents.into_iter().map(|document| {
            Ok(ParsedDocument { file: source_path(&document.relative_path)?, document })
        });
        let documents = documents.collect::<Result<Vec<_>>>()?;
        let document_symbols = documents.iter().flat_map(|document| {
            let symbols = document.document.symbols.iter().map(|information| information.symbol.as_str());
            let occurrences = document.document.occurrences.iter().map(|occurrence| occurrence.symbol.as_str());
            symbols.chain(occurrences)
        });
        let external_symbol_names = external_symbols.iter().map(|information| information.symbol.as_str());
        let symbol_cache = SymbolCache::parse(document_symbols.chain(external_symbol_names));
        Ok(Self { metadata, documents, external_symbols, special_fields, symbol_cache })
    }

    pub(crate) fn documents_with_symbols_mut(&mut self) -> (&mut [ParsedDocument], &mut SymbolCache) {
        (&mut self.documents, &mut self.symbol_cache)
    }

    pub(crate) fn symbols(&self) -> impl Iterator<Item = &SymbolInformation> {
        let document_symbols = self.documents.iter().flat_map(|document| &document.document.symbols);
        document_symbols.chain(&self.external_symbols)
    }

    #[must_use]
    pub(crate) fn into_index(self) -> Index {
        let documents = self.documents.into_iter().map(|document| document.document).collect();
        Index { metadata: self.metadata, documents, external_symbols: self.external_symbols, special_fields: self.special_fields }
    }
}

#[derive(Getters)]
#[getset(get = "pub(crate)")]
pub(crate) struct ParsedDocument {
    file: FileNode,
    document: Document,
}

impl ParsedDocument {
    pub(crate) const fn occurrences_mut(&mut self) -> &mut Vec<Occurrence> {
        &mut self.document.occurrences
    }

    pub(crate) const fn symbols_mut(&mut self) -> &mut Vec<SymbolInformation> {
        &mut self.document.symbols
    }
}

pub(crate) struct SymbolCache {
    symbols: BTreeMap<String, ParsedSymbol>,
}

impl SymbolCache {
    #[must_use]
    fn parse<'a>(symbol_names: impl Iterator<Item = &'a str>) -> Self {
        let distinct_symbols = symbol_names.collect::<BTreeSet<_>>();
        let symbols = distinct_symbols.into_iter().map(|symbol| (symbol.to_owned(), ParsedSymbol::parse(symbol))).collect();
        Self { symbols }
    }

    #[must_use]
    pub(crate) fn get(&self, symbol: &str) -> &ParsedSymbol {
        self.symbols.get(symbol).unwrap_or_else(|| unreachable!("SCIP symbol must be registered before use: {symbol:?}"))
    }

    pub(crate) fn intern(&mut self, symbol: Symbol) -> String {
        let formatted = format_symbol(symbol);
        /* Formatting can canonicalize fields, so the original structure may not match the emitted symbol. */
        self.symbols.entry(formatted.clone()).or_insert_with(|| ParsedSymbol::parse(&formatted));
        formatted
    }
}

pub(crate) enum ParsedSymbol {
    Structured(Symbol),
    Opaque { decoding_error: String },
}

impl ParsedSymbol {
    #[must_use]
    fn parse(symbol: &str) -> Self {
        match parse_symbol(symbol) {
            Ok(parsed) => Self::Structured(parsed),
            Err(error) => Self::Opaque { decoding_error: format!("Cannot decode symbol {symbol:?}: {error:?}") },
        }
    }

    #[must_use]
    pub(crate) const fn structure(&self) -> Option<&Symbol> {
        match *self {
            Self::Structured(ref symbol) => Some(symbol),
            Self::Opaque { .. } => None,
        }
    }

    pub(crate) fn require_structure(&self) -> Result<&Symbol> {
        match *self {
            Self::Structured(ref symbol) => Ok(symbol),
            Self::Opaque { ref decoding_error } => Err(anyhow!(decoding_error.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::SymbolCache;

    #[rstest]
    #[case::structured("rust-analyzer cargo fixture 0.0.0 run().", true)]
    #[case::opaque("synthetic-method", false)]
    #[case::empty("", false)]
    fn test_distinct_symbol_outcomes(#[case] symbol: &str, #[case] structured: bool) {
        let system_under_test = SymbolCache::parse([symbol, symbol, symbol].into_iter());

        let optional_structure = system_under_test.get(symbol).structure();
        let required_structure = system_under_test.get(symbol).require_structure();
        let repeated_structure = system_under_test.get(symbol).require_structure();

        assert_eq!(system_under_test.symbols.len(), 1, "Repeated raw symbols must share one cached outcome");
        assert_eq!(optional_structure.is_some(), structured);
        assert_eq!(required_structure.is_ok(), structured);
        let required_error = required_structure.err().map(|error| error.to_string());
        let repeated_error = repeated_structure.err().map(|error| error.to_string());
        assert_eq!(required_error, repeated_error, "Decoding failures must retain their diagnostic");
    }

    #[rstest]
    #[case::canonical("rust-analyzer cargo fixture 0.0.0 run().")]
    #[case::escaped_package("rust-analyzer cargo fixture  name 0.0.0 run().")]
    fn test_emitted_symbol_outcome(#[case] symbol: &str) {
        let mut system_under_test = SymbolCache::parse([symbol].into_iter());
        let Some(structure) = system_under_test.get(symbol).structure() else {
            unreachable!("The emitted-symbol fixture must have SCIP structure");
        };
        let structure = structure.clone();

        let emitted = system_under_test.intern(structure);
        let replayed = SymbolCache::parse([emitted.as_str()].into_iter());
        let cached_structure = system_under_test.get(&emitted).require_structure();
        let replayed_structure = replayed.get(&emitted).require_structure();

        assert_eq!(system_under_test.symbols.len(), if emitted == symbol { 1 } else { 2 });
        assert_eq!(cached_structure.as_ref().ok(), replayed_structure.as_ref().ok());
        let cached_error = cached_structure.err().map(|error| error.to_string());
        let replayed_error = replayed_structure.err().map(|error| error.to_string());
        assert_eq!(cached_error, replayed_error);
    }
}
